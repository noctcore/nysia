//! The first frame on every connection, and the pid record that lets a restarted app
//! recognise the daemon it found.
//!
//! §3.1 puts a `hello` at the head of every connection, before any other frame. Two things
//! come out of it:
//!
//! 1. **Compatibility.** The endpoint name already carries the protocol version, so a
//!    wholly incompatible client never finds the socket. The handshake closes the
//!    remaining gap — the peers found each other but disagree about what they can serve —
//!    and a rejection says whether to back off or to die.
//! 2. **Identity.** The daemon answers with its [`DaemonIdentity`], which the client
//!    compares against the [`PidRecord`] it read beside the socket. A pid alone is not
//!    enough: pids are reused, and a client that attaches to whatever now holds the old pid
//!    is exactly the failure the launch nonce exists to prevent.
//!
//! There is no `token` field. §3.1 sketched one, and §3.2 replaced it: the daemon learns
//! the caller's pid from the kernel (`SO_PEERCRED` / `GetNamedPipeClientProcessId`) and
//! walks it up to a session it spawned. A token in the frame would be a second, weaker
//! authority that an attacker can present and the kernel cannot.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize, Serializer};
use ts_rs::TS;

use crate::newtype::deserialize_via_from_str;
use crate::version::{ProtocolRange, ProtocolVersion};

/// Why a handshake value could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    /// A client id was empty, too long, or carried whitespace or a control character.
    #[error(
        "a client id is 1..={max} characters with no whitespace or control characters, \
         got {value:?}"
    )]
    ClientId {
        /// The ceiling a client id may not exceed.
        max: usize,
        /// The offending value.
        value: String,
    },
    /// A launch nonce was not a bare hyphenated uuid.
    #[error("a launch nonce is a hyphenated uuid, got {0:?}")]
    LaunchNonceShape(String),
}

/// A client id may not exceed this, so a hostile peer cannot make the daemon's logs
/// unreadable with one frame.
const CLIENT_ID_MAX: usize = 128;

/// Who a client says it is, for logging and for the idle-retire check.
///
/// Deliberately *not* an authority: §3.2 proves identity from peer credentials and the PTY
/// process tree, never from something the peer typed. This id is how a line in the daemon
/// log says "the window" rather than "connection 4", and how `shutdownIfIdle` recognises
/// that the caller is the sole client.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct ClientId(String);

impl ClientId {
    /// The id as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ClientId {
    type Err = HandshakeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let unusable = s.is_empty()
            || s.chars().count() > CLIENT_ID_MAX
            || s.chars().any(|c| c.is_whitespace() || c.is_control());
        if unusable {
            return Err(HandshakeError::ClientId {
                max: CLIENT_ID_MAX,
                value: s.to_owned(),
            });
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

deserialize_via_from_str!(ClientId);

/// A fresh random value minted once per daemon start.
///
/// This is what makes the adoption lease work. `pid` plus `startedAtMs` narrows the field;
/// the nonce closes it, because two daemons cannot mint the same one even if the operating
/// system hands out the same pid in the same millisecond after a crash-restart loop.
///
/// A bare hyphenated uuid, with the other uuid spellings refused, so string equality is a
/// sound test for "the same daemon".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct LaunchNonce(String);

impl LaunchNonce {
    /// Mint a nonce for a starting daemon.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().as_hyphenated().to_string())
    }

    /// The nonce as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for LaunchNonce {
    type Err = HandshakeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let shape = || HandshakeError::LaunchNonceShape(s.to_owned());
        let parsed = uuid::Uuid::try_parse(s).map_err(|_| shape())?;
        // `uuid` also accepts the braced, urn and simple spellings. Refuse them, so one
        // daemon has exactly one nonce spelling and `==` means what it looks like.
        if parsed.as_hyphenated().to_string() != s {
            return Err(shape());
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for LaunchNonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

deserialize_via_from_str!(LaunchNonce);

/// Which half of the protocol a connection carries.
///
/// Two roles rather than one connection doing both, because the streams have opposite
/// shapes: control is request/response NDJSON where a slow reply blocks the next request,
/// and stream is a firehose of length-prefixed binary frames under a credit window (§7.3).
/// Multiplexing them onto one socket would let a stalled terminal read delay a session
/// close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ClientRole {
    /// Request/response verbs as newline-delimited JSON.
    Control,
    /// Length-prefixed binary terminal output, under a credit window.
    Stream,
}

impl ClientRole {
    /// The wire spelling, which is also what this type exports to TypeScript.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Stream => "stream",
        }
    }
}

