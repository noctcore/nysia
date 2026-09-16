//! `gh`, behind the same chokepoint as git and under a policy of its own.
//!
//! D-5 makes tasks GitHub Issues queried live, with no local task model, so the daemon's
//! answer to "what is on this project's list" is a `gh` spawn. gh is not git, but the four
//! concerns D-15 names are the same ones — argument construction, the working directory,
//! timeouts and the credential environment — so it runs through [`super::runner`] rather than
//! through a spawner of its own. What differs is everything in this file.
//!
//! # The working directory is how gh learns the repository
//!
//! gh resolves `owner/repo` from the git remote in its own working directory. That is exactly
//! the chokepoint's rule — **a path is a working directory, never an argument** — so there is
//! no `-R` to construct, no path to quote, and no way for a folder named like a flag to
//! become one. It also means a project folder with no GitHub remote is a state of its own,
//! and not "no issues": see [`GhFailure::NoRepository`].
//!
//! # The three failure states, and why the obvious mapping is wrong
//!
//! The window draws three different headings and the plan forbids collapsing them: *"a user
//! with no issues and a user whose token expired must not see the same screen."* Measured
//! against gh 2.97.0 on Windows, which is where the trap is:
//!
//! | situation | exit | stderr |
//! |---|---|---|
//! | issues listed, none open | 0 | — (stdout `[]`) |
//! | **no authentication configured at all** | **4** | `To get started with GitHub CLI, please run: gh auth login` |
//! | **token present but rejected** | **1** | `HTTP 401: Bad credentials` |
//! | cwd is not a git repository | 1 | `failed to run git: fatal: not a git repository` |
//! | the repository has no remotes | 1 | `no git remotes found` |
//! | no remote points at a GitHub host | 1 | `none of the git remotes ... point to a known GitHub host` |
//! | no such repository | 1 | `GraphQL: Could not resolve to a Repository ...` |
//! | host unreachable / offline | 1 | `Post "https://...": dial tcp ...` |
//!
//! **Exit 4 does not mean "not authenticated". It means *never* authenticated.** The case the
//! plan names explicitly — a user whose token expired — exits **1**, indistinguishable by
//! exit code from being offline or from a typo'd repository. So mapping 4 to
//! `gh_unauthenticated` and collapsing 1 into `query_failed` puts an expired token under the
//! wrong heading, which is the lie this module exists to avoid.
//!
//! ## So the 401 is matched as a string, deliberately
//!
//! [`classify`] reads gh's stderr for `HTTP 401`. That is a string match on another program's
//! output and it is worth being plain about what it costs: **if gh rewords that line, an
//! expired token silently becomes `query_failed`** — one heading less specific, carrying gh's
//! own sentence, rather than a wrong one. `an_expired_token_is_told_apart_from_being_offline`
//! pins the measured text so the day it changes is a red test rather than a quiet
//! regression.
//!
//! The alternative was `gh auth status`, one extra spawn on the failure path, and it was
//! **measured and rejected**. Against an unreachable host it exits non-zero and reports
//! *"The token in GH_TOKEN is invalid"* — so it calls being offline a credential problem, and
//! using it would put offline under *"GitHub CLI is not signed in"*. That is the same lie
//! reached by a different road. A 401 cannot make that mistake: it is an HTTP status from a
//! response, so it exists only when GitHub was actually reached and answered.
//!
//! **403 is deliberately not matched.** It is mostly a rate limit, which is a `query_failed`
//! and retryable; treating it as "not signed in" would tell a rate-limited user to log in
//! again, which does not help and loses the sentence that would have.
//!
//! # What reaches the user, and what reaches the log
//!
//! Trap 14: no error variant echoes a payload, and a repository path names a person's disk.
//! gh's stderr carries repository names and URLs — `https://api.github.com/graphql` is in the
//! 401 line — so **it never goes into an error envelope.** [`GhFailure`] carries no gh text
//! at all; the daemon words each state itself. The stderr is logged at `warn`, bounded, which
//! is where v0.2's logging work already confines it, and this does not widen that.

