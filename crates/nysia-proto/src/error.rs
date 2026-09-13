//! The error envelope every failed verb answers with.
//!
//! §6.2 is emphatic about this and it is worth restating: **every error carries next
//! steps**, so an agent can recover instead of inventing flags. An agent that is told
//! "invalid argument" and nothing else does not stop; it guesses, and the guess is a
//! plausible-looking flag that does not exist. The cost of that is not one failed command,
//! it is a loop.
//!
//! So [`NextSteps`] is non-empty by construction, [`ErrorEnvelope`]'s only constructor
//! demands it, and its fields are private — an envelope without next steps is not a
//! degraded error, it is an unrepresentable one. The guarantee survives the wire too:
//! deserialising an empty `nextSteps` array fails, and the exported TypeScript type is
//! `[string, ...string[]]` rather than `string[]`.
//!
//! `nextCommandArgs` is the machine half of the same idea. Where there is one obvious
//! command to run next, the error carries its argv so the caller runs it rather than
//! reconstructing it from prose.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};
use ts_rs::TS;

/// Why an error envelope could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorEnvelopeError {
    /// `nextSteps` was absent, empty, or held nothing but blank strings.
    #[error("every error carries at least one non-blank next step (§6.2)")]
    NoNextSteps,
}

/// What went wrong, in a form a caller can branch on.
///
/// Open rather than closed. A client that meets a code a newer daemon added must still be
/// able to read the rest of the envelope — the message and the next steps are the parts
/// that actually help — so an unrecognised code lands in [`ErrorCode::Other`] instead of
/// failing the whole frame. An error that cannot be parsed is the worst possible place to
/// be strict.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(
    export,
    type = "\"unknown_session\" | \"invalid_request\" | \"unsupported\" | \"path_refused\" \
            | \"spawn_failed\" | \"session_busy\" | \"internal\" | (string & {})"
)]
pub enum ErrorCode {
    /// No session by that handle. Usually a handle from before a daemon restart.
    UnknownSession,
    /// The frame parsed as JSON but was not a valid request.
    InvalidRequest,
    /// A verb this daemon does not serve at the negotiated protocol version.
    Unsupported,
    /// A path was refused by `safe_join` / `path_confine` (§7.5).
    PathRefused,
    /// The PTY child could not be started.
    SpawnFailed,
    /// The session exists but cannot take this verb right now.
    SessionBusy,
    /// The daemon failed in a way the caller did nothing to cause.
    Internal,
    /// A code this build does not know, carried verbatim.
    Other(String),
}

impl ErrorCode {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::UnknownSession => "unknown_session",
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported => "unsupported",
            Self::PathRefused => "path_refused",
            Self::SpawnFailed => "spawn_failed",
            Self::SessionBusy => "session_busy",
            Self::Internal => "internal",
            Self::Other(code) => code,
        }
    }

    /// The code a wire spelling names, falling back to [`ErrorCode::Other`].
    ///
    /// Total by design: there is no "unparseable code", because refusing to read an error
    /// leaves the caller with less than the error would have given it.
    #[must_use]
    pub fn from_wire(code: &str) -> Self {
        match code {
            "unknown_session" => Self::UnknownSession,
            "invalid_request" => Self::InvalidRequest,
            "unsupported" => Self::Unsupported,
            "path_refused" => Self::PathRefused,
            "spawn_failed" => Self::SpawnFailed,
            "session_busy" => Self::SessionBusy,
            "internal" => Self::Internal,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&String::deserialize(deserializer)?))
    }
}

/// One or more things the caller can do about the error.
///
/// Non-empty by construction, and non-empty coming off the wire. The type exists so that
/// "I forgot the next steps" is a compile error rather than a runtime surprise at the one
/// moment when the caller most needed the help.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct NextSteps(Vec<String>);