impl fmt::Display for ClientRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A wire field that is always the literal `"hello"`.
///
/// A real field rather than serde's struct-level `#[serde(tag = …)]`, which *writes* the
/// tag but does not *require* it: a frame with the tag missing still deserialises into the
/// struct. For the one frame that has to be recognised before anything else is agreed, the
/// tag has to be checked, so it is modelled the same way [`OkTrue`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HelloTag;

impl Serialize for HelloTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(HELLO_TAG)
    }
}

impl<'de> Deserialize<'de> for HelloTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == HELLO_TAG {
            Ok(Self)
        } else {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&raw),
                &HELLO_TAG,
            ))
        }
    }
}

/// The one value [`HelloTag`] takes.
const HELLO_TAG: &str = "hello";

/// The client's first frame: `{"type":"hello", …}`.
///
/// The `type` tag is part of the struct rather than of an enclosing envelope, because this
/// frame precedes the envelope machinery — a daemon reading it has not yet agreed with the
/// peer on what an envelope looks like.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HelloRequest {
    /// Always `"hello"`. Checked on the way in, not merely written on the way out.
    #[serde(rename = "type")]
    #[ts(rename = "type", type = "\"hello\"")]
    pub tag: HelloTag,
    /// The protocol version the client speaks.
    pub version: ProtocolVersion,
    /// Which half of the protocol this connection carries.
    pub role: ClientRole,
    /// Who the client says it is. Not an authority; see the module docs.
    pub client_id: ClientId,
}

impl HelloRequest {
    /// Compose the first frame of a connection.
    #[must_use]
    pub fn new(version: ProtocolVersion, role: ClientRole, client_id: ClientId) -> Self {
        Self {
            tag: HelloTag,
            version,
            role,
            client_id,
        }
    }
}

/// A wire field that is always the literal `true`.
///
/// It exists so [`HelloAccepted`] cannot be built with `ok: false`, and so the TypeScript
/// union narrows on `ok` rather than on which optional field happens to be present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OkTrue;

/// A wire field that is always the literal `false`. The counterpart to [`OkTrue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OkFalse;

impl Serialize for OkTrue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl Serialize for OkFalse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for OkTrue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Bool(false),
                &"true",
            ))
        }
    }
}

impl<'de> Deserialize<'de> for OkFalse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Bool(true),
                &"false",
            ))
        } else {
            Ok(Self)
        }
    }
}

/// Who the daemon is, answered in the handshake.
///
/// The three lease fields and the version the daemon was built from. D-11 lets the GUI and
/// the daemon run different versions — that is what an in-place upgrade requires — so
/// `appVersion` is reported rather than asserted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DaemonIdentity {
    /// The daemon's process id.
    pub pid: u32,
    /// Unix milliseconds at which the daemon started.
    pub started_at_ms: u64,
    /// The random value minted at that start.
    pub launch_nonce: LaunchNonce,
    /// The Nysia version the daemon was built from, e.g. `0.1.0`.
    pub app_version: String,
}

impl DaemonIdentity {
    /// The lease this identity writes beside the socket.
    #[must_use]
    pub fn pid_record(&self) -> PidRecord {
        PidRecord {
            pid: self.pid,
            started_at_ms: self.started_at_ms,
            launch_nonce: self.launch_nonce.clone(),
        }
    }
}

/// The adoption lease written beside the socket (§3.1).
///
/// A restarted app reads this file, finds a daemon, and has to answer one question: *is the
/// thing now listening the daemon this record describes?* The handshake's
/// [`DaemonIdentity`] is the other half of that comparison — see [`PidRecord::describes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PidRecord {
    /// The pid the record was written for.
    pub pid: u32,
    /// Unix milliseconds at which that process started.
    pub started_at_ms: u64,
    /// The random value minted at that start.
    pub launch_nonce: LaunchNonce,
}

impl PidRecord {
    /// Whether `identity` is the daemon this record was written for.
    ///
    /// All three fields must agree. The nonce alone would very nearly do, but comparing the
    /// pid and the start time as well means a record that was hand-edited, half-written, or
    /// copied between machines fails the check rather than passing it on one lucky field.
    #[must_use]
    pub fn describes(&self, identity: &DaemonIdentity) -> bool {
        self.pid == identity.pid
            && self.started_at_ms == identity.started_at_ms
            && self.launch_nonce == identity.launch_nonce
    }
}

/// The daemon's answer to an accepted `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HelloAccepted {
    /// Always `true`. What the TypeScript union narrows on.
    #[ts(type = "true")]
    pub ok: OkTrue,
    /// Who answered.
    pub daemon_identity: DaemonIdentity,
}

impl HelloAccepted {
    /// Accept a `hello` on behalf of `identity`.
    #[must_use]
    pub fn new(daemon_identity: DaemonIdentity) -> Self {
        Self {
            ok: OkTrue,
            daemon_identity,
        }
    }
}

