//! The issue list, on the wire.
//!
//! D-5: tasks are GitHub Issues, **queried live**, with no local task domain model. There is
//! no table behind this and nothing keyed by it — [`Issue`] is a row in flight, and the only
//! thing that persists between two queries is the project the caller named.
//!
//! # Why this type exists now
//!
//! `apps/web` has been reading this shape since the Tasks screen shipped, out of a
//! hand-written mirror that said so in its own documentation: *"Rust does not export these
//! yet — wave C1 serves `tasks_list`, and their `nysia-proto` types land with them."* This is
//! that landing: D-13 makes Rust the sole authority on a wire shape the moment Rust spells
//! it, and this is Rust spelling it.
//!
//! **The window has not moved onto it, and this module does not get to say that it has.**
//! `apps/web/src/tasks/issue.ts` is still hand-written, still what the store and the screen
//! import, and still guarded at runtime by `transport/tasks.ts`; nothing imports the
//! generated type yet. Moving it is a change in `apps/web` and it is not in this one.
//!
//! It will not be a deletion when it happens, either. `issue.ts` says so itself — *"this file
//! does not then become a re-export of the generated type"* — because it describes the shape
//! the **screen** draws rather than the shape the wire carries. What the generated type
//! replaces is the wire half: `transport/tasks.ts` starts from this instead of from
//! `unknown`, `tsc` takes over the work of noticing a missing field, and the conversion those
//! functions already do is what stays.
//!
//! One thing for whoever does it to re-read rather than trust: two of the three differences
//! `issue.ts` lists are differences from *gh*, and the daemon now flattens both before they
//! reach this type. What that leaves worth converting is a question for that change, with
//! both files open.
//!
//! The shape is taken from what the window already parses, not invented beside it. Four
//! fields are non-optional because its reader refuses a row without them — `number`, `title`,
//! `updatedAt` and `url` — and a type that made any of them optional would produce answers
//! that build here and are rejected there.
//!
//! # Two fields that are not gh's
//!
//! `gh issue list --json` answers with an **object** for `author` and objects for `labels`,
//! and neither crosses this wire in that shape. The daemon flattens both before it answers.
//!
//! - **`author` is the login**, a bare string. The window's reader treats a non-string as
//!   *no author* — deliberately, because GitHub really does answer with no author for a
//!   deleted account — so passing gh's object through would render every row with no author
//!   and report nothing wrong. A quiet wrong answer, which this project ranks below a loud
//!   failure.
//! - **`labels` are names only.** gh sends `{id, name, description, color}` per label, and
//!   the colour is dropped by the window either way: every colour in Nysia is a token, and a
//!   hex off the wire is a pixel the theme switcher cannot reach. Sending it would be
//!   shipping a field to be discarded, so it is not on the wire at all.
//!
//! **The body is not here**, and that is the sharpest of the three. Nothing renders it, it is
//! someone else's text (traps register #13/#14), and a field that is not on the wire cannot
//! be logged by accident later. The daemon does not ask gh for it either.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::project::ProjectId;

/// Whether an issue is open.
///
/// **An enum, not gh's string.** `gh` sends `OPEN` and `CLOSED` and the pill reads `● Open`,
/// so somebody has to decide what the wire carries; a Rust enum makes the daemon do it once
/// rather than every client doing it slightly differently. The lowercase spelling is the one
/// the window already branches on.
///
/// Both variants exist even though [`TasksList`] asks only for open issues: the state is
/// rendered, a filter that asks for more is a later milestone's flag rather than a later
/// milestone's *type*, and a field whose only possible value is `open` would be one the
/// screen could not have drawn a pill for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum IssueState {
    /// Open, which is the only state v0.3 asks for.
    Open,
    /// Closed.
    Closed,
}

/// One GitHub issue, as the Tasks screen draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Issue {
    /// The issue number, which the ID column prints after a hash.
    ///
    /// Unique within a repository, which is the whole of the argument that a branch name
    /// derived from it is unique too (`apps/web/src/tasks/branchName.ts`). **Nothing keys a
    /// worktree by it** — D-6 keys by branch, and the number reaches a branch only as text
    /// inside one.
    pub number: u64,
    /// The issue's title, as GitHub holds it.
    pub title: String,
    /// Whether it is open. See [`IssueState`].
    pub state: IssueState,
    /// ISO 8601, exactly as `gh` sends it.
    ///
    /// Carried as a string rather than parsed into a timestamp on purpose. The window formats
    /// it for a 110px column and nothing in the daemon does arithmetic on it, so parsing here
    /// would be a conversion that can fail, in aid of nobody — and a row whose date the
    /// daemon could not parse would be a row the screen never saw.
    pub updated_at: String,
    /// The issue on github.com.
    ///
    /// The only field carrying the owner and the repository, which is why the window recovers
    /// its `owner · repo` sub-line from it: `gh issue list --json` refuses the name
    /// `repository` outright, so there is nowhere else for either half to come from.
    pub url: String,
    /// The login that opened it, or `null` where GitHub reported no author.
    ///
    /// `null` is a real answer rather than a missing one: the account may be deleted. See the
    /// module documentation for why this is the login and not gh's author object.
    pub author: Option<String>,
    /// Label names, without their colours. See the module documentation.
    pub labels: Vec<String>,
}