use std::ffi::OsString;
use std::time::Duration;

use super::path::CanonicalPath;
use super::runner::{EnvPolicy, Finished, RunError, Runner, trim_stderr};
use crate::pty::ResolveError;

/// How long a `gh issue list` gets before it is killed.
///
/// **Not [`super::DEFAULT_TIMEOUT`]**, and the difference is the point: git's 10 seconds is
/// generous for a local `rev-parse` that answers in milliseconds, while this is a GraphQL
/// round trip to GitHub across whatever network the user is on. Measured at roughly one
/// second against `cli/cli` on a healthy connection, so this is fifteen times the observed
/// cost rather than a number picked to feel safe.
///
/// It is bounded rather than generous for a reason worth naming: the window serves its
/// control connection with a single worker draining a queue **in order**, so every second
/// spent here is a second `terminal_send` waits behind. That is the same tension
/// `project_start` resolved the same way — a person pressed a button and is watching, so the
/// verb keeps a real deadline rather than `project_list`'s two-second one, which exists
/// because nobody asked for the sidebar to redraw itself.
///
/// One spawn per call, so unlike `project_list` there is no fan-out to bound: the absolute
/// deadline and the per-item one are the same thing here, and the reason that distinction
/// mattered there does not arise.
pub const ISSUE_TIMEOUT: Duration = Duration::from_secs(15);

/// The most issues one query asks for.
///
/// **`gh issue list` defaults to 30 and says nothing about it**, which is a silent truncation
/// in a screen whose whole job is to be honest about what it is showing. Stating it is what
/// makes the number a decision rather than an accident.
///
/// Five hundred is far more than a person triages in a sitting and far less than the output
/// cap. **Measured rather than estimated**: a full 500-row answer for `cli/cli`, a repository
/// with a large open backlog, is 127,639 bytes — around 125 KiB against a cap of 8 MiB, which
/// is 64 times the headroom. So a complete answer cannot reach the truncation path, and
/// [`Finished::truncated`](super::runner::Finished::truncated) firing here would mean
/// something other than a long list.
///
/// Beyond this the list is genuinely short, which is the one dishonesty left in the verb. It
/// is bounded, named here, and `rpc::tasks` logs when an answer comes back at exactly this
/// many rows — the only signal available, since gh reports a capped list and a complete one
/// identically.
pub const ISSUE_LIMIT: u32 = 500;

/// The fields asked for, which is exactly what the Tasks screen draws.
///
/// **The body is deliberately absent.** Nothing renders it, it is someone else's text (traps
/// register #13/#14), and a field that is not on the wire cannot be logged by accident later.
/// Asking for less is the cheapest confinement available.
const ISSUE_FIELDS: &str = "number,title,state,updatedAt,url,author,labels";

/// The variables gh authenticates and finds its configuration through.
///
/// **Scrubbing any of these is a compile error**, enforced by [`SCRUB_IS_CREDENTIAL_FREE`]
/// below. That is the guarantee this list exists for, and it is not hypothetical: `GH_TOKEN`
/// and `GITHUB_TOKEN` are how a developer authenticates in the first place, and the day a
/// scrub list grows toward "anything that names a credential" is the day every machine
/// running Nysia becomes permanently unauthenticated — with the *Tasks* screen reporting that
/// nobody is signed in on a machine where somebody plainly is.
///
/// `GH_CONFIG_DIR` and the three variables it falls back to are here for the same reason one
/// step removed: gh's stored credentials live in that directory, so removing the variable
/// that locates it unauthenticates a keyring user just as thoroughly as removing the token
/// would. `GH_HOST` is here because an enterprise user's credentials are keyed by host, and
/// losing it sends the query to github.com with the wrong ones.
pub const GH_CREDENTIAL_VARS: &[&str] = &[
    // The tokens themselves, in gh's own order of precedence.
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    // Which host the credentials are for.
    "GH_HOST",
    // Where the stored credentials live, and the three variables gh derives that from when
    // it is unset — `$XDG_CONFIG_HOME/gh`, `$AppData/GitHub CLI`, `$HOME/.config/gh`.
    "GH_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "AppData",
    "HOME",
];

