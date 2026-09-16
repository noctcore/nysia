//! `tasks_list`, served.
//!
//! D-5: tasks are GitHub Issues queried live, and this is where that happens. The project's
//! registered folder is resolved out of the store, `gh` is run in it, and gh's answer is
//! reshaped into the wire type `nysia-proto` owns.
//!
//! # There is no state here, deliberately
//!
//! No cache, no last answer, no table — D-5 forbids a local task domain model, and the
//! absence is load-bearing beyond that. The window's own review found a race where an answer
//! for project A landed after the user had switched to project B, and fixed it on its side by
//! discarding an answer whose project is no longer the active one. **The daemon must not
//! build the other half of that**: a service that remembered "the last issue list" would let
//! two racing calls overwrite each other here, where no caller can see it happen, and the
//! window's check could not help because both answers would be about whatever the shared
//! field last held.
//!
//! So every call is independent, reads nothing it did not fetch, and writes nothing. Two
//! `tasks_list` calls in flight are two spawns with two answers, and whichever the window
//! keeps is the window's decision to make.
//!
//! # The four endings
//!
//! One success and three refusals, and the plan is explicit that collapsing any of them is a
//! lie: *"a user with no issues and a user whose token expired must not see the same
//! screen."* An **empty list is a success** — this repository's v0.1 backlog was cleared, so
//! `[]` is the truthful answer for it today.
//!
//! [`crate::git::gh`] decides which refusal a gh invocation amounted to and why; this module
//! decides what each one says to a person. The two are apart on purpose: one is a fact about
//! another program's exit codes, the other is a sentence somebody reads.
//!
//! # What is in a message, and what is not
//!
//! **No path and no repository name.** A registered folder names a person's disk (traps
//! register #13/#14) and gh's stderr carries repository names and URLs, so neither reaches an
//! envelope: [`crate::git::GhFailure`] has no field to carry gh's text, and the messages below
//! are written here. `PathError` is matched on and replaced rather than rendered, exactly as
//! `rpc::project` does, because its `Display` carries the path.

use std::sync::Arc;

use nysia_proto::{ErrorCode, ErrorEnvelope, Issue, IssueState, ResponsePayload, TasksList};
use serde::Deserialize;

use crate::git::{CanonicalPath, Gh, GhFailure};
use crate::rpc::errors::envelope;
use crate::store::{Store, StoreError};

/// The issue lists this daemon can answer for.
#[derive(Debug)]
pub struct TasksService {
    store: Arc<Store>,
    /// The one resolved `gh`, or the reason there is none.
    ///
    /// Resolved once at bind rather than per verb, which is what [`Gh::locate`] is for: on
    /// Windows the resolution is the difference between an error naming what was looked for
    /// and a bare `NotFound` out of `CreateProcess` (traps register #8).
    ///
    /// **A daemon with no gh still starts**, for the reason `ProjectService` starts without
    /// git: gh is needed by one verb, and refusing to bind without it would stop somebody
    /// opening a shell on a machine that has never had the GitHub CLI — a far larger failure
    /// than the one being reported. The envelope is built once, here, because [`GhFailure`]
    /// is not the thing a caller reads and every call owes the same answer.
    gh: Result<Gh, ErrorEnvelope>,
}