/// Why a `hello` was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum RejectReason {
    /// The client's version is outside what this daemon serves. The daemon reports both
    /// halves so the client can say *which* of them has to change.
    #[serde(rename_all = "camelCase")]
    UnsupportedVersion {
        /// The version the daemon itself speaks.
        daemon: ProtocolVersion,
        /// The range the daemon will serve.
        attachable: ProtocolRange,
    },
    /// Peer credentials did not resolve to a caller this daemon serves (§3.2).
    #[serde(rename_all = "camelCase")]
    Unauthorized {
        /// What to tell the operator. Never the credentials themselves.
        detail: String,
    },
    /// The daemon is retiring and will not take new connections.
    ShuttingDown,
    /// The first frame was not a readable `hello`.
    #[serde(rename_all = "camelCase")]
    Malformed {
        /// What could not be read.
        detail: String,
    },
}

impl RejectReason {
    /// Whether a client that saw this should back off and try again, or give up.
    ///
    /// Only [`ShuttingDown`](RejectReason::ShuttingDown) is retryable: the daemon is on its
    /// way out and the next connection attempt reaches a fresh one. The other three are
    /// properties of the client, and retrying reproduces them exactly.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::ShuttingDown)
    }
}

/// The daemon's answer to a refused `hello`.
///
/// Build it with [`HelloRejected::new`], which derives `retryable` from the reason. The
/// field exists on the wire because the client should not have to know the reason taxonomy
/// to decide whether to back off — but nothing inside this crate should be choosing the two
/// independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HelloRejected {
    /// Always `false`. What the TypeScript union narrows on.
    #[ts(type = "false")]
    pub ok: OkFalse,
    /// Why the connection was refused.
    pub reason: RejectReason,
    /// Whether backing off and reconnecting could succeed.
    pub retryable: bool,
}

impl HelloRejected {
    /// Refuse a `hello`, taking `retryable` from the reason.
    #[must_use]
    pub fn new(reason: RejectReason) -> Self {
        Self {
            ok: OkFalse,
            retryable: reason.retryable(),
            reason,
        }
    }
}

