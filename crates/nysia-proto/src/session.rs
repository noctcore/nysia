//! Creating, listing and closing sessions.
//!
//! A session is a PTY the daemon owns: a shell, or (from v0.2) a Claude agent. The verbs
//! here are deliberately thin, because the interesting state — the grid, the replay ring,
//! the logical line log — lives in the daemon and is read through the terminal verbs
//! (§7.2, D-7). Nothing in this module knows how to spawn anything; it only describes what
//! a caller asked for and what the daemon answered.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::identity::{Incarnation, PaneKey, SessionHandle, SessionKind};
use crate::project::ProjectId;

/// Which shell to spawn.
///
/// Four variants, not an open registry, because D-3 fixes the set for v1. WSL is one of
/// them and nothing more: D-17 makes it `wsl.exe -d <distro>` in a ConPTY, with no path
/// translation, no worktrees and no agents inside it. That is what the design's menu entry
/// promises and all it promises.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "shell", rename_all = "snake_case")]
#[ts(export)]
pub enum ShellProfile {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// The Windows command processor (`cmd.exe`).
    Cmd,
    /// The bash that ships with Git for Windows.
    GitBash,
    /// A WSL distribution, as a plain shell (D-17).
    Wsl {
        /// Which distribution, or `null` for whatever `wsl.exe` treats as the default.
        ///
        /// Naming it is the difference between "the user's default distro, whatever that is
        /// today" and "Ubuntu-24.04". A session that outlives a `wsl --set-default` should
        /// land where it landed the first time, so the daemon records what it resolved.
        distro: Option<String>,
    },
}

/// Where a session starts.
///
/// # Why a project is named rather than its folder
///
/// **The window has no folder to send.** [`Project`](crate::Project) carries no path on the
/// wire, on purpose: a path travels in one direction only, into
/// [`ProjectRegister`](crate::ProjectRegister) (traps register #13/#14), and a
/// [`ProjectId`] is a one-way hash of one. So a window that wanted a session in a project
/// could only ever send `null`, and the daemon opened the session wherever the daemon itself
/// was running — a Claude session started on a project whose banner named the user's home
/// directory. The fix is the one [`TasksList`](crate::TasksList) already made for the same
/// reason: name the project, and let the daemon resolve the folder from its registration.
///
/// # The three cases
///
/// - **`null` — no project.** The session inherits the daemon's own working directory and
///   belongs to no worktree. That is what `nysia session create` without `--cwd` has always
///   done, and what the window sends when no project is selected.
/// - **[`Self::Project`]** — the folder the project was registered at, which is its primary
///   checkout, resolved by the daemon when this request arrives. A project that has been
///   forgotten since the caller listed it is refused as `unknown_project`, and one whose
///   folder no longer opens as `path_unreadable`. **Neither falls back to the daemon's own
///   directory**: opening somewhere silently wrong is the defect this variant exists to fix.
/// - **[`Self::Path`]** — a folder the caller names, which only a caller holding a path can
///   do: the CLI's `--cwd`. Advisory — the daemon confines it before spawning anything
///   (absolute, existing, a directory), so a path here is a request rather than a guarantee.
///
/// # Why an enum and not a `project` field beside `cwd`
///
/// So that "both" cannot be sent. Two optional fields would need a rule for which one wins
/// and a refusal for a caller that set both; one field with three shapes has neither.
///
/// It also decides what an **older daemon** does with a request that names a project, and
/// that is worth saying because the daemon outlives the window (D-1). An additive `project`
/// field would be ignored by a daemon that predates it — serde drops unknown fields — and
/// that daemon would open the session in its own directory with nothing to say so: the
/// defect again, returned by the upgrade that was meant to fix it. An object where that
/// daemon expects a string is instead a frame it cannot read, and it refuses the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "from", rename_all = "snake_case")]
#[ts(export)]
pub enum WorkingDirectory {
    /// A registered project's folder, resolved by the daemon.
    Project {
        /// Which project. The id is all the window holds, and all it needs to hold.
        project: ProjectId,
    },
    /// A folder the caller names.
    Path {
        /// The folder. Confined by the daemon before anything is spawned.
        path: PathBuf,
    },
}