impl TasksService {
    /// Serve issue lists for the projects in `store`.
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        let gh = Gh::locate().map_err(|_| {
            tracing::info!(
                "the GitHub CLI was not found; the Tasks screen will say so when it is asked"
            );
            gh_missing()
        });
        Self { store, gh }
    }

    /// A project's open issues, queried live.
    ///
    /// Blocking: it reads a row, canonicalises a path and spawns `gh`. Every caller runs it
    /// on the blocking pool — [`std::fs::canonicalize`] alone blocks for the OS's own timeout
    /// on a disconnected share, **outside gh's deadline**, and a runtime worker parked there
    /// stalls every session sharing it.
    #[must_use]
    pub fn list(&self, request: &TasksList) -> ResponsePayload {
        match self.listing(request) {
            Ok(issues) => ResponsePayload::TasksList { issues },
            Err(envelope) => ResponsePayload::Error(envelope),
        }
    }

    /// The body of [`TasksService::list`], in the shape `?` can be used in.
    fn listing(&self, request: &TasksList) -> Result<Vec<Issue>, ErrorEnvelope> {
        let gh = self.gh.as_ref().map_err(Clone::clone)?;
        let stored = self
            .store
            .project(&request.project)
            .map_err(|err| store_refusal(&err))?
            .ok_or_else(unknown_project)?;

        // The registered folder, re-resolved. A project whose drive has been unplugged lists
        // in the sidebar perfectly well — that is deliberate, see `ProjectService::list` — so
        // this is the first step that finds out, and it must say which of the two it is
        // rather than blaming GitHub for a disk that is not there.
        let at = CanonicalPath::of(&stored.path).map_err(|_| {
            tracing::warn!(
                project = %stored.id,
                "a registered folder could not be resolved, so its issues cannot be listed"
            );
            envelope(
                ErrorCode::PathUnreadable,
                "that project's folder could not be opened",
                "check the folder is still there and that you can open it",
                &[
                    "a project on a drive that is not plugged in lists but cannot be queried",
                    "`nysia project forget <id>` removes the registration and nothing on disk",
                ],
            )
        })?;

        let answer = gh.issues(&at).map_err(refusal)?;
        let issues = parse(&answer).map_err(|err| {
            // **Not `%err`**, which was the first draft. Measured: `serde_json::Error`'s
            // `Display` does not quote the surrounding input, but it does echo the offending
            // *scalar* — `invalid type: string "<some issue title>", expected u64` is
            // reachable from a malformed answer. That is a response body in a log line, which
            // `rpc::log_file` bans outright, so what is recorded is the classification and
            // the position: a closed-set name and two numbers.
            tracing::warn!(
                project = %stored.id,
                problem = ?err.classify(),
                line = err.line(),
                column = err.column(),
                "gh's issue list could not be read"
            );
            unreadable()
        })?;

        // A list that comes back at exactly the cap is the one answer this verb cannot
        // promise is complete, and gh reports a capped list and a complete one identically —
        // so the count is the only signal there is. Logged rather than refused or flagged on
        // the wire: 500 open issues is a real repository, not a fault, and the screen showing
        // the first 500 of them is the right behaviour. This is what makes it *diagnosable*
        // when somebody asks why an issue they can see on github.com is not in the list.
        if issues.len() as u32 >= crate::git::gh::ISSUE_LIMIT {
            tracing::warn!(
                project = %stored.id,
                limit = crate::git::gh::ISSUE_LIMIT,
                "a project's issue list reached the query limit; it may be short"
            );
        }
        tracing::info!(
            project = %stored.id,
            issues = issues.len(),
            "listed a project's open issues"
        );
        Ok(issues)
    }
}

/// One issue, in the shape `gh issue list --json` answers with.
///
/// Private, and it never crosses the wire: [`Issue`] is what a client reads, and this exists
/// only to be turned into one. Unknown fields are ignored rather than denied — gh sends
/// several this does not ask about, and a new one in a future gh must not fail the verb.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssue {
    number: u64,
    title: String,
    /// `OPEN` or `CLOSED`, which is gh's spelling and not the wire's.
    state: String,
    updated_at: String,
    url: String,
    /// **An object**, which is the field that would have gone wrong quietly.
    ///
    /// `None` where gh sent JSON `null`. See [`GhAuthor`].
    author: Option<GhAuthor>,
    labels: Vec<GhLabel>,
}

/// gh's author object, of which one field survives.
///
/// Measured: `{"id": "U_kgDOERlJTQ", "is_bot": false, "login": "leamcprice20", "name": ""}`.
/// Passing that object through to the window would render **every row with no author and
/// report nothing wrong**, because its reader treats a non-string as *no author* — so the
/// login is lifted out here, which is the whole reason this struct exists.
#[derive(Debug, Deserialize)]
struct GhAuthor {
    /// Absent, or empty, for an issue whose account is gone.
    login: Option<String>,
}