/// The environment variables removed before every gh invocation.
///
/// Four groups, and none of them is a credential — see [`GH_CREDENTIAL_VARS`], which the
/// compiler holds this list away from.
///
/// 1. **Which repository gh operates on.** `GH_REPO` is gh's `GIT_DIR`: it overrides the
///    repository for commands that would otherwise read the local one, so with it set the
///    working directory stops being the authority and a project would answer with somebody
///    else's issues. The `GIT_*` group is here for the same reason one layer down — gh
///    *shells out to git* to read the remote, which is visible in its own error text
///    (`failed to run git: …`), so a `GIT_DIR` in the daemon's environment redirects the
///    lookup before gh ever sees it.
/// 2. **What the output looks like.** `GH_FORCE_TTY` and `CLICOLOR_FORCE` make gh render as
///    though it were writing to a terminal **even when its output is redirected**, which puts
///    ANSI escapes into the stream this module parses as JSON. These are not hygiene: they
///    are the two variables that would corrupt the answer rather than merely change it.
/// 3. **Programs gh runs**, and `GH_PATH`, which is sharper than it looks — it tells gh where
///    its own executable is, and the resolution this chokepoint does up front is worth
///    nothing if the child then re-points itself.
/// 4. **Loader overrides**, which inject a library before the child's `main` runs. Program
///    execution by another route, and the same group git's list ends with.
///
/// `GH_DEBUG` and `DEBUG` are in group 2 rather than being ignored: both make gh write
/// verbose tracing to **stderr**, which is the stream [`classify`] reads to tell an expired
/// token from an unreachable host.
pub const GH_SCRUBBED_VARS: &[&str] = &[
    // 1. Which repository the answer is about.
    "GH_REPO",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    // 2. What the output looks like, and what else is written to the stream that is read.
    "GH_FORCE_TTY",
    "CLICOLOR_FORCE",
    "GH_DEBUG",
    "DEBUG",
    // 3. Programs gh runs, and where gh finds itself.
    "GH_PATH",
    "GH_PAGER",
    "PAGER",
    "GH_BROWSER",
    "BROWSER",
    "GH_EDITOR",
    "GIT_EDITOR",
    "VISUAL",
    "EDITOR",
    // 4. Libraries loaded into the child before it runs.
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
];

/// The variables set on every gh invocation.
///
/// **`GIT_TERMINAL_PROMPT=0` does nothing here** — that is git's switch, and reaching for it
/// would be the quiet kind of mistake, a line that looks like the hardening git has and is
/// inert. gh's own switch is `GH_PROMPT_DISABLED`.
///
/// `stdin` is already `/dev/null`, so a prompt cannot wait for an answer nobody will give;
/// what this adds is that a prompt is never *started*. A prompt that dies on EOF still costs
/// the round trip and still writes something to the stderr [`classify`] reads.
///
/// The update notifier is off because it is a second network call on the latency path of a
/// verb that already makes one, and it writes to stderr when it fires.
pub const GH_FORCED_VARS: &[(&str, &str)] = &[
    // gh's own prompt switch. Not GIT_TERMINAL_PROMPT, which gh has never heard of.
    ("GH_PROMPT_DISABLED", "1"),
    // No release check on the latency path, and nothing extra on stderr.
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1"),
    // Belt and braces with the `*_FORCE` scrubs above: no escapes in what is parsed.
    ("NO_COLOR", "1"),
    ("CLICOLOR", "0"),
];

/// gh's environment, as the one policy every gh spawn runs under.
const GH_ENV: EnvPolicy = EnvPolicy {
    scrubbed: GH_SCRUBBED_VARS,
    forced: GH_FORCED_VARS,
};

