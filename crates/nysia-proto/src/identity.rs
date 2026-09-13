//! The three identities every session is addressed by, plus what kind of session it is.
//!
//! | Id | Shape | Lifetime |
//! |---|---|---|
//! | [`PaneKey`] | `<tabId>:<leafId>` | Durable. The primary key for status, orchestration binding, everything persisted. |
//! | [`SessionHandle`] | `sess_<uuid>` | Runtime-scoped. Routing only. |
//! | [`Incarnation`] | `<paneKey>@<n>` | One per spawn. A relaunched agent in the same pane is a new incarnation. |
//!
//! All three are newtypes over `String`, so they serialise as plain strings and `ts-rs`
//! exports them as `string`. The string shape *is* the wire format, which means it has to
//! be validated on the way in: `Deserialize` is routed through [`FromStr`] rather than
//! derived, so a malformed id is rejected at the socket boundary instead of surfacing as a
//! confusing lookup miss three layers later.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::newtype::deserialize_via_from_str;

/// Why a string could not be read as one of the identity types.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// A pane key was not exactly `<tabId>:<leafId>`.
    #[error("a pane key is `<tabId>:<leafId>`, got {0:?}")]
    PaneKeyShape(String),
    /// A pane-key segment was empty, or contained a character that would make the composed
    /// id ambiguous.
    #[error("{segment} must be non-empty and free of ':', '@' and whitespace, got {value:?}")]
    Segment {
        /// Which half of the pane key was rejected — `tabId` or `leafId`.
        segment: &'static str,
        /// The offending value.
        value: String,
    },
    /// A session handle was not `sess_<uuid>`.
    #[error("a session handle is `sess_<uuid>`, got {0:?}")]
    SessionHandleShape(String),
    /// An incarnation was not `<paneKey>@<n>`.
    #[error("an incarnation is `<paneKey>@<n>`, got {0:?}")]
    IncarnationShape(String),
    /// The generation suffix of an incarnation was not a bare decimal `u32`.
    #[error("an incarnation generation is a decimal u32, got {0:?}")]
    Generation(String),
    /// A session kind was not one of the two wire spellings.
    #[error("a session kind is `shell` or `agent`, got {0:?}")]
    SessionKindShape(String),
}