impl NextSteps {
    /// The first step. There is no constructor that does not take one.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorEnvelopeError::NoNextSteps`] if `first` is blank — a whitespace-only
    /// step is an empty one wearing a disguise.
    pub fn new(first: impl Into<String>) -> Result<Self, ErrorEnvelopeError> {
        let first = first.into();
        if first.trim().is_empty() {
            return Err(ErrorEnvelopeError::NoNextSteps);
        }
        Ok(Self(vec![first]))
    }

    /// Add another step.
    ///
    /// A blank step is dropped rather than refused: the list is already non-empty, so
    /// there is nothing to protect, and failing a whole error frame over a stray empty
    /// string would be worse than the string.
    #[must_use]
    pub fn and(mut self, step: impl Into<String>) -> Self {
        let step = step.into();
        if !step.trim().is_empty() {
            self.0.push(step);
        }
        self
    }

    /// The steps, oldest first. Never empty.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NextSteps {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let steps = Vec::<String>::deserialize(deserializer)?;
        let steps: Vec<String> = steps
            .into_iter()
            .filter(|step| !step.trim().is_empty())
            .collect();
        if steps.is_empty() {
            return Err(serde::de::Error::custom(ErrorEnvelopeError::NoNextSteps));
        }
        Ok(Self(steps))
    }
}

/// What a failed verb answers with.
///
/// Built through [`ErrorEnvelope::new`], which takes the next steps as an argument rather
/// than leaving them to be filled in later. The fields are private for the same reason:
/// there is no order of operations in which an envelope exists without them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ErrorEnvelope {
    /// What went wrong, in a form a caller can branch on.
    code: ErrorCode,
    /// What went wrong, in a form a person can read. Never carries credentials or
    /// scrollback — trap 14.
    message: String,
    /// Whether retrying the identical request could succeed.
    retryable: bool,
    /// What to do about it. Never empty.
    #[ts(type = "[string, ...string[]]")]
    next_steps: NextSteps,
    /// The argv of the one obvious command to run next, when there is one.
    next_command_args: Option<Vec<String>>,
}

impl ErrorEnvelope {
    /// An error that says what to do about itself.
    ///
    /// Not retryable and with no follow-up command; add either with [`Self::retryable`] or
    /// [`Self::with_next_command_args`].
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, next_steps: NextSteps) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            next_steps,
            next_command_args: None,
        }
    }

    /// Mark the error as one a plain retry could clear.
    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Attach the argv of the command that would fix it.
    #[must_use]
    pub fn with_next_command_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.next_command_args = Some(args.into_iter().map(Into::into).collect());
        self
    }

    /// What went wrong, in a form a caller can branch on.
    #[must_use]
    pub fn code(&self) -> &ErrorCode {
        &self.code
    }

    /// What went wrong, in a form a person can read.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether retrying the identical request could succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// What to do about it. Never empty.
    #[must_use]
    pub fn next_steps(&self) -> &[String] {
        self.next_steps.as_slice()
    }

    /// The argv of the command that would fix it, if there is one.
    #[must_use]
    pub fn next_command_args(&self) -> Option<&[String]> {
        self.next_command_args.as_deref()
    }
}

