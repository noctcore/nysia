//! Reading, writing, resizing and waiting on a terminal (§7.2).
//!
//! The one decision in this module that is worth more than the rest of it put together is
//! [`ReadMode::Screen`] being the **default**.
//!
//! Orca defaults `terminal read` to the accumulated escape-stripped stream. A `clear` typed
//! key by key therefore reads back as `cclclecleaclear`, because every intermediate repaint
//! is still in the stream — Orca's own help text apologises for it. An agent reading that
//! sees a line it never typed and reasons about a terminal that never existed.
//!
//! Nysia's grid is in Rust (D-7), so the rendered screen is already there to hand back, and
//! it is what a caller gets unless it asks for something else. Stream mode is opt-in and
//! cursor-paged off the logical line log, which is the right tool when the question is
//! "what scrolled past while I was away" rather than "what is on screen now".
//!
//! The default has to hold *on the wire*, not merely in Rust: a client that omits `mode`
//! gets the screen. That is `#[serde(default)]` on the field, and there is a test for it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::identity::SessionHandle;
use crate::session::ExitStatus;

/// Why a terminal value could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TerminalError {
    /// A read mode was not one of the two wire spellings.
    #[error("a read mode is `screen` or `stream`, got {0:?}")]
    ReadModeShape(String),
    /// A wait target was not one of the two wire spellings.
    #[error("a wait target is `exit` or `idle`, got {0:?}")]
    WaitForShape(String),
}

/// A position in a session's logical line log.
///
/// §7.2 gives every row that scrolls out of the viewport a monotonic id. A cursor is one of
/// those ids, so paging is "everything after N" rather than "the last N bytes" — the second
/// of which cannot be resumed after a disconnect without re-reading what you already had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LineCursor(pub u64);

impl LineCursor {
    /// The start of the log: everything the daemon still holds.
    pub const START: Self = Self(0);

    /// The cursor as the bare number it is on the wire.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for LineCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a [`TerminalRead`] returns.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReadMode {
    /// The rendered screen, as the grid currently shows it. **The default.**
    #[default]
    Screen,
    /// Logical lines from the scrollback log, paged by cursor.
    Stream,
}

impl ReadMode {
    /// The wire spelling, which is also what this type exports to TypeScript.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Stream => "stream",
        }
    }
}

impl fmt::Display for ReadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReadMode {
    type Err = TerminalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "screen" => Ok(Self::Screen),
            "stream" => Ok(Self::Stream),
            other => Err(TerminalError::ReadModeShape(other.to_owned())),
        }
    }
}

/// Read what a session has on screen, or what has scrolled past it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TerminalRead {
    /// Which session.
    pub handle: SessionHandle,
    /// Screen or stream. Absent means [`ReadMode::Screen`] — see the module docs.
    #[serde(default)]
    pub mode: ReadMode,
    /// Where to resume from in stream mode. Ignored in screen mode, which has no history.
    pub cursor: Option<LineCursor>,
    /// At most this many lines. `null` takes the daemon's ceiling.
    pub limit: Option<u32>,
}

impl TerminalRead {
    /// Read the rendered screen — the shape a caller that expresses no preference gets.
    #[must_use]
    pub fn screen(handle: SessionHandle) -> Self {
        Self {
            handle,
            mode: ReadMode::Screen,
            cursor: None,
            limit: None,
        }
    }

    /// Read the scrollback from `cursor` onwards.
    #[must_use]
    pub fn stream(handle: SessionHandle, cursor: LineCursor) -> Self {
        Self {
            handle,
            mode: ReadMode::Stream,
            cursor: Some(cursor),
            limit: None,
        }
    }
}

/// What a [`TerminalRead`] answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TerminalReadResult {
    /// The lines, oldest first, escape sequences already stripped.
    pub lines: Vec<String>,
    /// Where to resume next time.
    ///
    /// In stream mode this is one past the last line returned. In screen mode it is the
    /// head of the log, so a caller can switch modes without re-reading history it has.
    pub cursor: LineCursor,
    /// Which mode actually ran.
    ///
    /// Echoed rather than assumed. A caller that omitted `mode` should be able to see, in
    /// the answer, that it got the screen — the whole point of the default is that it is
    /// visible rather than implicit.
    pub mode: ReadMode,
    /// Whether `limit` or the daemon's ceiling cut the answer short. More lines are
    /// available from `cursor`.
    pub truncated: bool,
}