/// A pane-key segment may not contain the separators that compose the larger ids, and may
/// not contain whitespace or control characters, because ids end up in log lines and in
/// `NYSIA_PANE_KEY`.
fn check_segment(segment: &'static str, value: &str) -> Result<(), IdentityError> {
    let ambiguous = value.is_empty()
        || value
            .chars()
            .any(|c| c == ':' || c == '@' || c.is_whitespace() || c.is_control());
    if ambiguous {
        return Err(IdentityError::Segment {
            segment,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// The durable identity of a pane: `<tabId>:<leafId>`.
///
/// This is the primary key for agent status, orchestration binding and everything
/// persisted. It outlives the process in the pane, the window, and the daemon.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct PaneKey(String);

impl PaneKey {
    /// Compose a pane key from its two halves.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::Segment`] if either half is empty or contains `:`, `@`,
    /// whitespace or a control character.
    pub fn new(tab_id: &str, leaf_id: &str) -> Result<Self, IdentityError> {
        check_segment("tabId", tab_id)?;
        check_segment("leafId", leaf_id)?;
        Ok(Self(format!("{tab_id}:{leaf_id}")))
    }

    /// The tab half.
    #[must_use]
    pub fn tab_id(&self) -> &str {
        self.split().0
    }

    /// The leaf half.
    #[must_use]
    pub fn leaf_id(&self) -> &str {
        self.split().1
    }

    /// The whole key as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Total because the only two constructors both guarantee exactly one `:`.
    fn split(&self) -> (&str, &str) {
        self.0.split_once(':').unwrap_or((&self.0, ""))
    }
}

impl FromStr for PaneKey {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (tab_id, leaf_id) = s
            .split_once(':')
            .ok_or_else(|| IdentityError::PaneKeyShape(s.to_owned()))?;
        Self::new(tab_id, leaf_id)
    }
}

impl fmt::Display for PaneKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Every session handle carries this prefix, so a handle is recognisable in a log line.
const SESSION_HANDLE_PREFIX: &str = "sess_";

/// The runtime-scoped identity of a session: `sess_<uuid>`.
///
/// Used for routing only. Under D-2 the runtime does not restart when the UI does, so a
/// handle stays valid across a UI upgrade — but it is still never persisted, and a caller
/// that wants to remember a session across daemon restarts remembers its [`PaneKey`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct SessionHandle(String);

impl SessionHandle {
    /// Mint a fresh handle for a newly created session.
    #[must_use]
    pub fn generate() -> Self {
        Self(format!(
            "{SESSION_HANDLE_PREFIX}{}",
            uuid::Uuid::new_v4().as_hyphenated()
        ))
    }

    /// The uuid half, without the `sess_` prefix.
    #[must_use]
    pub fn uuid(&self) -> &str {
        self.0
            .strip_prefix(SESSION_HANDLE_PREFIX)
            .unwrap_or(&self.0)
    }

    /// The whole handle as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SessionHandle {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let shape = || IdentityError::SessionHandleShape(s.to_owned());
        let rest = s.strip_prefix(SESSION_HANDLE_PREFIX).ok_or_else(shape)?;
        let parsed = uuid::Uuid::try_parse(rest).map_err(|_| shape())?;
        // `uuid` also accepts the braced, urn and simple spellings. Refuse them, so one
        // session has exactly one spelling on the wire and string equality is meaningful.
        if parsed.as_hyphenated().to_string() != rest {
            return Err(shape());
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One spawn in a pane: `<paneKey>@<n>`.
///
/// A relaunched agent in the same pane is a new incarnation, which is how status arriving
/// from a dying process is told apart from status arriving from its replacement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct Incarnation(String);

impl Incarnation {
    /// Compose the `generation`th incarnation of `pane`, counting from zero.
    #[must_use]
    pub fn new(pane: &PaneKey, generation: u32) -> Self {
        Self(format!("{pane}@{generation}"))
    }

    /// The pane this incarnation ran in.
    #[must_use]
    pub fn pane_key(&self) -> PaneKey {
        let head = self.0.rsplit_once('@').map_or(self.0.as_str(), |(h, _)| h);
        PaneKey(head.to_owned())
    }

    /// Which spawn this is, counting from zero.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.0
            .rsplit_once('@')
            .and_then(|(_, n)| n.parse().ok())
            .unwrap_or_default()
    }

    /// The whole id as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Incarnation {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // `rsplit_once`, not `split_once`: a pane key may not contain `@`, and splitting
        // from the right is what turns `a:b@2@3` into a clean rejection rather than a
        // silent truncation to `a:b@2`.
        let (head, generation) = s
            .rsplit_once('@')
            .ok_or_else(|| IdentityError::IncarnationShape(s.to_owned()))?;
        let pane: PaneKey = head.parse()?;
        // `u32::from_str` accepts a leading `+`; the wire does not.
        if generation.is_empty() || !generation.bytes().all(|b| b.is_ascii_digit()) {
            return Err(IdentityError::Generation(generation.to_owned()));
        }
        let generation: u32 = generation
            .parse()
            .map_err(|_| IdentityError::Generation(generation.to_owned()))?;
        Ok(Self::new(&pane, generation))
    }
}

impl fmt::Display for Incarnation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What is running in a pane.
///
/// Claude is the only agent in v1 and there is no provider abstraction (D-3, D-4), so this
/// is deliberately two variants rather than an open registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SessionKind {
    /// A plain shell: pwsh, cmd, Git Bash or WSL.
    Shell,
    /// A Claude agent session.
    Agent,
}

impl SessionKind {
    /// The wire spelling, which is also what this type exports to TypeScript.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Agent => "agent",
        }
    }
}

impl fmt::Display for SessionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SessionKind {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shell" => Ok(Self::Shell),
            "agent" => Ok(Self::Agent),
            other => Err(IdentityError::SessionKindShape(other.to_owned())),
        }
    }
}

