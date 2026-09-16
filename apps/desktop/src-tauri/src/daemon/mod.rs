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
    /// There was no daemon, and the window could not start one.
    ///
    /// The window ships the `nysia` runtime beside itself and starts it on a first launch
    /// (§12 q5), so this is what is left when that cannot happen: the sidecar is missing from
    /// the bundle, or the file will not execute. **Never retryable**, and that is the point of
    /// having a variant of its own — neither changes by waiting, and a window that kept
    /// reconnecting against a runtime nobody can start would show *Reconnecting* for ever
    /// without once saying why.
    ///
    /// A runtime that *ran* and has not answered is [`Self::Starting`], not this. It is the
    /// one case here that waiting does fix, and the two must not be confused: this variant
    /// tells the user the window has stopped.
    #[error("{message}")]
    Spawn {
        /// What went wrong, as a sentence.
        message: String,
        /// What to do about it. Never blank.
        next_step: String,
    },
    /// The window started its runtime and it has not answered **yet**.
    ///
    /// The sibling of [`Self::Spawn`] and its exact opposite on the only question that
    /// matters: this one is **retryable**. A readiness wait is a bound on one call, not a
    /// verdict on the daemon — Defender scanning a binary it has never seen pushes a first
    /// bind past twenty seconds on precisely the machine where a first launch happens — and a
    /// window that called it permanent told the user to reopen the app beside a daemon that
    /// came up five seconds later and worked. Its own variant rather than [`Self::Io`] so the
    /// sentence can say what is being waited for and name the log that would explain a wait
    /// that never ends.
    #[error("{message}")]
    Starting {
        /// What is happening, as a sentence.
        message: String,
        /// What to do about it — including that the window has *not* stopped. Never blank.
        next_step: String,
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
    /// This failure as a word, for a log line that must not carry the message.
    ///
    /// Every variant here renders a **sentence for the user**, and a sentence is built from
    /// whatever the failure was about: a `cwd` the session asked for, the program a shell
    /// spawn could not find, an endpoint path. None of that belongs in a file (trap 13, and
    /// the rule in `nysia_core::rpc::log_file`), and "log the message, it is only an error"
    /// is exactly how a payload ends up in one.
    ///
    /// So a log line gets this instead: a fixed word per variant, plus the daemon's own
    /// [`nysia_proto::ErrorCode`] where there is one. It is enough to tell a refused
    /// handshake from a dead socket from a shell that would not start, which is what a reader
    /// of the log is asking. The sentence still reaches the user on screen, where it is
    /// wanted and where it is not written down.
    ///
    /// [`nysia_proto::ErrorCode::Other`] is rendered as `other` rather than passed through.
    /// It is the one open end of proto's otherwise closed taxonomy — a code from a daemon
    /// newer than this build, carried verbatim and of unbounded length — and a field that
    /// writes whatever the far end sent is not a closed set however unlikely the far end is
    /// to abuse it.
    pub fn kind(&self) -> &str {
        match self {
            Self::Endpoint(_) => "endpoint",
            Self::Unreachable { .. } => "unreachable",
            Self::Refused { .. } => "refused",
            Self::Spawn { .. } => "spawn",
            Self::Starting { .. } => "starting",
            Self::Io(_) => "io",
            Self::Protocol(_) => "protocol",
            Self::Disconnected => "disconnected",
            Self::Daemon(envelope) => match envelope.code() {
                nysia_proto::ErrorCode::Other(_) => "other",
                known => known.as_str(),
            },
        }
    }

    /// Whether trying again could plausibly work.
    ///
    /// The window uses this to decide between reconnecting and giving up, so it is
    /// deliberately conservative: a protocol mismatch never becomes retryable by waiting,
    /// and pretending otherwise would spin forever against a daemon that will never agree.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unreachable { .. } | Self::Io(_) | Self::Disconnected | Self::Starting { .. } => {
                true
            }
            Self::Refused { retryable, .. } => *retryable,
            Self::Daemon(envelope) => envelope.is_retryable(),
            Self::Endpoint(_) | Self::Protocol(_) | Self::Spawn { .. } => false,
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
            // Composed where the failure happened, because only that caller knows which path
            // was tried and which log would say why.
            Self::Spawn { next_step, .. } | Self::Starting { next_step, .. } => {
                vec![next_step.clone()]
            }
        }
    }
}

/// A failure as the webview sees it.
///
/// Every Tauri command returns this rather than a bare string, because the store turns it
/// into a `StoreCommandError` — the rejection type the webview's command router treats as
/// an expected outcome already on screen. Anything else it classes as a provider bug and
/// routes to a separate unexpected-failure channel, which is not where "the daemon went
/// away" belongs.
///
/// `next_steps` is never empty. A notice that says only what broke is the notice users
/// learn to dismiss unread.
///
/// # Why there is a `kind` beside the sentence
///
/// `message` and `next_steps` are for a person; `kind` is for the code that has to *do*
/// something different depending on which failure this is. Registering a folder has four
/// answers a user must be able to tell apart — it is not a repository, it holds several,
/// it could not be read, and it worked (v0.3 plan §3.2) — and a window that could only read
/// the sentence would have to match on English to distinguish them. That is the kind of
/// coupling a wire `ErrorCode` exists to remove, and it was being thrown away here.
///
/// It is [`DaemonError::kind`] verbatim rather than a second vocabulary, so it is the same
/// closed set the log already writes and carries the same guarantee: an
/// [`ErrorCode::Other`](nysia_proto::ErrorCode::Other) from a daemon newer than this build
/// is rendered `other` rather than passed through, and a caller that does not recognise a
/// kind still has the sentence and the next steps, which are the parts that help.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandFailure {
    /// What went wrong, as one word from a closed set. See the type docs.
    pub kind: String,
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
            kind: error.kind().to_owned(),
            message: error.to_string(),
            next_steps: error.next_steps(),
            retryable: error.retryable(),
        }
    }
}