impl fmt::Display for ErrorEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> ErrorEnvelope {
        ErrorEnvelope::new(
            ErrorCode::UnknownSession,
            "no session sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
            NextSteps::new("List the sessions the daemon currently owns.")
                .unwrap()
                .and("If the daemon restarted, the handle is stale; create a new session."),
        )
        .with_next_command_args(["nysia", "session", "list"])
    }

    #[test]
    fn an_envelope_carries_its_next_steps_to_the_wire_and_back() {
        let envelope = envelope();
        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["code"], "unknown_session");
        assert_eq!(json["nextSteps"].as_array().unwrap().len(), 2);
        assert_eq!(
            json["nextCommandArgs"],
            serde_json::json!(["nysia", "session", "list"])
        );
        assert_eq!(json["retryable"], false);
        assert_eq!(
            serde_json::from_value::<ErrorEnvelope>(json).unwrap(),
            envelope
        );
        assert_eq!(envelope.next_steps().len(), 2);
        assert_eq!(envelope.next_command_args().unwrap().len(), 3);
        assert_eq!(
            envelope.to_string(),
            "unknown_session: no session sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
        );
    }

    #[test]
    fn an_envelope_without_next_steps_cannot_be_read_off_the_wire() {
        // The one guarantee this module exists for. Rust's constructor already refuses it;
        // this is the half that a peer could otherwise get past.
        let empty = serde_json::json!({
            "code": "internal",
            "message": "something broke",
            "retryable": false,
            "nextSteps": [],
            "nextCommandArgs": null,
        });
        assert!(serde_json::from_value::<ErrorEnvelope>(empty).is_err());

        let blank = serde_json::json!({
            "code": "internal",
            "message": "something broke",
            "retryable": false,
            "nextSteps": ["", "   ", "\t"],
            "nextCommandArgs": null,
        });
        assert!(serde_json::from_value::<ErrorEnvelope>(blank).is_err());

        let missing = serde_json::json!({
            "code": "internal",
            "message": "something broke",
            "retryable": false,
            "nextCommandArgs": null,
        });
        assert!(serde_json::from_value::<ErrorEnvelope>(missing).is_err());
    }

    #[test]
    fn blank_steps_are_dropped_but_never_take_the_list_with_them() {
        assert!(NextSteps::new("").is_err());
        assert!(NextSteps::new("   ").is_err());
        let steps = NextSteps::new("Do the thing.").unwrap().and("").and("   ");
        assert_eq!(steps.as_slice(), ["Do the thing."]);

        let salvaged: NextSteps =
            serde_json::from_value(serde_json::json!(["", "Do the thing."])).unwrap();
        assert_eq!(salvaged.as_slice(), ["Do the thing."]);
    }

    #[test]
    fn an_unknown_code_is_carried_rather_than_rejected() {
        // A newer daemon's code must not cost the caller the message and the next steps.
        let from_a_newer_daemon = serde_json::json!({
            "code": "worktree_locked",
            "message": "the worktree is held by another session",
            "retryable": true,
            "nextSteps": ["Wait for the other session to finish."],
            "nextCommandArgs": null,
        });
        let envelope: ErrorEnvelope = serde_json::from_value(from_a_newer_daemon).unwrap();
        assert_eq!(
            envelope.code(),
            &ErrorCode::Other("worktree_locked".to_owned())
        );
        assert!(envelope.is_retryable());
        assert_eq!(envelope.next_steps().len(), 1);
        // And it goes back out exactly as it came in.
        assert_eq!(
            serde_json::to_value(&envelope).unwrap()["code"],
            "worktree_locked"
        );
    }

    #[test]
    fn every_known_code_round_trips_through_its_wire_spelling() {
        for code in [
            ErrorCode::UnknownSession,
            ErrorCode::InvalidRequest,
            ErrorCode::Unsupported,
            ErrorCode::PathRefused,
            ErrorCode::SpawnFailed,
            ErrorCode::SessionBusy,
            ErrorCode::Internal,
            ErrorCode::Other("worktree_locked".to_owned()),
        ] {
            assert_eq!(ErrorCode::from_wire(code.as_str()), code);
            let text = serde_json::to_string(&code).unwrap();
            assert_eq!(serde_json::from_str::<ErrorCode>(&text).unwrap(), code);
        }
    }

    #[test]
    fn retryable_is_opt_in() {
        let base = ErrorEnvelope::new(
            ErrorCode::SessionBusy,
            "a resize is already in flight",
            NextSteps::new("Try again in a moment.").unwrap(),
        );
        assert!(!base.is_retryable());
        assert!(base.clone().retryable(true).is_retryable());
        assert!(base.next_command_args().is_none());
    }
}
