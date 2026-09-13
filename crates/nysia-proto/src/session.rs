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

/// Spawn a session.
///
/// The daemon assigns the [`PaneKey`] and the [`SessionHandle`]; a caller cannot name a
/// pane into existence, because the pane key is the durable primary key for status and
/// orchestration and minting it is the daemon's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionCreate {
    /// A shell or an agent.
    pub kind: SessionKind,
    /// Which shell, when `kind` is [`SessionKind::Shell`]. `null` takes the platform
    /// default. Ignored for an agent session, which is always `claude`.
    pub profile: Option<ShellProfile>,
    /// Where to start. `null` takes the project root.
    ///
    /// Advisory: the daemon confines it through `safe_join` / `path_confine` (§7.5) before
    /// spawning anything, so a path here is a request rather than a guarantee.
    pub cwd: Option<PathBuf>,
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