/// gh's label object, of which one field survives.
///
/// gh also sends `id`, `description` and a hex `color`. The colour is the one worth naming:
/// every colour in Nysia is a token and a wire hex is a pixel the theme switcher cannot
/// reach, so it stops here rather than being dropped at the window.
#[derive(Debug, Deserialize)]
struct GhLabel {
    name: Option<String>,
}

/// Turn gh's answer into the rows the wire carries.
///
/// The whole of the reshaping, in one place, so that "what the daemon sends is not what gh
/// said" is a claim with a single site to check.
fn parse(answer: &[u8]) -> Result<Vec<Issue>, serde_json::Error> {
    let rows: Vec<GhIssue> = serde_json::from_slice(answer)?;
    Ok(rows.into_iter().map(issue).collect())
}

/// One gh row as the wire spells it.
fn issue(row: GhIssue) -> Issue {
    Issue {
        number: row.number,
        title: row.title,
        // gh sends `OPEN`; anything that is not open is read as closed rather than refused,
        // which is the same stance the window's reader took while it owned this decision.
        state: if row.state.eq_ignore_ascii_case("open") {
            IssueState::Open
        } else {
            IssueState::Closed
        },
        updated_at: row.updated_at,
        url: row.url,
        // An empty login is `None`, not `Some("")`. GitHub answers this way for a deleted
        // account, and a row whose author renders as an empty string is a row that looks
        // like a rendering bug rather than like an issue nobody owns.
        author: row
            .author
            .and_then(|author| author.login)
            .filter(|login| !login.is_empty()),
        labels: row
            .labels
            .into_iter()
            .filter_map(|label| label.name)
            .filter(|name| !name.is_empty())
            .collect(),
    }
}

/// What a gh refusal says to a person.
///
/// Every [`GhFailure`] gets a sentence here, and the codes underneath them are the ones the
/// Tasks screen branches on — fewer codes than failures, because two states can deserve the
/// same heading and still deserve different words. Every message and every step is written
/// here rather than taken from gh, because gh's text names repositories and URLs (trap 14).
fn refusal(failure: GhFailure) -> ErrorEnvelope {
    match failure {
        GhFailure::NotInstalled => gh_missing(),
        // **Installed, and it did not start.** Deliberately not `gh_missing`: telling
        // somebody who can see gh on their `PATH` to go and install it sends them after what
        // they already have, and leaves the thing that actually failed unnamed.
        //
        // Not retryable, for the reason the two below it are not: the ordinary causes are a
        // binary a policy refuses to launch and an installation part-way through an upgrade,
        // and waiting does not resolve either. A client that thought otherwise would spin.
        GhFailure::CouldNotRun => envelope(
            ErrorCode::QueryFailed,
            "the GitHub CLI is installed and would not start",
            "check that `gh --version` runs in a terminal",
            &[
                "a policy or an antivirus that blocks new processes stops it here",
                "an installation part-way through an upgrade resolves and does not launch",
            ],
        ),
        GhFailure::Unauthenticated => envelope(
            ErrorCode::GhUnauthenticated,
            "the GitHub CLI has no credentials for this repository",
            "run `gh auth login` to sign in",
            &[
                // The sentence the whole classifier exists to be able to say. A user whose
                // token expired is not offline and is not missing a repository, and telling
                // them to check their network is how an expired token goes unfixed for a day.
                "if you signed in before, the token may have expired; sign in again",
                "`gh auth status` shows which account the CLI is using",
            ],
        )
        .with_next_command_args(["gh", "auth", "login"]),
        GhFailure::NoRepository => envelope(
            ErrorCode::QueryFailed,
            "this project has no GitHub repository to list issues from",
            "check the project's folder is a git repository with a GitHub remote",
            &[
                // Deliberately first among the causes: a repository that was never pushed is
                // the ordinary one, not an error the user made.
                "a repository that has never been pushed has no remote to query",
                "`git remote -v` in the project's folder shows what is configured",
            ],
        ),
        GhFailure::QueryFailed => envelope(
            ErrorCode::QueryFailed,
            "GitHub did not answer the issue query",
            "check your network connection and try again",
            &[
                "if you are signed in and online, GitHub may be rate-limiting you",
                "a repository that was renamed or deleted answers the same way",
            ],
        )
        // The one refusal here that a plain retry can clear: a dropped connection and a rate
        // limit both pass. The two above do not — retrying an expired token forever is what
        // `retryable` exists to stop a client doing.
        .retryable(true),
        GhFailure::Unreadable => unreadable(),
    }
}