/// Whether [`GH_SCRUBBED_VARS`] or [`GH_FORCED_VARS`] touches a credential.
///
/// The check behind [`SCRUB_IS_CREDENTIAL_FREE`]. Forcing is checked as well as scrubbing,
/// because `("GH_TOKEN", "")` unauthenticates a machine exactly as thoroughly as removing the
/// variable does, and a list of pairs is the easier of the two to add it to by accident.
const fn touches_a_credential() -> bool {
    let mut credential = 0;
    while credential < GH_CREDENTIAL_VARS.len() {
        let name = GH_CREDENTIAL_VARS[credential];

        let mut scrubbed = 0;
        while scrubbed < GH_SCRUBBED_VARS.len() {
            if same_name(GH_SCRUBBED_VARS[scrubbed], name) {
                return true;
            }
            scrubbed += 1;
        }

        let mut forced = 0;
        while forced < GH_FORCED_VARS.len() {
            if same_name(GH_FORCED_VARS[forced].0, name) {
                return true;
            }
            forced += 1;
        }

        credential += 1;
    }
    false
}

/// Byte equality, in a `const fn`, because `str::eq` is not one.
const fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// gh's environment policy may not touch a credential, checked when this crate is compiled.
///
/// **This is the "impossible by construction" the split was made for.** A test would catch
/// the same mistake, but only when somebody ran it; this refuses to build. Adding `GH_TOKEN`
/// to [`GH_SCRUBBED_VARS`] — or `("GH_TOKEN", "")` to [`GH_FORCED_VARS`] — stops the compiler
/// here with the message below rather than shipping a daemon that reports every user as
/// signed out.
///
/// It covers gh's **effective** set rather than a list someone remembered to check, because
/// [`GH_ENV`] is built from exactly these two constants and [`super::runner`] adds nothing of
/// its own to them.
const SCRUB_IS_CREDENTIAL_FREE: () = assert!(
    !touches_a_credential(),
    "gh's environment policy removes or overrides a variable gh authenticates with; \
     see GH_CREDENTIAL_VARS. Scrubbing one of these does not harden gh, it makes every \
     machine running Nysia permanently unauthenticated."
);

/// Force the assertion above to be evaluated.
///
/// An unused associated constant is not necessarily monomorphised, so the check is bound to
/// a `const` that this module's own code path reads.
const _: () = SCRUB_IS_CREDENTIAL_FREE;

/// Why a `gh` query did not produce a list of issues.
///
/// **Carries none of gh's own text**, which is the whole of trap 14 here: gh's stderr names
/// repositories and URLs, and this type is what a refusal is built from. Each variant is a
/// state the daemon has its own words for; the measured text that produced it goes to the log
/// and no further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhFailure {
    /// `gh` is not installed, or not on this process's `PATH`.
    ///
    /// Reported before any spawn, so it names gh rather than arriving as a bare `NotFound`
    /// out of `CreateProcess` (traps register #8).
    NotInstalled,
    /// Nobody has ever authenticated, or the token that exists was rejected.
    ///
    /// The two are one state to a user — *sign in* is the answer to both — and they arrive
    /// by two different roads: exit 4 for the first, and a `HTTP 401` on stderr for the
    /// second. Telling them apart further would be a heading nobody needs.
    Unauthenticated,
    /// The folder is not a repository gh can ask GitHub about.
    ///
    /// Its own state because *"this project has no issues"* is a different sentence from
    /// *"this project is not on GitHub"*, and a screen that said the first about a folder
    /// with no remote would be lying in the most ordinary case there is — a local repository
    /// that was never pushed.
    NoRepository,
    /// gh ran and the query did not come back.
    ///
    /// Offline, rate-limited, no such repository, a host that refused the connection. One
    /// state on purpose: the daemon's sentence is what distinguishes these, and a heading per
    /// network condition would be four words guessing at a sentence that is already there.
    QueryFailed,
    /// gh answered, and not with the shape this module reads.
    ///
    /// Kept apart from [`GhFailure::QueryFailed`] because it is *this build's* problem rather
    /// than the user's or the network's: a gh too old for `--json`, or an answer cut at the
    /// output cap. The user still sees a failed query; the log says which.
    Unreadable,
}