deserialize_via_from_str!(PaneKey);
deserialize_via_from_str!(SessionHandle);
deserialize_via_from_str!(Incarnation);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_key_round_trips_through_display_and_from_str() {
        let key = PaneKey::new("tab_7", "leaf-2").unwrap();
        assert_eq!(key.to_string(), "tab_7:leaf-2");
        assert_eq!(key.tab_id(), "tab_7");
        assert_eq!(key.leaf_id(), "leaf-2");
        assert_eq!("tab_7:leaf-2".parse::<PaneKey>().unwrap(), key);
    }

    #[test]
    fn pane_key_rejects_ambiguous_segments() {
        assert!(PaneKey::new("", "leaf").is_err());
        assert!(PaneKey::new("tab", "").is_err());
        assert!(PaneKey::new("ta:b", "leaf").is_err());
        assert!(PaneKey::new("tab", "le@af").is_err());
        assert!(PaneKey::new("tab", "le af").is_err());
        assert!("no-separator".parse::<PaneKey>().is_err());
        assert!("a:b:c".parse::<PaneKey>().is_err());
        assert!(":leaf".parse::<PaneKey>().is_err());
    }

    #[test]
    fn session_handle_round_trips_and_rejects_other_uuid_spellings() {
        let handle = SessionHandle::generate();
        let text = handle.to_string();
        assert!(text.starts_with("sess_"));
        assert_eq!(text.parse::<SessionHandle>().unwrap(), handle);
        assert_eq!(handle.uuid().len(), 36);

        assert!("sess_not-a-uuid".parse::<SessionHandle>().is_err());
        assert!(
            "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
                .parse::<SessionHandle>()
                .is_err()
        );
        assert!(
            "sess_{0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60}"
                .parse::<SessionHandle>()
                .is_err()
        );
        assert!(
            "sess_0e2fa1f44f3e4c5f9f2a1b2c3d4e5f60"
                .parse::<SessionHandle>()
                .is_err()
        );
    }

    #[test]
    fn incarnation_round_trips_and_splits_from_the_right() {
        let pane = PaneKey::new("tab", "leaf").unwrap();
        let inc = Incarnation::new(&pane, 3);
        assert_eq!(inc.to_string(), "tab:leaf@3");
        assert_eq!(inc.pane_key(), pane);
        assert_eq!(inc.generation(), 3);
        assert_eq!("tab:leaf@3".parse::<Incarnation>().unwrap(), inc);
    }

    #[test]
    fn incarnation_rejects_extra_separators_and_non_decimal_generations() {
        assert!("tab:leaf".parse::<Incarnation>().is_err());
        assert!("tab:leaf@".parse::<Incarnation>().is_err());
        assert!("tab:leaf@2@3".parse::<Incarnation>().is_err());
        assert!("tab:leaf@+3".parse::<Incarnation>().is_err());
        assert!("tab:leaf@-1".parse::<Incarnation>().is_err());
        assert!("tab:leaf@3x".parse::<Incarnation>().is_err());
        assert!("@3".parse::<Incarnation>().is_err());
        // Overflow is a shape error, not a wrap.
        assert!("tab:leaf@4294967296".parse::<Incarnation>().is_err());
    }

    #[test]
    fn session_kind_round_trips_in_its_wire_spelling() {
        for kind in [SessionKind::Shell, SessionKind::Agent] {
            assert_eq!(kind.to_string().parse::<SessionKind>().unwrap(), kind);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{kind}\""));
        }
        assert!("Shell".parse::<SessionKind>().is_err());
    }

    #[test]
    fn identities_serialise_as_bare_strings() {
        let pane = PaneKey::new("tab", "leaf").unwrap();
        assert_eq!(serde_json::to_string(&pane).unwrap(), "\"tab:leaf\"");

        let inc = Incarnation::new(&pane, 0);
        assert_eq!(serde_json::to_string(&inc).unwrap(), "\"tab:leaf@0\"");

        let handle = SessionHandle::generate();
        assert_eq!(
            serde_json::to_string(&handle).unwrap(),
            format!("\"{handle}\"")
        );
    }

    #[test]
    fn deserialisation_rejects_what_the_constructors_reject() {
        assert!(serde_json::from_str::<PaneKey>("\"tab:leaf\"").is_ok());
        assert!(serde_json::from_str::<PaneKey>("\"tableaf\"").is_err());
        assert!(serde_json::from_str::<Incarnation>("\"tab:leaf@1\"").is_ok());
        assert!(serde_json::from_str::<Incarnation>("\"tab:leaf\"").is_err());
        assert!(serde_json::from_str::<SessionHandle>("\"sess_nope\"").is_err());
        assert!(serde_json::from_str::<SessionKind>("\"shell\"").is_ok());
        assert!(serde_json::from_str::<SessionKind>("\"Shell\"").is_err());
    }
}
