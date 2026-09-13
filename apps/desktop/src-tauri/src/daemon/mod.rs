//! The window's client of the `nysiad` socket.
//!
//! Under D-1 and D-2 this process is a **client and nothing else**. The daemon owns the
//! PTYs, the terminal state, the store, git and orchestration; killing this window must
//! never interrupt a running session. So the rule for everything below: if it holds state
//! the daemon should own, it is a bug.
//!
//! Two connections, because [`ClientRole`](nysia_proto::handshake::ClientRole) has two
//! values and they have opposite shapes:
//!
//! - [`control`] — request/response NDJSON, served in order. Short verbs.
//! - [`stream`] — a firehose of length-prefixed binary frames under a credit window.
//!
//! Multiplexing them onto one socket would let a stalled terminal read delay a session
//! close, which is exactly what the split exists to prevent.
//!
//! ## Why this lives here and not in `nysia-core`
//!
//! `nysia-core::rpc` is the daemon's *server*, owned by W4. A client of it is a different
//! thing, and the second client — W4's own CLI — does not exist yet. D-4's rule applies:
//! a seam extracted from one implementation encodes that implementation's assumptions and
//! calls them universal. When both clients exist, someone lifts what they genuinely share
//! into `nysia-core` from two real cases. Until then this is written as if it is the only
//! one, because it is.

pub mod control;
pub mod endpoint;
pub mod stream;

use nysia_proto::error::ErrorEnvelope;
use serde::Serialize;

/// Why talking to the daemon failed.
///
/// The variants are split by what the *user* can do about them, not by where in the stack
/// they arose. `Unreachable` means start the daemon; `Refused` means this build and that
/// daemon cannot work together; `Daemon` carries the daemon's own envelope, which already
/// contains a sentence a person can act on. Collapsing them into one string would make the
/// window's notice generic exactly when it most needs to be specific.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DaemonError {
    /// The endpoint could not be composed for this account.
    #[error("the daemon endpoint could not be named: {0}")]
    Endpoint(String),
    /// Nothing is listening.
    ///
    /// The ordinary case on a machine where the daemon has not been started, not a fault.
    #[error("no daemon is listening on {endpoint}: {cause}")]
    Unreachable {
        /// The socket that was tried.
        endpoint: String,
        /// What the OS said.
        cause: String,
    },
    /// The daemon declined the handshake, or speaks a protocol this build cannot attach to.
    ///
    /// `retryable` comes off the wire rather than being inferred from the reason: a daemon
    /// that is `shutting_down` will be replaced by a fresh one in a moment, and a client
    /// that gave up on it would need the user to restart the window for nothing. Proto
    /// decides, because proto owns the reason taxonomy.
    #[error("the daemon refused this client: {reason}")]
    Refused {
        /// What to tell the user.
        reason: String,
        /// Whether another attempt could reach a daemon that says yes.
        retryable: bool,
    },
    /// The socket died.
    #[error("the connection to the daemon failed: {0}")]
    Io(String),
    /// Something arrived that is not the protocol.
    #[error("the daemon sent something this build cannot read: {0}")]
    Protocol(String),
    /// The client is not connected, or its worker has stopped.
    #[error("the window is not connected to a daemon")]
    Disconnected,
    /// The daemon answered with an error envelope.
    ///
    /// Boxed because [`ErrorEnvelope`] is much larger than every other variant, and an
    /// enum is as big as its largest member — an unboxed envelope would make every
    /// `Result` in the client pay for the rarest case.
    #[error("{}", .0.message())]
    Daemon(Box<ErrorEnvelope>),
}

impl DaemonError {
    /// Whether trying again could plausibly work.
    ///
    /// The window uses this to decide between reconnecting and giving up, so it is
    /// deliberately conservative: a protocol mismatch never becomes retryable by waiting,
    /// and pretending otherwise would spin forever against a daemon that will never agree.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unreachable { .. } | Self::Io(_) | Self::Disconnected => true,
            Self::Refused { retryable, .. } => *retryable,
            Self::Daemon(envelope) => envelope.is_retryable(),
            Self::Endpoint(_) | Self::Protocol(_) => false,
        }
    }

    /// What to put in front of the user.
    ///
    /// The daemon's envelope already carries `nextSteps` — a sentence a person can act on
    /// rather than a status code — so when there is one it is used verbatim. The rest get
    /// a next step composed here, because a notice that says only what broke and not what
    /// to do is the notice users learn to dismiss unread.
    pub fn next_steps(&self) -> Vec<String> {
        match self {
            Self::Daemon(envelope) => envelope.next_steps().to_vec(),
            Self::Unreachable { .. } | Self::Disconnected => {
                vec!["Start the Nysia daemon, then try again.".to_owned()]
            }
            Self::Io(_) => {
                vec!["The daemon went away. Nysia will reconnect on its own.".to_owned()]
            }
            Self::Refused { .. } | Self::Protocol(_) => {
                vec![
                    "This build and the running daemon disagree about the protocol. \
                      Restart the daemon so both are the same version."
                        .to_owned(),
                ]
            }
            Self::Endpoint(_) => {
                vec!["Nysia could not work out where the daemon's socket should be.".to_owned()]
            }
        }
    }
}