/// A resolved `gh`, and the deadline every query through it carries.
#[derive(Debug, Clone)]
pub struct Gh {
    runner: Runner,
}

impl Gh {
    /// Find `gh` on `PATH` and pre-validate it.
    ///
    /// Resolved once, up front, for [`super::Git::locate`]'s reason: on Windows this is the
    /// difference between an error naming what was looked for and a bare `NotFound` from a
    /// spawn deep in a call stack (traps register #8).
    ///
    /// # Errors
    ///
    /// [`GhFailure::NotInstalled`] when nothing on `PATH` matched, which is the ordinary
    /// answer on a machine that has never had the GitHub CLI installed.
    pub fn locate() -> Result<Self, GhFailure> {
        Self::located_as("gh")
    }

    /// Find a program by name, so that a test can ask what happens when gh is missing.
    ///
    /// Not public: the only production spelling is `gh`, and a caller that could choose the
    /// program would be a second chokepoint.
    pub(crate) fn located_as(program: &str) -> Result<Self, GhFailure> {
        Runner::locate(program, GH_ENV, ISSUE_TIMEOUT)
            .map(|runner| Self { runner })
            .map_err(|err: ResolveError| {
                tracing::debug!(%err, "gh could not be resolved");
                GhFailure::NotInstalled
            })
    }

    /// The open issues of the repository whose remote lives in `at`.
    ///
    /// The answer is gh's raw stdout, left for the caller to deserialise: this module owns
    /// running gh and reading how it ended, and `nysia-proto` owns what an issue is (D-13).
    ///
    /// Blocking. Every caller runs it on the blocking pool.
    ///
    /// # Errors
    ///
    /// See [`GhFailure`]. A non-zero exit is classified rather than reported, because the
    /// difference between a missing credential and an unreachable host is the whole point of
    /// the verb.
    pub fn issues(&self, at: &CanonicalPath) -> Result<Vec<u8>, GhFailure> {
        let finished = self.runner.run(&issue_argv(), at).map_err(|err| {
            match err {
                // A `gh` that resolved to a batch shim. No gh installation ships one, and
                // letting it through silently would be the opposite of a chokepoint.
                RunError::Argv(err) => tracing::warn!(%err, "gh refused its argument vector"),
                RunError::Empty => tracing::warn!("gh was given an empty argument vector"),
                RunError::Spawn(err) => tracing::warn!(%err, "gh could not be started"),
            }
            GhFailure::NotInstalled
        })?;

        match classify(&finished) {
            Some(failure) => {
                // **The one place gh's own words are allowed to go**, and it is a log line
                // rather than an envelope. gh's stderr carries repository names and URLs
                // (trap 14), so it is bounded here and never travels to a caller.
                tracing::warn!(
                    exit = finished.code.unwrap_or(-1),
                    truncated = finished.truncated,
                    stderr = %trim_stderr(&finished.stderr),
                    "a gh issue query did not return a list"
                );
                Err(failure)
            }
            None => Ok(finished.stdout),
        }
    }

    /// The deadline each query through this handle carries.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.runner.timeout()
    }
}

