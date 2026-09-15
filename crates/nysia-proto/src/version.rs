//! The protocol version, the range of daemons a client will attach to, and the endpoint
//! names derived from it.
//!
//! §3.1 makes the version part of the *name* rather than only part of the handshake: a
//! client that speaks a protocol the daemon does not understand never opens the socket in
//! the first place, because it is looking for a differently-named one. The handshake is
//! the second line of defence, for the case where the names match but the peers disagree
//! about what they can serve.
//!
//! Nothing here touches the filesystem. The helpers compose *names*; where those names
//! live — which directory on Unix, which account on Windows — is the daemon's business.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The protocol version a peer speaks.
///
/// A `u32` newtype rather than a bare integer, so a version cannot be silently passed
/// where a pid or a generation was meant. A serde newtype struct is already indistinguish-
/// able from its contents on the wire, so no `transparent` is needed — and ts-rs does not
/// understand that attribute anyway, warning instead of honouring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProtocolVersion(pub u32);

impl ProtocolVersion {
    /// The version as the bare number it is on the wire.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The protocol version this build speaks and advertises in the handshake.
///
/// Two, because v2 added [`FrameKind::ReplayEnd`](crate::FrameKind::ReplayEnd).
///
/// # When a new frame kind is breaking, and when it is not
///
/// A reader's decoder refuses a kind it does not know and drops the connection, by design.
/// So the question a new kind has to answer is **whether an older client can be sent one**,
/// and that turns on how its frames come to exist:
///
/// - `ReplayEnd` is **breaking**, which is why it cost a version. The daemon sends it on
///   every `stream_attach`, unbidden — a v1 client that attached anything would be sent a
///   kind its decoder treats as fatal, and would lose every session on that connection.
/// - [`FrameKind::AgentStatus`](crate::FrameKind::AgentStatus) is **additive**, and did not.
///   Its frames travel only on a stream id minted by
///   [`AgentStatusSubscribe`](crate::AgentStatusSubscribe) — a verb a client that does not
///   know the kind cannot send — so there is no sequence of requests by which an older
///   client receives one. A v0.1 client asking a v0.2 daemon for a verb it does not know
///   gets [`ErrorCode::Unsupported`](crate::ErrorCode::Unsupported) on the control
///   connection and nothing at all on the stream one.
///
/// The rule, then, is not "a new kind bumps the version". It is: **a new kind bumps the
/// version when the daemon can send it on a stream an older client could have opened.** A
/// daemon that emitted `AgentStatus` on a session stream would break that and silently make
/// this constant wrong, which is why the variant's own docs state the confinement rather
/// than leaving it here.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(2);

/// The oldest daemon protocol this build will attach to.
///
/// Orca is on `daemon-v36` and still advertises `attachableDaemonProtocolVersions: [1..36]`,
/// so a newer app adopts an older daemon rather than orphaning its sessions (§3.1). Widening
/// this range is what makes an in-place upgrade non-disruptive, and narrowing it is what
/// abandons old sessions **deliberately** rather than by accident.
///
/// This is a narrowing, and it is deliberate. §3.1 already keeps the two apart by *name* —
/// a v1 client dials `nysiad-v1` and cannot find a v2 daemon at all — so the only way a v1
/// hello reaches a v2 daemon is an explicit endpoint override. Serving it would mean sending
/// [`FrameKind::ReplayEnd`](crate::FrameKind::ReplayEnd) to a decoder that treats an unknown
/// kind as fatal, so the daemon refuses the handshake instead, and the client is told which
/// versions it could have spoken. A v0.1 daemon's sessions are orphaned by a v0.2 window;
/// the alternative was leaving every re-attach corrupting.
pub const MIN_ATTACHABLE_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(2);

/// An inclusive range of protocol versions a client will attach to.
///
/// This is the type that answers "may I talk to the daemon I just found?". A client that
/// finds a daemon outside its range must refuse rather than proceed, because a mismatched
/// frame layout corrupts sessions the daemon is still serving for someone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProtocolRange {
    /// The oldest version in the range, inclusive.
    pub min: ProtocolVersion,
    /// The newest version in the range, inclusive.
    pub max: ProtocolVersion,
}

impl ProtocolRange {
    /// The range this build will attach to.
    #[must_use]
    pub const fn attachable() -> Self {
        Self {
            min: MIN_ATTACHABLE_PROTOCOL_VERSION,
            max: PROTOCOL_VERSION,
        }
    }

    /// Whether `version` falls inside the range.
    ///
    /// An inverted range (`min > max`) contains nothing, which is the safe reading: a
    /// misconfigured range refuses every daemon rather than accepting every daemon.
    #[must_use]
    pub const fn contains(self, version: ProtocolVersion) -> bool {
        version.0 >= self.min.0 && version.0 <= self.max.0
    }
}

impl fmt::Display for ProtocolRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.min, self.max)
    }
}

/// Why a Windows pipe name could not be composed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    /// The account name was empty, or carried a character that cannot appear in a pipe
    /// name.
    #[error(
        "a pipe account name must be non-empty and free of separators and whitespace; a \
         `DOMAIN\\user` string must be reduced to its account half first, got {0:?}"
    )]
    AccountName(String),
    /// The composed name exceeded the Win32 ceiling.
    #[error("a pipe name is at most {max} characters, {actual} were composed")]
    PipeNameTooLong {
        /// The Win32 ceiling.
        max: usize,
        /// What composing produced.
        actual: usize,
    },
}

/// Everything after `\\.\pipe\`, and everything before `.sock`: `nysiad-v1`.
#[must_use]
pub fn endpoint_stem(version: ProtocolVersion) -> String {
    format!("nysiad-v{version}")
}