/// Write to a session's input.
///
/// The three fields are applied in a fixed order — `interrupt`, then `text`, then `enter` —
/// so one frame can say "cancel whatever is running and then send this command". Built with
/// [`TerminalSend::new`] and the helpers, which is why the defaults are all "do nothing".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TerminalSend {
    /// Which session.
    pub handle: SessionHandle,
    /// Literal bytes to write. Sent as typed; no shell quoting is applied.
    #[serde(default)]
    pub text: String,
    /// Append a carriage return after `text`.
    #[serde(default)]
    pub enter: bool,
    /// Send the interrupt character (`0x03`) before anything else.
    ///
    /// Not a signal: ConPTY has none (§7.1). This writes the byte the line discipline or
    /// the foreground TUI interprets, which is what a human pressing Ctrl-C does.
    #[serde(default)]
    pub interrupt: bool,
}

impl TerminalSend {
    /// Send nothing to `handle`; set the fields you mean.
    #[must_use]
    pub fn new(handle: SessionHandle) -> Self {
        Self {
            handle,
            text: String::new(),
            enter: false,
            interrupt: false,
        }
    }

    /// Type `text` and press return — the common case.
    #[must_use]
    pub fn line(handle: SessionHandle, text: impl Into<String>) -> Self {
        Self {
            handle,
            text: text.into(),
            enter: true,
            interrupt: false,
        }
    }

    /// Send Ctrl-C and nothing else.
    #[must_use]
    pub fn interrupt(handle: SessionHandle) -> Self {
        Self {
            handle,
            text: String::new(),
            enter: false,
            interrupt: true,
        }
    }
}

/// Tell the daemon the viewport changed size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TerminalResize {
    /// Which session.
    pub handle: SessionHandle,
    /// New width in cells. Must be at least 1.
    pub cols: u16,
    /// New height in cells. Must be at least 1.
    pub rows: u16,
}

/// What a [`TerminalWait`] is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WaitFor {
    /// The child process exited.
    ///
    /// The authoritative signal for anything Nysia runs on behalf of an agent: §7.2 spawns
    /// such a command as the PTY child directly and takes the status from `wait()`, so
    /// there are no escape sequences to interpret and nothing to be ambiguous about.
    Exit,
    /// The session stopped producing output and looks ready for input.
    ///
    /// Heuristic by construction — a repainting TUI never truly stops — and therefore the
    /// weaker of the two. Prefer [`WaitFor::Exit`] whenever the thing being waited on is a
    /// command rather than a human's shell.
    Idle,
}

impl WaitFor {
    /// The wire spelling, which is also what this type exports to TypeScript.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Idle => "idle",
        }
    }
}

impl fmt::Display for WaitFor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WaitFor {
    type Err = TerminalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "exit" => Ok(Self::Exit),
            "idle" => Ok(Self::Idle),
            other => Err(TerminalError::WaitForShape(other.to_owned())),
        }
    }
}

/// Block until a session exits or goes idle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TerminalWait {
    /// Which session.
    pub handle: SessionHandle,
    /// Exit or idle.
    pub wait_for: WaitFor,
    /// Give up after this many milliseconds. `null` waits as long as the connection lives.
    pub timeout_ms: Option<u64>,
}

/// How a [`TerminalWait`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum WaitOutcome {
    /// The child exited.
    Exited {
        /// How it ended.
        status: ExitStatus,
    },
    /// The session went quiet.
    Idle,
    /// `timeoutMs` elapsed first. Not an error: the caller asked for a bounded wait and
    /// got a bounded answer, and may wait again.
    TimedOut,
}