/// The argument vector for the issue query.
///
/// Every element is a literal written in this crate, and there is nothing caller-supplied in
/// it at all — no repository, no filter, no query string. v0.3's screen passes a project and
/// nothing else, and the working directory carries that. So unlike git's builder there is no
/// `--` to place and no operand to protect: the vector has no place for one.
fn issue_argv() -> Vec<OsString> {
    [
        "issue".to_owned(),
        "list".to_owned(),
        // Open only. The screen draws a state pill because a closed issue can arrive from a
        // future filter, not because this asks for one.
        "--state".to_owned(),
        "open".to_owned(),
        // Stated rather than defaulted: gh's own default is 30 and it is silent about it.
        "--limit".to_owned(),
        ISSUE_LIMIT.to_string(),
        "--json".to_owned(),
        ISSUE_FIELDS.to_owned(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// gh's exit code when nothing has ever been authenticated.
///
/// Its own name because the number is the trap: 4 is *"never authenticated"*, and the case
/// the plan cares about — a token that has expired — is not this. See the module
/// documentation.
const NEVER_AUTHENTICATED_EXIT: i32 = 4;

/// What an ending amounts to, or `None` when gh answered the question.
///
/// A pure function of the ending so that every state in the module's table is a unit test
/// with the measured stderr as its fixture, and none of them needs a network, a token, or a
/// GitHub account to run.
fn classify(finished: &Finished) -> Option<GhFailure> {
    if finished.timed_out {
        return Some(GhFailure::QueryFailed);
    }
    // Before the exit code: gh exits **zero** on a truncated answer, having written more than
    // the cap and reported nothing wrong. Read as success the JSON would stop mid-array, and
    // the caller would report a parse failure — true, but about the wrong thing.
    if finished.truncated {
        return Some(GhFailure::Unreadable);
    }

    let stderr = String::from_utf8_lossy(&finished.stderr);
    match finished.code {
        Some(0) => None,
        // Never authenticated. Unambiguous, and the only state with an exit code of its own.
        Some(NEVER_AUTHENTICATED_EXIT) => Some(GhFailure::Unauthenticated),
        // Everything else is exit 1, which gh uses for every failure it has — so the stderr
        // is the only thing that distinguishes them.
        _ => Some(classify_stderr(&stderr)),
    }
}

/// Which failure gh's stderr describes.
///
/// Split out so the string matching is in one place, visible, and testable without a process.
/// See the module documentation for why a string match is the right instrument here and what
/// it costs when gh rewords a line: one heading less specific, never a wrong one.
fn classify_stderr(stderr: &str) -> GhFailure {
    // **A token that was rejected.** The measured line is
    // `HTTP 401: Bad credentials (https://api.github.com/graphql)`. An HTTP status can only
    // come from a response, so this cannot fire when the host was never reached — which is
    // exactly the confusion `gh auth status` would have introduced.
    //
    // 403 is deliberately absent: it is mostly a rate limit, which is retryable and belongs
    // under a failed query, not under "sign in again".
    if stderr.contains("HTTP 401") {
        return GhFailure::Unauthenticated;
    }

    // **Not a repository gh can ask about.** Three different sentences for one state, and
    // each is a thing a person really does: a folder that was never `git init`ed, a
    // repository that was never pushed, and one whose remote is not GitHub.
    if stderr.contains("not a git repository")
        || stderr.contains("no git remotes found")
        || stderr.contains("point to a known GitHub host")
    {
        return GhFailure::NoRepository;
    }

    // Offline, rate-limited, no such repository, a host that refused. The daemon's sentence
    // is what tells these apart for the user.
    GhFailure::QueryFailed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{Scratch, gh_or_skip, git_or_skip};

    /// An ending, as though gh had produced it.
    fn ended(code: i32, stderr: &str) -> Finished {
        Finished {
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            timed_out: false,
            truncated: false,
        }
    }

    #[test]
    fn a_missing_gh_is_reported_before_anything_is_spawned() {
        // The `gh_missing` state, and the reason resolution happens up front: on Windows a
        // bare spawn fails with `NotFound` for several unrelated causes (traps register #8).
        assert_eq!(
            Gh::located_as("nysia-no-such-gh-exists").unwrap_err(),
            GhFailure::NotInstalled
        );
    }

    #[test]
    fn an_expired_token_is_told_apart_from_being_offline() {
        // **The test the module exists for**, and the one that pins the string match.
        //
        // Both of these are exit 1. Collapsing them — which is what mapping exit 4 to
        // "unauthenticated" and everything else to "the query failed" does — is precisely
        // the lie the plan forbids: the user whose token expired is told the network is
        // down, and re-authenticating never occurs to them.
        //
        // The two stderr strings are measured against gh 2.97.0, not invented. If gh rewords
        // the first, this goes red and the failure is a heading one notch less specific
        // rather than a wrong one.
        assert_eq!(
            classify(&ended(
                1,
                "HTTP 401: Bad credentials (https://api.github.com/graphql)\n\
                 Try authenticating with:  gh auth login -h github.com"
            )),
            Some(GhFailure::Unauthenticated),
            "a rejected token must read as a credential problem"
        );
        assert_eq!(
            classify(&ended(
                1,
                "Post \"https://api.github.com/graphql\": dial tcp 140.82.121.6:443: \
                 connectex: No connection could be made because the target machine \
                 actively refused it."
            )),
            Some(GhFailure::QueryFailed),
            "an unreachable host must not read as a credential problem"
        );
    }

    #[test]
    fn never_authenticated_and_a_rejected_token_are_one_state_by_two_roads() {
        // Exit 4 is "never authenticated" and carries no 401, so it has to be recognised by
        // its code. Asserting the code path rather than the stderr is the point: a classifier
        // that only read stderr would send this to `query_failed`.
        assert_eq!(
            classify(&ended(
                NEVER_AUTHENTICATED_EXIT,
                "To get started with GitHub CLI, please run:  gh auth login\n\
                 Alternatively, populate the GH_TOKEN environment variable with a GitHub \
                 API authentication token."
            )),
            Some(GhFailure::Unauthenticated)
        );
    }

    #[test]
    fn a_folder_with_no_github_remote_is_not_a_repository_rather_than_no_issues() {
        // §5's question, and the reason it is not `query_failed` either: "this is not on
        // GitHub" is a sentence a person can act on, and "the query failed" is not.
        //
        // All three are measured against gh 2.97.0.
        for stderr in [
            "failed to run git: fatal: not a git repository (or any of the parent \
             directories): .git",
            "no git remotes found",
            "none of the git remotes configured for this repository point to a known GitHub \
             host. To tell gh about a new GitHub host, please use `gh auth login`",
        ] {
            assert_eq!(
                classify(&ended(1, stderr)),
                Some(GhFailure::NoRepository),
                "not a GitHub repository: {stderr}"
            );
        }
    }

    #[test]
    fn a_repository_with_no_open_issues_is_a_success_and_not_a_failure() {
        // The state the plan says must be reachable and must not be a placeholder: this
        // repository's v0.1 backlog was cleared, so `[]` on exit 0 is the truthful answer
        // today. Collapsing it into any of the failures above is the lie the screen's four
        // endings exist to prevent.
        let empty = Finished {
            code: Some(0),
            stdout: b"[]".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
            truncated: false,
        };
        assert_eq!(classify(&empty), None);
    }

    #[test]
    fn a_truncated_answer_is_unreadable_rather_than_a_short_list() {
        // gh exits **zero** having written more than the cap, so without this the JSON would
        // be parsed, fail mid-array, and be reported as whatever the parser made of it. The
        // exit code is deliberately 0 here: that is what makes the check's position — before
        // the code is looked at — load-bearing rather than tidy.
        let cut = Finished {
            code: Some(0),
            stdout: b"[{\"number\":1".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
            truncated: true,
        };
        assert_eq!(classify(&cut), Some(GhFailure::Unreadable));
    }

    #[test]
    fn a_query_that_overran_its_deadline_is_a_failed_query() {
        let hung = Finished {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: true,
            truncated: false,
        };
        assert_eq!(classify(&hung), Some(GhFailure::QueryFailed));
    }

    #[test]
    fn a_missing_repository_is_a_failed_query_rather_than_a_missing_remote() {
        // The boundary between the two "there is nothing to list" states. A repository that
        // does not exist — or that this token cannot see — is a *query* that failed, because
        // the folder's remote was perfectly readable and GitHub is what said no.
        assert_eq!(
            classify(&ended(
                1,
                "GraphQL: Could not resolve to a Repository with the name \
                 'Shironex/definitely-not-a-real-repo-xyz'. (repository)"
            )),
            Some(GhFailure::QueryFailed)
        );
    }

    #[test]
    fn the_query_asks_for_no_body_and_states_its_own_limit() {
        let argv: Vec<String> = issue_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        // The body is not requested, and a field that is not on the wire cannot be logged by
        // accident later (traps register #13/#14). Asserted on the field list itself rather
        // than on the whole vector, so that adding an unrelated flag does not fail it.
        let fields = argv
            .iter()
            .position(|arg| arg == "--json")
            .map(|at| argv[at + 1].clone())
            .expect("the query asks for named fields");
        assert!(
            !fields.contains("body"),
            "the issue body must never be requested: {fields}"
        );

        // The limit is stated. Without it gh silently returns 30 and the screen shows a
        // truncated list as though it were the whole one.
        let limit = argv
            .iter()
            .position(|arg| arg == "--limit")
            .map(|at| argv[at + 1].clone())
            .expect("the query states a limit rather than taking gh's default of 30");
        assert_eq!(limit, ISSUE_LIMIT.to_string());
    }

    #[test]
    fn a_real_gh_in_a_repository_with_no_remote_answers_that_it_is_not_a_repository() {
        // The one thing the fixture tests above cannot prove: that this module's argument
        // vector and environment policy really do produce the ending it claims, out of a
        // real gh. Everything else here reads a `Finished` somebody typed.
        //
        // **Deterministic and offline.** gh fails on the missing remote before it opens a
        // socket, so this needs no network, no token and no GitHub account — which is what
        // makes it safe to run on a CI leg that has gh but no credentials, and what makes a
        // failure here mean the spawn is wrong rather than that the network is.
        let (Some(_git), Some(gh)) = (git_or_skip(), gh_or_skip()) else {
            return;
        };
        let scratch = Scratch::new("gh-no-remote");
        let repository = scratch.repository("local-only");
        let at = CanonicalPath::of(&repository).expect("a folder");

        assert_eq!(
            gh.issues(&at),
            Err(GhFailure::NoRepository),
            "a repository that was never pushed is not a repository with no issues"
        );
    }

    #[test]
    fn ghs_environment_keeps_every_credential_and_removes_what_would_redirect_the_answer() {
        // The runtime half of the compile-time guarantee above. `SCRUB_IS_CREDENTIAL_FREE`
        // refuses to build if a credential is scrubbed; this asserts the same thing about
        // the environment that is actually applied to a child, which is the set that
        // matters — a variable removed by the runner rather than by the policy would not be
        // visible to a `const fn`.
        let mut command = std::process::Command::new("nysia-not-spawned");
        super::super::runner::apply_environment(&mut command, &GH_ENV);

        let changes: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();

        for credential in GH_CREDENTIAL_VARS {
            assert!(
                !changes
                    .iter()
                    .any(|(key, value)| key == credential && value.is_none()),
                "{credential} must reach gh: removing it unauthenticates the machine"
            );
        }

        // And the two that would corrupt the parse rather than merely change it: both make
        // gh write ANSI escapes into output that is redirected, which is the stream this
        // module hands to a JSON parser.
        for corrupting in ["GH_FORCE_TTY", "CLICOLOR_FORCE"] {
            assert!(
                changes
                    .iter()
                    .any(|(key, value)| key == corrupting && value.is_none()),
                "{corrupting} must be removed: it puts escape sequences in the JSON"
            );
        }

        // gh's own prompt switch, and not git's. `GIT_TERMINAL_PROMPT` here would look like
        // hardening and do nothing.
        assert!(
            changes
                .iter()
                .any(|(key, value)| key == "GH_PROMPT_DISABLED" && value.as_deref() == Some("1")),
            "gh's prompt must be disabled with gh's own switch"
        );
    }
}