/// A failure as the webview sees it.
///
/// Every Tauri command returns this rather than a bare string, because the store turns it
/// into a `StoreCommandError` — the one rejection type `runCommand` surfaces to the user.
/// Anything else reaches `console.error` and the user sees nothing at all, which is exactly
/// the silent failure the store's own error docs exist to prevent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandFailure {
    /// A sentence, shown verbatim.
    pub message: String,
    /// What to do about it. Never empty.
    pub next_steps: Vec<String>,
    /// Whether the window should reconnect rather than give up.
    pub retryable: bool,
}

impl From<DaemonError> for CommandFailure {
    fn from(error: DaemonError) -> Self {
        Self {
            message: error.to_string(),
            next_steps: error.next_steps(),
            retryable: error.retryable(),
        }
    }
}

#[cfg(test)]
mod tests {
    use nysia_proto::error::{ErrorCode, NextSteps};

    use super::*;

    #[test]
    fn a_missing_daemon_is_retryable_and_a_protocol_mismatch_is_not() {
        // Waiting never resolves a disagreement about the wire format, so a client that
        // called it retryable would reconnect forever against a daemon that will never
        // agree.
        assert!(
            DaemonError::Unreachable {
                endpoint: "pipe".to_owned(),
                cause: "not found".to_owned(),
            }
            .retryable()
        );
        assert!(DaemonError::Io("reset".to_owned()).retryable());
        assert!(DaemonError::Disconnected.retryable());
        assert!(
            !DaemonError::Refused {
                reason: "this daemon speaks v9".to_owned(),
                retryable: false,
            }
            .retryable()
        );
        assert!(
            DaemonError::Refused {
                reason: "the daemon is retiring".to_owned(),
                retryable: true,
            }
            .retryable(),
            "a retiring daemon is replaced in a moment; giving up on it helps nobody"
        );
        assert!(!DaemonError::Protocol("garbage".to_owned()).retryable());
        assert!(!DaemonError::Endpoint("no account".to_owned()).retryable());
    }

    #[test]
    fn the_daemons_own_verdict_on_retrying_is_the_one_that_counts() {
        let envelope = |retryable| {
            DaemonError::Daemon(Box::new(
                ErrorEnvelope::new(
                    ErrorCode::SessionBusy,
                    "that session is mid-resize",
                    NextSteps::new("Try again in a moment.").expect("a non-empty step"),
                )
                .retryable(retryable),
            ))
        };
        assert!(envelope(true).retryable());
        assert!(!envelope(false).retryable());
    }

    #[test]
    fn every_failure_tells_the_user_what_to_do_next() {
        // A notice that says only what broke is the notice users learn to dismiss unread.
        let failures = [
            DaemonError::Endpoint("no account".to_owned()),
            DaemonError::Unreachable {
                endpoint: "pipe".to_owned(),
                cause: "not found".to_owned(),
            },
            DaemonError::Refused {
                reason: "this daemon speaks v9".to_owned(),
                retryable: false,
            },
            DaemonError::Io("reset".to_owned()),
            DaemonError::Protocol("garbage".to_owned()),
            DaemonError::Disconnected,
            DaemonError::Daemon(Box::new(ErrorEnvelope::new(
                ErrorCode::SpawnFailed,
                "pwsh is not on PATH",
                NextSteps::new("Install PowerShell 7.").expect("a non-empty step"),
            ))),
        ];

        for failure in failures {
            let surfaced = CommandFailure::from(failure.clone());
            assert!(!surfaced.message.is_empty(), "{failure:?} has no message");
            assert!(
                !surfaced.next_steps.is_empty(),
                "{failure:?} has no next step"
            );
            assert!(
                surfaced.next_steps.iter().all(|step| !step.is_empty()),
                "{failure:?} has an empty next step"
            );
        }
    }

    #[test]
    fn the_daemons_next_steps_survive_the_trip_to_the_webview() {
        let surfaced = CommandFailure::from(DaemonError::Daemon(Box::new(ErrorEnvelope::new(
            ErrorCode::PathRefused,
            "that path is outside the project",
            NextSteps::new("Pick a directory inside the repository.").expect("a non-empty step"),
        ))));
        assert_eq!(
            surfaced.next_steps,
            vec!["Pick a directory inside the repository.".to_owned()]
        );
        assert_eq!(surfaced.message, "that path is outside the project");
    }
}