/// Spawn a session.
///
/// The [`SessionHandle`] is always the daemon's to mint. The [`PaneKey`] is not: whoever
/// owns the pane names it, and when nobody does the daemon fills in. See
/// [`pane_key`](Self::pane_key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionCreate {
    /// A shell or an agent.
    pub kind: SessionKind,
    /// The pane this session belongs to, when the caller owns one.
    ///
    /// **The GUI supplies it.** It creates the tab and the leaf, so it already holds
    /// `<tabId>:<leafId>` (§3.3). Having the daemon mint a key instead would leave the
    /// window maintaining a map from daemon keys back to its own tree — the bookkeeping §4
    /// exists to delete, and the first thing to rot the next time a tab is split or
    /// restored.
    ///
    /// **The CLI omits it.** `nysia session create` has no pane and no tab, and requiring a
    /// key from it would force it to invent one — putting the minting logic in every client
    /// rather than in the one process that owns persistence. When this is `null` the daemon
    /// mints a well-formed synthetic `<uuid>:<uuid>`.
    ///
    /// Either way [`SessionCreated::pane_key`] carries the definitive value, so a caller
    /// never has to guess what it ended up with.
    ///
    /// Two rules come with this. They are properties of the wire rather than of one
    /// implementation, which is why they are written down here even though the daemon is
    /// what enforces them:
    ///
    /// 1. A create naming a pane key that already has a live session is **refused**, never
    ///    silently rebound. Two clients racing for one pane is a real case, and the loser
    ///    has to be told rather than left believing it owns a session it does not.
    /// 2. The incarnation counter is **per pane key and owned by the daemon**, so a
    ///    relaunch in the same pane is `<paneKey>@<n+1>`. A client that counted for itself
    ///    would restart at zero across its own restart and make two spawns
    ///    indistinguishable — which is the one thing [`Incarnation`] exists to prevent.
    pub pane_key: Option<PaneKey>,
    /// Which shell, when `kind` is [`SessionKind::Shell`]. `null` takes the platform
    /// default. Ignored for an agent session, which is always `claude`.
    pub profile: Option<ShellProfile>,
    /// Where to start: a project, a folder, or `null` for neither. See [`WorkingDirectory`].
    ///
    /// `null` is **not** a project root: it is the daemon's own working directory.
    pub cwd: Option<WorkingDirectory>,
    /// Environment entries layered over the daemon's scrubbed base environment.
    ///
    /// Layered *over*, never instead of: §7.1 scrubs `CLAUDECODE`,
    /// `CLAUDE_CODE_CHILD_SESSION` and friends, and forces `TERM`/`COLORTERM`. A `BTreeMap`
    /// rather than a `HashMap` so the JSON is byte-identical run to run, which is what
    /// makes the golden fixtures reviewable.
    pub env_overrides: BTreeMap<String, String>,
    /// Initial width in cells. Must be at least 1; the daemon refuses zero rather than
    /// handing ConPTY a degenerate console.
    pub cols: u16,
    /// Initial height in cells. Must be at least 1.
    pub rows: u16,
}

/// What the daemon minted for a [`SessionCreate`].
///
/// All three ids, because they answer different questions and a caller generally needs
/// more than one: route with the handle, persist the pane key, attribute status to the
/// incarnation (§3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionCreated {
    /// Runtime-scoped routing id.
    pub handle: SessionHandle,
    /// The durable pane identity.
    pub pane_key: PaneKey,
    /// This spawn, distinct from any later relaunch in the same pane.
    pub incarnation: Incarnation,
}

/// List every session the daemon owns.
///
/// Deliberately empty. There is no filter in v0.1: the daemon holds tens of sessions, not
/// thousands, and a filter that only ever ran against a small list would be a wire shape to
/// support forever for no measured benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionList {}