/// The daemon's first frame back, either way.
///
/// Untagged, because the two arms are told apart by `ok` — which is a literal `true` or
/// `false` in both Rust and TypeScript, so neither side has to guess from which optional
/// field happens to be present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum HelloResponse {
    /// The connection may proceed.
    Accepted(HelloAccepted),
    /// The connection was refused.
    Rejected(HelloRejected),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::PROTOCOL_VERSION;

    fn identity() -> DaemonIdentity {
        DaemonIdentity {
            pid: 4242,
            started_at_ms: 1_757_721_600_000,
            launch_nonce: "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60".parse().unwrap(),
            app_version: "0.1.0".to_owned(),
        }
    }

    #[test]
    fn a_hello_carries_its_own_type_tag() {
        let hello = HelloRequest::new(
            PROTOCOL_VERSION,
            ClientRole::Control,
            "nysia-window".parse().unwrap(),
        );
        assert_eq!(
            serde_json::to_value(&hello).unwrap(),
            serde_json::json!({
                "type": "hello",
                "version": 1,
                "role": "control",
                "clientId": "nysia-window",
            })
        );
        let back: HelloRequest = serde_json::from_value(serde_json::to_value(&hello).unwrap())
            .expect("a hello round-trips");
        assert_eq!(back, hello);
    }

    #[test]
    fn a_hello_without_its_tag_is_refused() {
        let untagged = serde_json::json!({
            "version": 1,
            "role": "control",
            "clientId": "nysia-window",
        });
        assert!(serde_json::from_value::<HelloRequest>(untagged).is_err());

        let mistagged = serde_json::json!({
            "type": "helo",
            "version": 1,
            "role": "control",
            "clientId": "nysia-window",
        });
        assert!(serde_json::from_value::<HelloRequest>(mistagged).is_err());
    }

    #[test]
    fn client_ids_reject_what_would_make_a_log_line_unreadable() {
        assert!("nysia-cli".parse::<ClientId>().is_ok());
        for bad in ["", "has space", "has\nnewline", "has\u{0}nul"] {
            assert!(
                bad.parse::<ClientId>().is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!("x".repeat(CLIENT_ID_MAX).parse::<ClientId>().is_ok());
        assert!("x".repeat(CLIENT_ID_MAX + 1).parse::<ClientId>().is_err());
        assert!(serde_json::from_str::<ClientId>("\"has space\"").is_err());
    }

    #[test]
    fn a_launch_nonce_has_exactly_one_spelling() {
        let nonce = LaunchNonce::generate();
        assert_eq!(nonce.to_string().parse::<LaunchNonce>().unwrap(), nonce);
        assert_eq!(
            serde_json::to_string(&nonce).unwrap(),
            format!("\"{nonce}\"")
        );
        for bad in [
            "not-a-uuid",
            "{0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60}",
            "0e2fa1f44f3e4c5f9f2a1b2c3d4e5f60",
            "urn:uuid:0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
        ] {
            assert!(
                bad.parse::<LaunchNonce>().is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_pid_record_describes_only_the_daemon_it_was_written_for() {
        let identity = identity();
        let record = identity.pid_record();
        assert!(record.describes(&identity));

        // A reused pid with a fresh nonce is the case the lease exists for.
        let reused = DaemonIdentity {
            launch_nonce: LaunchNonce::generate(),
            ..identity.clone()
        };
        assert!(!record.describes(&reused));

        let restarted = DaemonIdentity {
            started_at_ms: identity.started_at_ms + 1,
            ..identity.clone()
        };
        assert!(!record.describes(&restarted));

        let other_process = DaemonIdentity {
            pid: identity.pid + 1,
            ..identity
        };
        assert!(!record.describes(&other_process));
    }

    #[test]
    fn an_accepted_hello_is_narrowed_by_a_literal_ok() {
        let accepted = HelloAccepted::new(identity());
        let json = serde_json::to_value(&accepted).unwrap();
        assert_eq!(json["ok"], serde_json::json!(true));
        assert_eq!(json["daemonIdentity"]["startedAtMs"], 1_757_721_600_000_u64);
        assert_eq!(
            json["daemonIdentity"]["launchNonce"],
            "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
        );

        // `ok: false` is not an accepted hello with a typo in it; it is a different frame.
        let mut wrong = json.clone();
        wrong["ok"] = serde_json::json!(false);
        assert!(serde_json::from_value::<HelloAccepted>(wrong).is_err());
        assert_eq!(
            serde_json::from_value::<HelloAccepted>(json).unwrap(),
            accepted
        );
    }

    #[test]
    fn retryable_follows_from_the_reason() {
        assert!(HelloRejected::new(RejectReason::ShuttingDown).retryable);
        for reason in [
            RejectReason::UnsupportedVersion {
                daemon: ProtocolVersion(1),
                attachable: ProtocolRange::attachable(),
            },
            RejectReason::Unauthorized {
                detail: "caller is not in a session this daemon spawned".to_owned(),
            },
            RejectReason::Malformed {
                detail: "first frame was not JSON".to_owned(),
            },
        ] {
            assert!(!HelloRejected::new(reason).retryable);
        }
    }

    #[test]
    fn a_rejection_names_both_halves_of_a_version_mismatch() {
        let rejected = HelloRejected::new(RejectReason::UnsupportedVersion {
            daemon: ProtocolVersion(1),
            attachable: ProtocolRange {
                min: ProtocolVersion(1),
                max: ProtocolVersion(3),
            },
        });
        assert_eq!(
            serde_json::to_value(&rejected).unwrap(),
            serde_json::json!({
                "ok": false,
                "reason": {
                    "kind": "unsupported_version",
                    "daemon": 1,
                    "attachable": { "min": 1, "max": 3 },
                },
                "retryable": false,
            })
        );
    }

    #[test]
    fn a_hello_response_round_trips_through_both_arms() {
        for response in [
            HelloResponse::Accepted(HelloAccepted::new(identity())),
            HelloResponse::Rejected(HelloRejected::new(RejectReason::ShuttingDown)),
        ] {
            let json = serde_json::to_value(&response).unwrap();
            assert_eq!(
                serde_json::from_value::<HelloResponse>(json).unwrap(),
                response
            );
        }
    }

    #[test]
    fn an_unknown_field_is_ignored_so_a_newer_peer_still_connects() {
        // The attachable range exists so mismatched-but-overlapping peers talk. That only
        // works if an unrecognised field is dropped rather than rejected.
        let from_a_newer_daemon = serde_json::json!({
            "ok": true,
            "daemonIdentity": {
                "pid": 4242,
                "startedAtMs": 1_757_721_600_000_u64,
                "launchNonce": "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
                "appVersion": "0.2.0",
                "capabilities": ["land"],
            },
            "sessionCount": 3,
        });
        assert!(serde_json::from_value::<HelloAccepted>(from_a_newer_daemon).is_ok());
    }

    #[test]
    fn roles_round_trip_in_their_wire_spelling() {
        for role in [ClientRole::Control, ClientRole::Stream] {
            assert_eq!(serde_json::to_string(&role).unwrap(), format!("\"{role}\""));
        }
        assert!(serde_json::from_str::<ClientRole>("\"Control\"").is_err());
    }
}