/// The answer for a machine with no GitHub CLI.
///
/// Built in two places — once at bind and once per refusal — so it is one function rather
/// than one sentence written twice and reworded once.
fn gh_missing() -> ErrorEnvelope {
    envelope(
        ErrorCode::GhMissing,
        "the GitHub CLI is not installed, so issues cannot be listed",
        "install the GitHub CLI from https://cli.github.com",
        &[
            "then run `gh auth login` to sign in",
            "tasks are GitHub Issues queried live, so Nysia has no copy to show instead",
        ],
    )
}

/// The answer when gh replied and this build could not read the reply.
///
/// `query_failed` rather than a fourth code: the window degrades a code it does not
/// recognise to that heading anyway, and this is genuinely a failed query from where a user
/// stands. The log line is where the difference is recorded.
fn unreadable() -> ErrorEnvelope {
    envelope(
        ErrorCode::QueryFailed,
        "the GitHub CLI's answer could not be read",
        "check that `gh` is recent enough to support `gh issue list --json`",
        &[
            "`gh --version` reports the installed version",
            "a very large issue list can also exceed what the daemon will read in one answer",
        ],
    )
}

/// The answer for an id nothing is registered under.
///
/// **Names no id**, unlike `project_forget`'s: that verb is typed by a person who may have
/// mistyped it, and this one is sent by a window from a list it was given, so echoing the id
/// back would put a value nobody typed into a message nobody can act on.
fn unknown_project() -> ErrorEnvelope {
    envelope(
        ErrorCode::UnknownProject,
        "no project is registered under that id",
        "run `nysia project list` to see the ids this daemon holds",
        &["a project that was forgotten keeps its folder and loses its id"],
    )
}