/// How a session's child ended.
///
/// An enum rather than a pair of nullable numbers, so "exited with 0" and "killed by
/// SIGTERM" are the only two shapes and "neither" is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum ExitStatus {
    /// The child returned a status code.
    Exited {
        /// The code it returned.
        code: i32,
    },
    /// The child was killed by a signal.
    ///
    /// Unix only. ConPTY has no signals (§7.1), so a Windows session that is torn down
    /// reports whatever code the Job Object teardown left behind, never this variant.
    Signaled {
        /// The signal number.
        signal: i32,
    },
}

/// One row of [`SessionList`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionSummary {
    /// Runtime-scoped routing id.
    pub handle: SessionHandle,
    /// The durable pane identity.
    pub pane_key: PaneKey,
    /// A shell or an agent.
    pub kind: SessionKind,
    /// What the tab shows. Derived by the daemon from OSC 0/2 or from the profile.
    pub title: String,
    /// Unix milliseconds at which the session was created.
    pub created_at_ms: u64,
    /// How the child ended, or `null` while it is still running.
    ///
    /// Trap 11: this follows the child's `wait()`, not the PTY's EOF. A backgrounded server
    /// holding the slave open keeps EOF away long after the shell is gone, so a client that
    /// waited on EOF would show a dead session as live.
    pub exit_status: Option<ExitStatus>,
}

/// Close a session and tear down its process tree.
///
/// Tearing down is a Job Object close on Windows and a `killpg` ladder on Unix (§7.1).
/// Neither is instantaneous, which is why the answer does not carry an exit status — a
/// caller that needs one waits for it with `terminal wait --for exit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionClose {
    /// Which session.
    pub handle: SessionHandle,
}

/// Ask which shells this daemon can launch.
///
/// # Why the window has to ask
///
/// [`ShellProfile`]'s four variants are the four a menu *can* offer, and deriving the menu
/// from them offered PowerShell 7 on a machine that did not have it: the person picked it,
/// and learned which shells were real from the refusal. Which programs resolve is a fact
/// about the machine the daemon runs on — its `PATH`, where Git for Windows is installed —
/// and the daemon is the only party that can look.
///
/// # When the answer is computed, and what makes it stale
///
/// **On every request**, by resolving each profile exactly as a spawn would, and cached
/// nowhere, so it is never older than the request that asked for it. A shell that appears in,
/// or vanishes from, a directory already on the daemon's `PATH` is reflected by the next one.
///
/// **A directory an installer adds to `PATH` is not.** The daemon searches the `PATH` it
/// started with, and a running process never sees a later change to the system's — so a
/// shell whose installer puts a new directory on `PATH`, as PowerShell 7's MSI does, is
/// offered once the daemon has been restarted from an environment that has it. That is true
/// of every program the daemon resolves, `git` included.
///
/// What makes a client's copy stale is the client not asking again, which is the client's
/// decision — and why a launch this answer called possible can still be refused. That refusal
/// stays; this answer does not replace it.
///
/// Deliberately empty, like [`SessionList`]: there are four profiles, and a filter over four
/// would be a wire shape to support for no benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileList {}