#[cfg(test)]
mod tests {
    use nysia_proto::error::{ErrorCode, NextSteps};
    use nysia_proto::project::RegisterRefusal;

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
            DaemonError::Spawn {
                message: "the Nysia runtime is not installed beside the app".to_owned(),
                next_step: "Reinstall Nysia.".to_owned(),
            },
            DaemonError::Starting {
                message: "Nysia started its runtime and it has not answered yet".to_owned(),
                next_step: "Nysia is still trying.".to_owned(),
            },
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
    fn a_runtime_that_cannot_be_started_is_never_retryable() {
        // The defect this variant closes: a window that treats "there is no runtime to start"
        // as something waiting could fix shows *Reconnecting* for ever and never says why.
        let failure = DaemonError::Spawn {
            message: "the Nysia runtime is not installed beside the app".to_owned(),
            next_step: "Reinstall Nysia.".to_owned(),
        };
        assert!(!failure.retryable());

        let surfaced = CommandFailure::from(failure);
        assert_eq!(surfaced.next_steps, vec!["Reinstall Nysia.".to_owned()]);
        assert!(
            surfaced.message.contains("not installed beside the app"),
            "the sentence the caller composed must survive, got {:?}",
            surfaced.message
        );
    }

    /// The pair that has to disagree: one runtime that cannot be started, one that has not
    /// finished starting.
    ///
    /// Asserted together because the whole risk is that they drift into saying the same
    /// thing. A readiness wait is a bound on one call — Defender scanning a binary it has
    /// never seen is exactly a first launch — so a window that reported it as permanent
    /// stopped beside a daemon that bound five seconds later and worked, and told the user to
    /// reopen an app that did not need reopening.
    #[test]
    fn a_runtime_that_has_not_answered_yet_is_retryable_and_a_missing_one_is_not() {
        let missing = DaemonError::Spawn {
            message: "the Nysia runtime is not installed beside the app".to_owned(),
            next_step: "Reinstall Nysia.".to_owned(),
        };
        let starting = DaemonError::Starting {
            message: "Nysia started its runtime and it has not answered yet".to_owned(),
            next_step: "Nysia is still trying.".to_owned(),
        };

        assert!(
            !missing.retryable(),
            "reinstalling is the fix; waiting is not"
        );
        assert!(
            starting.retryable(),
            "waiting is the whole of the fix; giving up strands a daemon that is on its way"
        );
        assert_eq!(
            CommandFailure::from(starting).next_steps,
            vec!["Nysia is still trying.".to_owned()],
            "the caller composes this sentence, because only it knows what is being awaited"
        );
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

    #[test]
    fn the_three_registration_refusals_reach_the_webview_as_three_kinds() {
        // §3.2's whole point: "could not register" is not an answer a user can act on. The
        // window paints a different panel for each of these, and this is the field it
        // branches on — so a `kind` that collapsed them would be invisible in Rust and
        // visible to every user as one generic failure.
        let kinds: Vec<String> = [
            RegisterRefusal::NotARepository,
            RegisterRefusal::ManyRepositories {
                found: vec!["nysia".to_owned(), "orca".to_owned()],
            },
            RegisterRefusal::Unreadable,
        ]
        .into_iter()
        .map(|refusal| {
            CommandFailure::from(DaemonError::Daemon(Box::new(refusal.into_envelope()))).kind
        })
        .collect();

        assert_eq!(
            kinds,
            vec![
                "not_a_repository".to_owned(),
                "many_repositories".to_owned(),
                "path_unreadable".to_owned()
            ]
        );
    }

    #[test]
    fn a_code_this_build_does_not_know_is_not_passed_through() {
        // The one open end of proto's taxonomy, and the reason `kind` is `DaemonError::kind`
        // rather than the code's own string: a newer daemon's code is arbitrary text of
        // unbounded length, and a field that writes back whatever the far end sent is not a
        // closed set however unlikely the far end is to abuse it.
        let surfaced = CommandFailure::from(DaemonError::Daemon(Box::new(ErrorEnvelope::new(
            ErrorCode::Other("a_verb_from_2027".to_owned()),
            "this daemon knows something newer",
            NextSteps::new("Update Nysia.").expect("a non-empty step"),
        ))));
        assert_eq!(surfaced.kind, "other");
        // The parts that still help are untouched, which is what makes an unknown kind
        // survivable rather than fatal.
        assert_eq!(surfaced.message, "this daemon knows something newer");
        assert_eq!(surfaced.next_steps, vec!["Update Nysia.".to_owned()]);
    }
}