/// The answer when the store itself would not answer.
fn store_refusal(err: &StoreError) -> ErrorEnvelope {
    envelope(
        ErrorCode::Internal,
        // `StoreError`'s variants name an action and never a row, so this is safe to render
        // — the same judgement `rpc::errors` makes for the status verbs.
        err.to_string(),
        "retry the verb",
        &["check that the daemon's runtime directory is writable and not full"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gh's real answer shape, including the fields this module does not ask for.
    const GH_ANSWER: &str = r#"[
      {
        "author": {"id": "U_kgDOERlJTQ", "is_bot": false, "login": "leamcprice20", "name": ""},
        "labels": [
          {"id": "LA_1", "name": "bug", "description": "broken", "color": "d73a4a"},
          {"id": "LA_2", "name": "area:daemon", "description": "", "color": "0e8a16"}
        ],
        "number": 200,
        "state": "OPEN",
        "title": "The issue list, served from the daemon",
        "updatedAt": "2026-09-16T09:12:44Z",
        "url": "https://github.com/Shironex/nysia/issues/200"
      }
    ]"#;

    #[test]
    fn ghs_author_object_becomes_a_login_rather_than_no_author() {
        // **The quiet wrong answer this reshaping exists to prevent.** gh answers with an
        // object; the window's reader treats a non-string author as *no author* and reports
        // nothing wrong — so forwarding gh's shape would render every row without a name and
        // look like a repository of anonymous issues rather than like a protocol mistake.
        let issues = parse(GH_ANSWER.as_bytes()).expect("gh's measured answer parses");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].author.as_deref(), Some("leamcprice20"));
    }

    #[test]
    fn ghs_label_objects_become_names_and_lose_their_colour() {
        let issues = parse(GH_ANSWER.as_bytes()).expect("gh's measured answer parses");
        assert_eq!(issues[0].labels, ["bug", "area:daemon"]);
    }

    #[test]
    fn ghs_uppercase_state_becomes_the_wires_enum() {
        // gh sends `OPEN`. Deciding this once in the daemon is why the wire has an enum.
        let issues = parse(GH_ANSWER.as_bytes()).expect("gh's measured answer parses");
        assert_eq!(issues[0].state, IssueState::Open);
        assert_eq!(issues[0].number, 200);
        assert_eq!(issues[0].updated_at, "2026-09-16T09:12:44Z");
    }

    #[test]
    fn an_issue_with_no_author_survives_as_a_row_without_one() {
        // GitHub really does answer this way for a deleted account, by two spellings: a null
        // author object, and an object whose login is empty. Both must produce a row — losing
        // the issue entirely, or rendering an author named "", would each be worse than
        // saying nobody owns it.
        let answers = [
            r#"[{"author": null, "labels": [], "number": 7, "state": "OPEN",
                 "title": "t", "updatedAt": "2026-01-01T00:00:00Z", "url": "u"}]"#,
            r#"[{"author": {"login": ""}, "labels": [], "number": 7, "state": "OPEN",
                 "title": "t", "updatedAt": "2026-01-01T00:00:00Z", "url": "u"}]"#,
        ];
        for answer in answers {
            let issues = parse(answer.as_bytes()).expect("an authorless issue parses");
            assert_eq!(issues.len(), 1, "the row must survive: {answer}");
            assert_eq!(issues[0].author, None, "{answer}");
        }
    }

    #[test]
    fn an_empty_list_parses_as_an_empty_list_and_not_as_a_failure() {
        // The state the plan says must be reachable and must not be a placeholder. `[]` on
        // exit 0 is the truthful answer for this repository today, and it has to come back as
        // a success or the screen cannot tell "no issues" from "no credentials".
        assert_eq!(parse(b"[]").expect("an empty answer parses"), Vec::new());
    }

    #[test]
    fn an_answer_that_is_not_a_list_of_issues_is_refused_rather_than_guessed_at() {
        // A short read, a gh too old for `--json`, or an answer cut at the output cap. Each
        // is reported rather than salvaged: half a list looks exactly like a repository with
        // fewer issues than it has.
        for broken in [
            &b"[{\"number\": 1"[..],
            &b"{\"issues\": []}"[..],
            &b"not json at all"[..],
        ] {
            assert!(
                parse(broken).is_err(),
                "{:?}",
                String::from_utf8_lossy(broken)
            );
        }
    }

    #[test]
    fn unknown_fields_in_a_future_gh_do_not_fail_the_verb() {
        // gh already sends `id`, `is_bot`, `name`, `description` and `color` that this module
        // ignores, and it will send more. Denying unknown fields would turn a gh release into
        // a Tasks screen that stops working.
        let answer = r#"[{"number": 1, "title": "t", "state": "OPEN",
            "updatedAt": "2026-01-01T00:00:00Z", "url": "u", "author": null, "labels": [],
            "somethingGhAddedLater": {"nested": true}}]"#;
        assert_eq!(
            parse(answer.as_bytes())
                .expect("an unknown field is ignored")
                .len(),
            1
        );
    }

    #[test]
    fn the_three_refusals_carry_the_three_codes_the_screen_branches_on() {
        // The window keys a distinct heading off each of these, and degrades anything it does
        // not recognise to `query_failed`. A rename here is a heading silently lost, so the
        // codes are asserted rather than assumed.
        assert_eq!(
            refusal(GhFailure::NotInstalled).code(),
            &ErrorCode::GhMissing
        );
        assert_eq!(
            refusal(GhFailure::Unauthenticated).code(),
            &ErrorCode::GhUnauthenticated
        );
        for failure in [
            GhFailure::CouldNotRun,
            GhFailure::NoRepository,
            GhFailure::QueryFailed,
            GhFailure::Unreadable,
        ] {
            assert_eq!(refusal(failure).code(), &ErrorCode::QueryFailed);
        }
    }

    #[test]
    fn a_gh_that_will_not_start_is_not_reported_as_a_gh_that_is_absent() {
        // The two arrive by different roads — one from resolution, one from the spawn — and
        // collapsing them tells somebody who can see `gh` on their `PATH` to go and install
        // it. That is advice they will follow, find nothing to do, and be no further on.
        let absent = refusal(GhFailure::NotInstalled);
        let stalled = refusal(GhFailure::CouldNotRun);

        assert_eq!(absent.code(), &ErrorCode::GhMissing);
        assert_ne!(
            stalled.code(),
            &ErrorCode::GhMissing,
            "a gh that is installed must not be reported under the missing-gh heading"
        );
        assert!(
            !stalled.message().contains("not installed"),
            "{}",
            stalled.message()
        );
        // And it does not send them to cli.github.com, which is the step that would waste
        // the trip.
        assert!(
            !stalled
                .next_steps()
                .iter()
                .any(|step| step.contains("cli.github.com")),
            "{:?}",
            stalled.next_steps()
        );
    }

    #[test]
    fn an_expired_token_is_the_one_refusal_that_says_so() {
        // The plan's requirement, at the layer a person reads. It is not enough that the code
        // differs: the sentence underneath is what tells somebody to sign in again rather
        // than to check their wifi, and `gh_unauthenticated` is the only heading that can.
        let expired = refusal(GhFailure::Unauthenticated);
        assert!(
            expired
                .next_steps()
                .iter()
                .any(|step| step.contains("expired")),
            "got {:?}",
            expired.next_steps()
        );
        assert_eq!(
            expired.next_command_args(),
            Some(&["gh".to_owned(), "auth".to_owned(), "login".to_owned()][..])
        );
        // And it is **not** retryable: waiting never fixes a rejected token, and a client that
        // thought otherwise would reconnect forever against something no amount of waiting
        // resolves.
        assert!(!expired.is_retryable());
        assert!(
            refusal(GhFailure::QueryFailed).is_retryable(),
            "a dropped connection is exactly what a retry is for"
        );
    }

    #[test]
    fn no_refusal_names_a_path_or_a_repository() {
        // Trap 14, asserted rather than trusted. `GhFailure` has no field to carry gh's text,
        // so the only way a path could reach a message is somebody writing one into this
        // module — which is what this test is here to catch.
        for failure in [
            GhFailure::NotInstalled,
            GhFailure::CouldNotRun,
            GhFailure::Unauthenticated,
            GhFailure::NoRepository,
            GhFailure::QueryFailed,
            GhFailure::Unreadable,
        ] {
            let envelope = refusal(failure);
            let text = format!("{} {}", envelope.message(), envelope.next_steps().join(" "));
            for shape in ['\\', ':'] {
                // `https://cli.github.com` is the one legitimate colon, and it is in the
                // `gh_missing` steps rather than in any message built from a folder.
                let suspicious = text
                    .split_whitespace()
                    .filter(|word| word.contains(shape))
                    .filter(|word| !word.starts_with("https://"))
                    .collect::<Vec<_>>();
                assert!(
                    suspicious.is_empty(),
                    "a refusal looks like it carries a path: {suspicious:?}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_project_is_refused_without_repeating_the_id_back() {
        // Unlike `project_forget`, which a person types: this id comes from a list the window
        // was given, so echoing it would put a value nobody typed into a message nobody can
        // act on — and an id is one more thing in a log line that names somebody's machine.
        let refused = unknown_project();
        assert_eq!(refused.code(), &ErrorCode::UnknownProject);
        assert!(
            !refused.message().contains("proj_"),
            "{}",
            refused.message()
        );
    }
}