/// What a [`TerminalWait`] answered.
///
/// The outcome is flattened in, so the frame reads `{"handle":…,"outcome":"timed_out"}`
/// rather than nesting an object under a key of the same name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TerminalWaitResult {
    /// Which session was waited on.
    pub handle: SessionHandle,
    /// How the wait ended.
    #[serde(flatten)]
    pub outcome: WaitOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> SessionHandle {
        "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60".parse().unwrap()
    }

    #[test]
    fn a_read_with_no_mode_on_the_wire_is_a_screen_read() {
        // The footgun this defends against: Orca defaults to stream, so `clear` typed key
        // by key reads back as `cclclecleaclear`. A Rust-side `Default` alone would not
        // stop a client that simply omits the field from getting the other behaviour.
        let without_mode = serde_json::json!({
            "handle": "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
            "cursor": null,
            "limit": null,
        });
        let read: TerminalRead = serde_json::from_value(without_mode).unwrap();
        assert_eq!(read.mode, ReadMode::Screen);
        assert_eq!(read, TerminalRead::screen(handle()));

        // Only an explicit opt-in gets the stream.
        let with_stream = serde_json::json!({
            "handle": "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
            "mode": "stream",
            "cursor": 42,
            "limit": null,
        });
        let read: TerminalRead = serde_json::from_value(with_stream).unwrap();
        assert_eq!(read.mode, ReadMode::Stream);
        assert_eq!(read, TerminalRead::stream(handle(), LineCursor(42)));
    }

    #[test]
    fn read_modes_and_wait_targets_round_trip_in_their_wire_spelling() {
        for mode in [ReadMode::Screen, ReadMode::Stream] {
            assert_eq!(mode.to_string().parse::<ReadMode>().unwrap(), mode);
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{mode}\""));
        }
        for target in [WaitFor::Exit, WaitFor::Idle] {
            assert_eq!(target.to_string().parse::<WaitFor>().unwrap(), target);
            assert_eq!(
                serde_json::to_string(&target).unwrap(),
                format!("\"{target}\"")
            );
        }
        assert!("Screen".parse::<ReadMode>().is_err());
        assert!("Exit".parse::<WaitFor>().is_err());
        assert!(serde_json::from_str::<ReadMode>("\"raw\"").is_err());
    }

    #[test]
    fn a_read_result_echoes_the_mode_that_ran() {
        let result = TerminalReadResult {
            lines: vec!["$ echo hello".to_owned(), "hello".to_owned()],
            cursor: LineCursor(2),
            mode: ReadMode::Screen,
            truncated: false,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["mode"], "screen");
        assert_eq!(json["cursor"], 2);
        assert_eq!(
            serde_json::from_value::<TerminalReadResult>(json).unwrap(),
            result
        );
        assert_eq!(LineCursor::START.get(), 0);
    }

    #[test]
    fn a_send_defaults_every_effect_to_nothing() {
        let quiet = TerminalSend::new(handle());
        assert_eq!(quiet.text, "");
        assert!(!quiet.enter);
        assert!(!quiet.interrupt);

        let from_wire: TerminalSend = serde_json::from_value(serde_json::json!({
            "handle": "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
        }))
        .unwrap();
        assert_eq!(from_wire, quiet);

        assert_eq!(
            TerminalSend::line(handle(), "cargo test"),
            TerminalSend {
                handle: handle(),
                text: "cargo test".to_owned(),
                enter: true,
                interrupt: false,
            }
        );
        let ctrl_c = TerminalSend::interrupt(handle());
        assert!(ctrl_c.interrupt);
        assert_eq!(ctrl_c.text, "");
    }

    #[test]
    fn resize_and_wait_round_trip() {
        let resize = TerminalResize {
            handle: handle(),
            cols: 120,
            rows: 30,
        };
        assert_eq!(
            serde_json::from_value::<TerminalResize>(serde_json::to_value(&resize).unwrap())
                .unwrap(),
            resize
        );

        let wait = TerminalWait {
            handle: handle(),
            wait_for: WaitFor::Exit,
            timeout_ms: Some(30_000),
        };
        let json = serde_json::to_value(&wait).unwrap();
        assert_eq!(json["waitFor"], "exit");
        assert_eq!(json["timeoutMs"], 30_000);
        assert_eq!(serde_json::from_value::<TerminalWait>(json).unwrap(), wait);
    }

    #[test]
    fn a_wait_outcome_distinguishes_a_timeout_from_an_exit() {
        let timed_out = TerminalWaitResult {
            handle: handle(),
            outcome: WaitOutcome::TimedOut,
        };
        assert_eq!(
            serde_json::to_value(&timed_out).unwrap(),
            serde_json::json!({
                "handle": "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
                "outcome": "timed_out",
            })
        );
        assert_eq!(
            serde_json::to_value(TerminalWaitResult {
                handle: handle(),
                outcome: WaitOutcome::Exited {
                    status: ExitStatus::Exited { code: 0 }
                },
            })
            .unwrap(),
            serde_json::json!({
                "handle": "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
                "outcome": "exited",
                "status": { "outcome": "exited", "code": 0 },
            })
        );
        for outcome in [
            WaitOutcome::Exited {
                status: ExitStatus::Exited { code: 0 },
            },
            WaitOutcome::Idle,
            WaitOutcome::TimedOut,
        ] {
            let result = TerminalWaitResult {
                handle: handle(),
                outcome,
            };
            assert_eq!(
                serde_json::from_value::<TerminalWaitResult>(
                    serde_json::to_value(&result).unwrap()
                )
                .unwrap(),
                result
            );
        }
    }
}