/// Ask for a project's open issues.
///
/// **The project and nothing else.** No repository slug, no path, no filter and no query
/// string: v0.3's screen passes a project, and every other control it draws is rendered
/// disabled with a title saying when it arrives. The daemon resolves the folder from the
/// registration and lets `gh` read the remote out of it, so there is no repository name on
/// this wire to be wrong about — and no path, which is the half that matters (traps register
/// #13/#14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TasksList {
    /// The project whose issues to list.
    pub project: ProjectId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue() -> Issue {
        Issue {
            number: 200,
            title: "The issue list, served from the daemon".to_owned(),
            state: IssueState::Open,
            updated_at: "2026-09-16T09:12:44Z".to_owned(),
            url: "https://github.com/Shironex/nysia/issues/200".to_owned(),
            author: Some("Shironex".to_owned()),
            labels: vec!["area:daemon".to_owned(), "P1-high".to_owned()],
        }
    }

    #[test]
    fn an_issue_carries_the_four_fields_the_window_refuses_a_row_without() {
        // `apps/web/src/transport/tasks.ts` refuses the **whole query** if a row is missing
        // its number, title, date or URL — so a Rust type that made any of them optional
        // would let the daemon build an answer the window throws away in one piece. They are
        // asserted as present-and-typed rather than merely non-null, because `null` in any of
        // these is exactly what that reader rejects.
        let json = serde_json::to_value(issue()).unwrap();
        assert!(json["number"].is_u64(), "{json}");
        assert!(json["title"].is_string(), "{json}");
        assert!(json["updatedAt"].is_string(), "{json}");
        assert!(json["url"].is_string(), "{json}");
    }

    #[test]
    fn the_wire_spelling_is_camel_case_and_the_state_is_lower_case() {
        let json = serde_json::to_value(issue()).unwrap();
        // `updatedAt`, not `updated_at`: the window reads the camel spelling and a snake one
        // would arrive as a missing field, which its reader turns into a refused query.
        assert_eq!(json["updatedAt"], "2026-09-16T09:12:44Z");
        assert!(json.get("updated_at").is_none(), "{json}");
        // `open`, not gh's `OPEN`. Deciding this once here is the point of the enum.
        assert_eq!(json["state"], "open");
        assert_eq!(serde_json::to_value(IssueState::Closed).unwrap(), "closed");
    }

    #[test]
    fn an_author_and_its_absence_both_round_trip() {
        // `null` is a real answer: GitHub reports no author for an issue whose account has
        // been deleted, and the window renders that row without a name rather than refusing
        // the list. So the absence has to survive the wire as an absence.
        for author in [Some("Shironex".to_owned()), None] {
            let issue = Issue {
                author: author.clone(),
                ..issue()
            };
            let json = serde_json::to_value(&issue).unwrap();
            assert_eq!(json["author"], serde_json::json!(author));
            assert_eq!(serde_json::from_value::<Issue>(json).unwrap(), issue);
        }
    }

    #[test]
    fn labels_are_names_and_carry_no_colour() {
        // gh sends `{id, name, description, color}` per label. Every colour in Nysia is a
        // token and a hex off the wire is a pixel the theme switcher cannot reach, so the
        // colour is not dropped at the edge — it is never on the wire to be dropped.
        let json = serde_json::to_value(issue()).unwrap();
        assert_eq!(
            json["labels"],
            serde_json::json!(["area:daemon", "P1-high"])
        );
        assert!(
            json["labels"][0].is_string(),
            "a label is a name, not an object: {json}"
        );
    }

    #[test]
    fn an_issue_body_has_no_field_to_arrive_in() {
        // Traps register #13/#14: the body is someone else's text, nothing renders it, and a
        // field that is not on the wire cannot be logged by accident later. Asserted on the
        // serialised object rather than on the struct, because this is a claim about what a
        // peer can send us as much as about what we send.
        let json = serde_json::to_value(issue()).unwrap();
        let object = json.as_object().expect("an issue is an object");
        assert!(!object.contains_key("body"), "{json}");
        assert_eq!(
            object.len(),
            7,
            "an issue has exactly the seven fields the screen draws: {json}"
        );
    }

    #[test]
    fn a_request_names_a_project_and_nothing_else() {
        // v0.3's screen passes a project and nothing else — every other control it draws is
        // disabled with a title saying when it arrives. **No path**, which is the half that
        // matters: a repository path names a person's disk (traps register #13/#14), and a
        // field that does not exist cannot carry one.
        let request = TasksList {
            project: "proj_0123456789abcdef0123456789abcdef"
                .parse()
                .expect("a well-formed project id"),
        };
        let json = serde_json::to_value(&request).unwrap();
        let object = json.as_object().expect("a request is an object");
        assert_eq!(object.len(), 1, "{json}");
        assert!(object.contains_key("project"), "{json}");
        assert_eq!(serde_json::from_value::<TasksList>(json).unwrap(), request);
    }
}