/// One shell, and whether the daemon can launch it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileAvailability {
    /// Which shell. WSL is listed once, as `distro: null` — the distribution a bare
    /// `wsl.exe` would open — because that is the one WSL entry a menu offers.
    pub profile: ShellProfile,
    /// Why the daemon cannot launch it right now, or `null` when it can.
    ///
    /// A sentence for a person, naming the shell and what resolution found — *not on
    /// `PATH`*, *not on this platform* — and **never a file**: the candidate a resolution
    /// rejected is a path on somebody's disk (traps register #13/#14). Not a closed set, so
    /// nothing should branch on its text; `null` or not is the whole of the machine-readable
    /// answer.
    pub unavailable: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> SessionHandle {
        "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60".parse().unwrap()
    }

    #[test]
    fn a_shell_profile_is_tagged_and_wsl_carries_its_distro() {
        assert_eq!(
            serde_json::to_value(ShellProfile::Pwsh).unwrap(),
            serde_json::json!({ "shell": "pwsh" })
        );
        assert_eq!(
            serde_json::to_value(ShellProfile::GitBash).unwrap(),
            serde_json::json!({ "shell": "git_bash" })
        );
        let wsl = ShellProfile::Wsl {
            distro: Some("Ubuntu-24.04".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(&wsl).unwrap(),
            serde_json::json!({ "shell": "wsl", "distro": "Ubuntu-24.04" })
        );
        assert_eq!(
            serde_json::from_value::<ShellProfile>(serde_json::to_value(&wsl).unwrap()).unwrap(),
            wsl
        );
        assert_eq!(
            serde_json::from_value::<ShellProfile>(serde_json::json!({
                "shell": "wsl", "distro": null
            }))
            .unwrap(),
            ShellProfile::Wsl { distro: None }
        );
        assert!(
            serde_json::from_value::<ShellProfile>(serde_json::json!({ "shell": "zsh" })).is_err()
        );
    }

    #[test]
    fn env_overrides_serialise_in_a_stable_order() {
        let create = SessionCreate {
            kind: SessionKind::Shell,
            pane_key: Some(PaneKey::new("tab_1", "leaf_1").unwrap()),
            profile: Some(ShellProfile::Pwsh),
            cwd: None,
            env_overrides: BTreeMap::from([
                ("Z_LAST".to_owned(), "1".to_owned()),
                ("A_FIRST".to_owned(), "2".to_owned()),
                ("M_MID".to_owned(), "3".to_owned()),
            ]),
            cols: 120,
            rows: 30,
        };
        let text = serde_json::to_string(&create).unwrap();
        assert!(
            text.contains(r#""envOverrides":{"A_FIRST":"2","M_MID":"3","Z_LAST":"1"}"#),
            "{text}"
        );
        assert_eq!(
            serde_json::from_str::<SessionCreate>(&text).unwrap(),
            create
        );
    }

    #[test]
    fn a_create_round_trips_with_every_optional_field_absent() {
        let create = SessionCreate {
            kind: SessionKind::Agent,
            pane_key: None,
            profile: None,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        };
        let json = serde_json::to_value(&create).unwrap();
        // Explicit `null`s rather than omitted keys, matching ts-rs's `T | null`.
        assert_eq!(json["profile"], serde_json::Value::Null);
        assert_eq!(json["cwd"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<SessionCreate>(json).unwrap(),
            create
        );
    }

    #[test]
    fn a_working_directory_says_which_of_its_cases_it_is() {
        let project = ProjectId::from_canonical_path(std::path::Path::new("/src/nysia"))
            .expect("a unicode path");
        let in_project = WorkingDirectory::Project {
            project: project.clone(),
        };
        assert_eq!(
            serde_json::to_value(&in_project).unwrap(),
            serde_json::json!({ "from": "project", "project": project.as_str() })
        );
        let at_path = WorkingDirectory::Path {
            path: PathBuf::from("/src/nysia"),
        };
        assert_eq!(
            serde_json::to_value(&at_path).unwrap(),
            serde_json::json!({ "from": "path", "path": "/src/nysia" })
        );
        for cwd in [in_project, at_path] {
            let json = serde_json::to_value(&cwd).unwrap();
            assert_eq!(
                serde_json::from_value::<WorkingDirectory>(json).unwrap(),
                cwd
            );
        }

        // A bare string is what `cwd` was before a project could be named, and it is refused
        // rather than read as a path: there is exactly one spelling for each case.
        assert!(
            serde_json::from_value::<WorkingDirectory>(serde_json::json!("/src/nysia")).is_err()
        );
        // And a project that is not an id is refused at the boundary, not three layers later.
        assert!(
            serde_json::from_value::<WorkingDirectory>(serde_json::json!({
                "from": "project", "project": "/src/nysia"
            }))
            .is_err()
        );
    }

    #[test]
    fn a_create_lets_the_pane_owner_name_the_pane_and_everyone_else_omit_it() {
        // The GUI already holds `<tabId>:<leafId>`, so it says so.
        let from_the_window = SessionCreate {
            kind: SessionKind::Shell,
            pane_key: Some(PaneKey::new("tab_1", "leaf_1").unwrap()),
            profile: Some(ShellProfile::Pwsh),
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        };
        assert_eq!(
            serde_json::to_value(&from_the_window).unwrap()["paneKey"],
            "tab_1:leaf_1"
        );

        // The CLI has no pane and no tab, so it does not invent one.
        let from_the_cli = SessionCreate {
            pane_key: None,
            ..from_the_window.clone()
        };
        assert_eq!(
            serde_json::to_value(&from_the_cli).unwrap()["paneKey"],
            serde_json::Value::Null
        );

        for create in [from_the_window, from_the_cli] {
            let json = serde_json::to_value(&create).unwrap();
            assert_eq!(
                serde_json::from_value::<SessionCreate>(json).unwrap(),
                create
            );
        }

        // A malformed key is still refused at the boundary rather than three layers later:
        // `Option` makes the field absent-able, not unvalidated.
        assert!(
            serde_json::from_value::<SessionCreate>(serde_json::json!({
                "kind": "shell",
                "paneKey": "no-separator",
                "profile": null,
                "cwd": null,
                "envOverrides": {},
                "cols": 80,
                "rows": 24,
            }))
            .is_err()
        );
    }

    #[test]
    fn an_exit_status_cannot_be_neither_code_nor_signal() {
        assert_eq!(
            serde_json::to_value(ExitStatus::Exited { code: 0 }).unwrap(),
            serde_json::json!({ "outcome": "exited", "code": 0 })
        );
        assert_eq!(
            serde_json::to_value(ExitStatus::Signaled { signal: 15 }).unwrap(),
            serde_json::json!({ "outcome": "signaled", "signal": 15 })
        );
        assert!(
            serde_json::from_value::<ExitStatus>(serde_json::json!({ "outcome": "exited" }))
                .is_err()
        );
        assert!(serde_json::from_value::<ExitStatus>(serde_json::json!({ "code": 0 })).is_err());
    }

    #[test]
    fn a_summary_round_trips_running_and_finished() {
        let running = SessionSummary {
            handle: handle(),
            pane_key: PaneKey::new("tab_1", "leaf_1").unwrap(),
            kind: SessionKind::Shell,
            title: "pwsh".to_owned(),
            created_at_ms: 1_757_721_600_000,
            exit_status: None,
        };
        let finished = SessionSummary {
            exit_status: Some(ExitStatus::Exited { code: 130 }),
            ..running.clone()
        };
        for summary in [running, finished] {
            let json = serde_json::to_value(&summary).unwrap();
            assert_eq!(
                serde_json::from_value::<SessionSummary>(json).unwrap(),
                summary
            );
        }
    }

    #[test]
    fn the_id_carrying_shapes_round_trip() {
        let created = SessionCreated {
            handle: handle(),
            pane_key: PaneKey::new("tab_1", "leaf_1").unwrap(),
            incarnation: Incarnation::new(&PaneKey::new("tab_1", "leaf_1").unwrap(), 0),
        };
        let json = serde_json::to_value(&created).unwrap();
        assert_eq!(json["paneKey"], "tab_1:leaf_1");
        assert_eq!(json["incarnation"], "tab_1:leaf_1@0");
        assert_eq!(
            serde_json::from_value::<SessionCreated>(json).unwrap(),
            created
        );

        let close = SessionClose { handle: handle() };
        assert_eq!(
            serde_json::from_value::<SessionClose>(serde_json::to_value(&close).unwrap()).unwrap(),
            close
        );
        assert_eq!(
            serde_json::to_value(SessionList {}).unwrap(),
            serde_json::json!({})
        );
    }
}