/// The Unix socket file name for `version`: `nysiad-v1.sock`.
///
/// The *name* only. The daemon decides the directory, because that is where the owner-only
/// permissions live and this crate does no IO.
#[must_use]
pub fn unix_socket_file_name(version: ProtocolVersion) -> String {
    format!("{}.sock", endpoint_stem(version))
}

/// `\\.\pipe\` — every Win32 named pipe on the local machine begins here.
const WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\";

/// Win32 caps a pipe name at 256 characters, prefix included.
const WINDOWS_PIPE_NAME_MAX: usize = 256;

/// The Windows named-pipe path for `version` and `account`: `\\.\pipe\nysiad-v1-<user>`.
///
/// The account is part of the name because two users signed into the same machine each get
/// their own daemon; the owner-only ACL on the pipe is what keeps them apart, and a shared
/// name would have them fighting over one endpoint.
///
/// # Errors
///
/// Returns [`EndpointError::AccountName`] if `account` is empty or carries a separator,
/// whitespace or a control character — a `\` in particular, because `GetUserNameEx` can
/// hand back `DOMAIN\user` and splicing that in would silently name a pipe in a different
/// namespace. Returns [`EndpointError::PipeNameTooLong`] if the composed name exceeds the
/// Win32 ceiling.
pub fn windows_pipe_name(version: ProtocolVersion, account: &str) -> Result<String, EndpointError> {
    let unusable = account.is_empty()
        || account
            .chars()
            .any(|c| c == '\\' || c == '/' || c.is_control() || c.is_whitespace());
    if unusable {
        return Err(EndpointError::AccountName(account.to_owned()));
    }
    let name = format!("{WINDOWS_PIPE_PREFIX}{}-{account}", endpoint_stem(version));
    let length = name.chars().count();
    if length > WINDOWS_PIPE_NAME_MAX {
        return Err(EndpointError::PipeNameTooLong {
            max: WINDOWS_PIPE_NAME_MAX,
            actual: length,
        });
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_range_admits_the_shipped_version() {
        let range = ProtocolRange::attachable();
        assert!(range.contains(PROTOCOL_VERSION));
        assert!(range.contains(MIN_ATTACHABLE_PROTOCOL_VERSION));
        assert_eq!(range.to_string(), "2..=2");
    }

    #[test]
    fn a_range_refuses_everything_outside_it() {
        let range = ProtocolRange {
            min: ProtocolVersion(2),
            max: ProtocolVersion(4),
        };
        assert!(!range.contains(ProtocolVersion(1)));
        assert!(range.contains(ProtocolVersion(2)));
        assert!(range.contains(ProtocolVersion(4)));
        assert!(!range.contains(ProtocolVersion(5)));
    }

    #[test]
    fn an_inverted_range_contains_nothing() {
        let inverted = ProtocolRange {
            min: ProtocolVersion(4),
            max: ProtocolVersion(2),
        };
        for v in 0..8 {
            assert!(!inverted.contains(ProtocolVersion(v)));
        }
    }

    #[test]
    fn endpoint_names_carry_the_version() {
        assert_eq!(endpoint_stem(ProtocolVersion(1)), "nysiad-v1");
        assert_eq!(unix_socket_file_name(ProtocolVersion(1)), "nysiad-v1.sock");
        assert_eq!(
            windows_pipe_name(ProtocolVersion(1), "kacpe").unwrap(),
            r"\\.\pipe\nysiad-v1-kacpe"
        );
        // A bump changes the name, which is the whole mechanism — and it is the mechanism
        // `FrameKind::ReplayEnd` leans on, because a v1 decoder drops the connection over a
        // kind byte it does not know. Asserted against the shipped version rather than a
        // literal, so the next bump moves this with it rather than rotting.
        assert_eq!(
            unix_socket_file_name(PROTOCOL_VERSION),
            format!("nysiad-v{PROTOCOL_VERSION}.sock")
        );
        assert_ne!(
            windows_pipe_name(ProtocolVersion(2), "kacpe").unwrap(),
            windows_pipe_name(ProtocolVersion(1), "kacpe").unwrap()
        );
    }

    #[test]
    fn a_pipe_account_that_would_escape_the_namespace_is_refused() {
        for account in [
            "",
            "CORP\\kacpe",
            "kac/pe",
            "kac pe",
            "kac\tpe",
            "kac\u{0}pe",
        ] {
            assert!(
                windows_pipe_name(PROTOCOL_VERSION, account).is_err(),
                "{account:?} should not compose a pipe name"
            );
        }
    }

    #[test]
    fn an_overlong_pipe_name_is_refused_rather_than_truncated() {
        let long = "a".repeat(WINDOWS_PIPE_NAME_MAX);
        assert_eq!(
            windows_pipe_name(PROTOCOL_VERSION, &long),
            Err(EndpointError::PipeNameTooLong {
                max: WINDOWS_PIPE_NAME_MAX,
                actual: WINDOWS_PIPE_PREFIX.len()
                    + endpoint_stem(PROTOCOL_VERSION).len()
                    + "-".len()
                    + long.len(),
            })
        );
    }

    #[test]
    fn a_version_is_a_bare_number_on_the_wire() {
        assert_eq!(serde_json::to_string(&PROTOCOL_VERSION).unwrap(), "2");
        assert_eq!(
            serde_json::from_str::<ProtocolVersion>("7").unwrap(),
            ProtocolVersion(7)
        );
        assert_eq!(
            serde_json::to_string(&ProtocolRange::attachable()).unwrap(),
            "{\"min\":2,\"max\":2}"
        );
    }
}
