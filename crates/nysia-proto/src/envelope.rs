//! The request and response envelopes, and the idempotency shape that makes "did it land?"
//! answerable.
//!
//! Every control frame after the handshake is an envelope. It carries a [`RequestId`],
//! which is what correlates an answer with its question on a connection that does not
//! promise to answer in order.
//!
//! The harder problem is the one §6.2 calls out: a mutation whose answer never arrived.
//! The caller cannot tell "the session was never created" from "the session was created and
//! the reply was lost", and those need opposite responses. So every mutation may carry
//! `retryRequest` — *this is a retry of that request* — and the daemon, which keyed the
//! original effect by request id, replays the original answer instead of doing the work
//! twice. The [`MutationReceipt`] on the way back says which happened.
//!
//! Both envelopes flatten their payload, so a frame reads
//! `{"requestId":…,"type":"session_create","kind":"shell",…}` rather than nesting an
//! object under a key. That matches the handshake, which puts `type` at the top level, and
//! it means one `type` field identifies every control frame on the socket.
//!
//! Request and response variants carry the **same** tag for the same verb. A response is
//! never confusable with a request — they travel in opposite directions on a
//! request/response channel — and the shared spelling lets a client assert that the answer
//! it got belongs to the question it asked.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::{
    AgentHook, AgentStatus, AgentStatusGet, AgentStatusList, AgentStatusSubscribe,
    AgentStatusSubscribed, AgentStatusUnsubscribe,
};
use crate::error::ErrorEnvelope;
use crate::newtype::deserialize_via_from_str;
use crate::session::{SessionClose, SessionCreate, SessionCreated, SessionList, SessionSummary};
use crate::stream::{StreamAttach, StreamAttached, StreamDetach};
use crate::terminal::{
    TerminalRead, TerminalReadResult, TerminalResize, TerminalSend, TerminalWait,
    TerminalWaitResult,
};

/// Why a request id could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// A request id was not `req_<uuid>`.
    #[error("a request id is `req_<uuid>`, got {0:?}")]
    RequestIdShape(String),
}

/// Every request id carries this prefix, so one is recognisable in a log line.
const REQUEST_ID_PREFIX: &str = "req_";

/// The idempotency key of one request: `req_<uuid>`.
///
/// Shaped like [`crate::SessionHandle`] and for the same reason — one spelling per id, so
/// string equality is a sound test. That matters more here than anywhere else: the daemon
/// keys mutation receipts by this value, and two spellings of the same id would let the
/// same mutation run twice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct RequestId(String);

impl RequestId {
    /// Mint a fresh id for a new request.
    #[must_use]
    pub fn generate() -> Self {
        Self(format!(
            "{REQUEST_ID_PREFIX}{}",
            uuid::Uuid::new_v4().as_hyphenated()
        ))
    }

    /// The id as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RequestId {
    type Err = EnvelopeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let shape = || EnvelopeError::RequestIdShape(s.to_owned());
        let rest = s.strip_prefix(REQUEST_ID_PREFIX).ok_or_else(shape)?;
        let parsed = uuid::Uuid::try_parse(rest).map_err(|_| shape())?;
        // The braced, urn and simple spellings are refused, so a receipt lookup cannot miss
        // because the retry spelled the same uuid differently.
        if parsed.as_hyphenated().to_string() != rest {
            return Err(shape());
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

deserialize_via_from_str!(RequestId);

/// What a request asks for.
///
/// `PartialEq` but not `Eq`: [`AgentHook`] carries Claude's question payload verbatim as
/// arbitrary JSON, and `serde_json::Value` is not `Eq` because a float could be `NaN`. The
/// same goes for [`ResponsePayload`] and both envelopes. Nothing here is a map key, so the
/// weaker bound costs nothing and keeping the stronger one would have meant re-shaping a
/// payload this crate has no authority over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum RequestPayload {
    /// Spawn a session.
    SessionCreate(SessionCreate),
    /// List every session.
    SessionList(SessionList),
    /// Close a session.
    SessionClose(SessionClose),
    /// Read the screen or the scrollback.
    TerminalRead(TerminalRead),
    /// Write to a session's input.
    TerminalSend(TerminalSend),
    /// Change a session's viewport size.
    TerminalResize(TerminalResize),
    /// Block until a session exits or goes idle.
    TerminalWait(TerminalWait),
    /// Start routing a session's output on this client's stream connection.
    StreamAttach(StreamAttach),
    /// Stop routing a stream id.
    StreamDetach(StreamDetach),
    /// Hand the daemon a Claude hook event (D-16).
    AgentHook(AgentHook),
    /// Read one pane's agent status.
    AgentStatusGet(AgentStatusGet),
    /// Read every pane's agent status.
    AgentStatusList(AgentStatusList),
    /// Start receiving status changes on this client's stream connection.
    AgentStatusSubscribe(AgentStatusSubscribe),
    /// Stop receiving status changes.
    AgentStatusUnsubscribe(AgentStatusUnsubscribe),
}

impl RequestPayload {
    /// The verb's wire tag.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::SessionCreate(_) => "session_create",
            Self::SessionList(_) => "session_list",
            Self::SessionClose(_) => "session_close",
            Self::TerminalRead(_) => "terminal_read",
            Self::TerminalSend(_) => "terminal_send",
            Self::TerminalResize(_) => "terminal_resize",
            Self::TerminalWait(_) => "terminal_wait",
            Self::StreamAttach(_) => "stream_attach",
            Self::StreamDetach(_) => "stream_detach",
            Self::AgentHook(_) => "agent_hook",
            Self::AgentStatusGet(_) => "agent_status_get",
            Self::AgentStatusList(_) => "agent_status_list",
            Self::AgentStatusSubscribe(_) => "agent_status_subscribe",
            Self::AgentStatusUnsubscribe(_) => "agent_status_unsubscribe",
        }
    }

    /// Whether this verb changes daemon state, and so may carry `retryRequest`.
    ///
    /// A read carrying a retry id is not an error — the daemon ignores it — but it is not
    /// idempotency either, because there is no effect to deduplicate. Only the verbs that
    /// change something get a receipt, and
    /// `only_the_verbs_that_change_something_are_mutations` is what holds this list to the
    /// enum above rather than to a number in this sentence.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        match self {
            Self::SessionCreate(_)
            | Self::SessionClose(_)
            | Self::TerminalSend(_)
            | Self::TerminalResize(_)
            | Self::StreamAttach(_)
            | Self::StreamDetach(_)
            // Ingest writes a row, and the spool replays what it could not send — a record
            // drained twice must record one state change, not two.
            | Self::AgentHook(_)
            // Subscribing and unsubscribing move a stream id, exactly as attaching and
            // detaching do, and ids are never reused: a retried subscribe that ran twice
            // would leak one for the life of the connection.
            | Self::AgentStatusSubscribe(_)
            | Self::AgentStatusUnsubscribe(_) => true,
            Self::SessionList(_)
            | Self::TerminalRead(_)
            | Self::TerminalWait(_)
            | Self::AgentStatusGet(_)
            | Self::AgentStatusList(_) => false,
        }
    }
}

/// One control request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestEnvelope {
    /// This request's idempotency key, minted by the caller.
    pub request_id: RequestId,
    /// The id of the request this one re-attempts, when it is a retry.
    ///
    /// Set it and the daemon looks for a receipt before doing anything: if the original
    /// landed, the original answer comes back with `replayed` set and nothing runs twice.
    /// Meaningful only when the payload [`is_mutation`](RequestPayload::is_mutation).
    pub retry_request: Option<RequestId>,
    /// What is being asked.
    #[serde(flatten)]
    pub payload: RequestPayload,
}

impl RequestEnvelope {
    /// A fresh request, with a newly minted id.
    #[must_use]
    pub fn new(payload: RequestPayload) -> Self {
        Self {
            request_id: RequestId::generate(),
            retry_request: None,
            payload,
        }
    }

    /// A retry of `original`, with its own fresh id.
    ///
    /// The new id is what this attempt is keyed by; `original` is what the daemon looks up.
    /// Reusing the original id instead would make the two attempts indistinguishable, which
    /// is the thing being fixed.
    #[must_use]
    pub fn retrying(payload: RequestPayload, original: RequestId) -> Self {
        Self {
            request_id: RequestId::generate(),
            retry_request: Some(original),
            payload,
        }
    }
}

/// Proof that a mutation landed, and whether this answer is the first one.
///
/// §6.2 keys receipts by `(caller_fingerprint, request_id)`. The fingerprint is the
/// daemon's business — it comes from peer credentials, not from the frame — so what crosses
/// the wire is the id and the one bit the caller cannot work out for itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MutationReceipt {
    /// The id the effect is recorded under — the original request's, on a replay.
    pub request_id: RequestId,
    /// `true` when the work had already been done and this is the stored answer.
    ///
    /// The whole point of the receipt. A caller that retried a `session_create` and gets
    /// `replayed: true` has one session, not two, and knows it.
    pub replayed: bool,
}

/// What a response carries.
///
/// The tags mirror [`RequestPayload`]'s, so a client can check that the answer belongs to
/// the question. Verbs whose whole answer is "it happened" are unit variants: the
/// `requestId` already says which request they answer, and inventing a body to hold nothing
/// would be a shape to maintain forever.
///
/// `PartialEq` but not `Eq`, for [`RequestPayload`]'s reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ResponsePayload {
    /// The ids the daemon minted.
    SessionCreate(SessionCreated),
    /// Every session the daemon owns.
    SessionList {
        /// The rows.
        sessions: Vec<SessionSummary>,
    },
    /// The session is closing.
    SessionClose,
    /// What was on screen, or what scrolled past.
    TerminalRead(TerminalReadResult),
    /// The input was written.
    TerminalSend,
    /// The viewport was resized.
    TerminalResize,
    /// How the wait ended.
    TerminalWait(TerminalWaitResult),
    /// The id the daemon assigned.
    StreamAttach(StreamAttached),
    /// The id was released.
    StreamDetach,
    /// The hook event was recorded.
    AgentHook,
    /// The pane's status, or `null` when no agent has ever reported one for it.
    ///
    /// A pane with no status is the ordinary case — a shell, or an agent that has not run a
    /// hook yet — so it is an absent answer rather than an error. `unknown_session` here
    /// would have a caller treating "nothing has happened" as a fault.
    AgentStatusGet {
        /// The status, or `null`.
        status: Option<AgentStatus>,
    },
    /// Every pane the daemon holds a status for.
    AgentStatusList {
        /// The rows.
        statuses: Vec<AgentStatus>,
    },
    /// The id the daemon assigned to the subscription.
    AgentStatusSubscribe(AgentStatusSubscribed),
    /// The subscription was released.
    AgentStatusUnsubscribe,
    /// The verb failed.
    Error(ErrorEnvelope),
}

impl ResponsePayload {
    /// The verb this answers, or `"error"`.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::SessionCreate(_) => "session_create",
            Self::SessionList { .. } => "session_list",
            Self::SessionClose => "session_close",
            Self::TerminalRead(_) => "terminal_read",
            Self::TerminalSend => "terminal_send",
            Self::TerminalResize => "terminal_resize",
            Self::TerminalWait(_) => "terminal_wait",
            Self::StreamAttach(_) => "stream_attach",
            Self::StreamDetach => "stream_detach",
            Self::AgentHook => "agent_hook",
            Self::AgentStatusGet { .. } => "agent_status_get",
            Self::AgentStatusList { .. } => "agent_status_list",
            Self::AgentStatusSubscribe(_) => "agent_status_subscribe",
            Self::AgentStatusUnsubscribe => "agent_status_unsubscribe",
            Self::Error(_) => "error",
        }
    }

    /// The error, if this is one.
    #[must_use]
    pub fn error(&self) -> Option<&ErrorEnvelope> {
        match self {
            Self::Error(error) => Some(error),
            _ => None,
        }
    }
}

/// One control response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResponseEnvelope {
    /// The id of the request this answers.
    ///
    /// The *incoming* id, even on a replay — the caller correlates on what it sent, and the
    /// receipt is where the original id lives.
    pub request_id: RequestId,
    /// Present when the request was a mutation.
    pub receipt: Option<MutationReceipt>,
    /// The answer.
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

impl ResponseEnvelope {
    /// Answer `request_id` with `payload`, carrying no receipt.
    #[must_use]
    pub fn new(request_id: RequestId, payload: ResponsePayload) -> Self {
        Self {
            request_id,
            receipt: None,
            payload,
        }
    }

    /// Attach a mutation receipt.
    #[must_use]
    pub fn with_receipt(mut self, receipt: MutationReceipt) -> Self {
        self.receipt = Some(receipt);
        self
    }

    /// Whether this response answers `request`, by id and by verb.
    ///
    /// An error answers every verb, so the verb comparison is skipped for one. Anything
    /// else that disagrees is a routing bug, and catching it here is cheaper than debugging
    /// a client that quietly parsed a `terminal_read` result as a `session_create`.
    #[must_use]
    pub fn answers(&self, request: &RequestEnvelope) -> bool {
        self.request_id == request.request_id
            && (self.payload.error().is_some() || self.payload.verb() == request.payload.verb())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentState, AgentStatusRow, HookEvent, HookEventName, UnixMillis};
    use crate::error::{ErrorCode, NextSteps};
    use crate::identity::{Incarnation, PaneKey, SessionHandle, SessionKind};
    use crate::terminal::{LineCursor, ReadMode};
    use std::collections::BTreeMap;

    fn handle() -> SessionHandle {
        "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60".parse().unwrap()
    }

    fn create() -> RequestPayload {
        RequestPayload::SessionCreate(SessionCreate {
            kind: SessionKind::Shell,
            pane_key: None,
            profile: None,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        })
    }

    #[test]
    fn a_request_id_has_exactly_one_spelling() {
        let id = RequestId::generate();
        assert!(id.as_str().starts_with("req_"));
        assert_eq!(id.to_string().parse::<RequestId>().unwrap(), id);
        for bad in [
            "req_not-a-uuid",
            "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
            "req_{0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60}",
            "req_0e2fa1f44f3e4c5f9f2a1b2c3d4e5f60",
            "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
        ] {
            assert!(
                bad.parse::<RequestId>().is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(serde_json::from_str::<RequestId>("\"req_nope\"").is_err());
    }

    #[test]
    fn an_envelope_flattens_its_payload_to_one_type_field() {
        let envelope =
            RequestEnvelope::new(RequestPayload::TerminalRead(TerminalRead::screen(handle())));
        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "terminal_read");
        assert_eq!(json["mode"], "screen");
        assert_eq!(json["retryRequest"], serde_json::Value::Null);
        assert_eq!(json["requestId"], envelope.request_id.as_str());
        assert_eq!(
            serde_json::from_value::<RequestEnvelope>(json).unwrap(),
            envelope
        );
    }

    #[test]
    fn every_verb_round_trips_through_the_request_envelope() {
        let payloads = [
            create(),
            RequestPayload::SessionList(SessionList {}),
            RequestPayload::SessionClose(SessionClose { handle: handle() }),
            RequestPayload::TerminalRead(TerminalRead::stream(handle(), LineCursor(7))),
            RequestPayload::TerminalSend(TerminalSend::line(handle(), "echo hello")),
            RequestPayload::TerminalResize(TerminalResize {
                handle: handle(),
                cols: 120,
                rows: 30,
            }),
            RequestPayload::TerminalWait(TerminalWait {
                handle: handle(),
                wait_for: crate::terminal::WaitFor::Exit,
                timeout_ms: None,
            }),
            RequestPayload::StreamAttach(StreamAttach { handle: handle() }),
            RequestPayload::StreamDetach(StreamDetach {
                stream_id: crate::stream::StreamId(3),
            }),
            RequestPayload::AgentHook(AgentHook {
                pane_hint: Some(PaneKey::new("tab_1", "leaf_1").unwrap()),
                event: HookEvent::new(HookEventName::Stop),
            }),
            RequestPayload::AgentStatusGet(AgentStatusGet {
                pane: PaneKey::new("tab_1", "leaf_1").unwrap(),
            }),
            RequestPayload::AgentStatusList(AgentStatusList {}),
            RequestPayload::AgentStatusSubscribe(AgentStatusSubscribe {}),
            RequestPayload::AgentStatusUnsubscribe(AgentStatusUnsubscribe {
                stream_id: crate::stream::StreamId(4),
            }),
        ];
        for payload in payloads {
            let envelope = RequestEnvelope::new(payload);
            let json = serde_json::to_value(&envelope).unwrap();
            assert_eq!(json["type"], envelope.payload.verb());
            assert_eq!(
                serde_json::from_value::<RequestEnvelope>(json).unwrap(),
                envelope
            );
        }
    }

    #[test]
    fn only_the_verbs_that_change_something_are_mutations() {
        assert!(create().is_mutation());
        assert!(RequestPayload::SessionClose(SessionClose { handle: handle() }).is_mutation());
        assert!(RequestPayload::TerminalSend(TerminalSend::new(handle())).is_mutation());
        assert!(
            RequestPayload::TerminalResize(TerminalResize {
                handle: handle(),
                cols: 80,
                rows: 24,
            })
            .is_mutation()
        );

        assert!(!RequestPayload::SessionList(SessionList {}).is_mutation());
        assert!(!RequestPayload::TerminalRead(TerminalRead::screen(handle())).is_mutation());
        assert!(
            !RequestPayload::TerminalWait(TerminalWait {
                handle: handle(),
                wait_for: crate::terminal::WaitFor::Idle,
                timeout_ms: Some(1_000),
            })
            .is_mutation()
        );

        // Attaching and detaching change which ids the daemon is routing, so both are
        // mutations and both carry a receipt. A retried attach that ran twice would leak a
        // stream id, and ids are never reused — so the leak would be permanent for the life
        // of the connection.
        assert!(RequestPayload::StreamAttach(StreamAttach { handle: handle() }).is_mutation());
        assert!(
            RequestPayload::StreamDetach(StreamDetach {
                stream_id: crate::stream::StreamId(3),
            })
            .is_mutation()
        );

        // Ingest writes a row, and the spool exists to send again what it could not send
        // the first time. Without a receipt a drained record would be indistinguishable
        // from a second hook firing, and the history §2.3 caps would fill with duplicates.
        assert!(
            RequestPayload::AgentHook(AgentHook {
                pane_hint: None,
                event: HookEvent::new(HookEventName::Stop),
            })
            .is_mutation()
        );
        // A subscription moves a stream id exactly as an attach does, and ids are never
        // reused — so a retried subscribe that ran twice would leak one permanently.
        assert!(RequestPayload::AgentStatusSubscribe(AgentStatusSubscribe {}).is_mutation());
        assert!(
            RequestPayload::AgentStatusUnsubscribe(AgentStatusUnsubscribe {
                stream_id: crate::stream::StreamId(4),
            })
            .is_mutation()
        );

        // Reading a status changes nothing, so neither read carries a receipt.
        assert!(
            !RequestPayload::AgentStatusGet(AgentStatusGet {
                pane: PaneKey::new("tab_1", "leaf_1").unwrap(),
            })
            .is_mutation()
        );
        assert!(!RequestPayload::AgentStatusList(AgentStatusList {}).is_mutation());
    }

    #[test]
    fn the_agent_status_verbs_are_matched_to_their_answers() {
        let get = RequestEnvelope::new(RequestPayload::AgentStatusGet(AgentStatusGet {
            pane: PaneKey::new("tab_1", "leaf_1").unwrap(),
        }));
        assert_eq!(get.payload.verb(), "agent_status_get");

        // A pane nothing has reported for answers `null`, not an error: a shell, or an
        // agent whose first hook has not fired, is the ordinary case rather than a fault.
        let empty = ResponseEnvelope::new(
            get.request_id.clone(),
            ResponsePayload::AgentStatusGet { status: None },
        );
        assert!(empty.answers(&get));
        let json = serde_json::to_value(&empty).unwrap();
        assert_eq!(json["type"], "agent_status_get");
        assert_eq!(json["status"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<ResponseEnvelope>(json).unwrap(),
            empty
        );

        let row = AgentStatusRow {
            pane: PaneKey::new("tab_1", "leaf_1").unwrap(),
            state: AgentState::Done,
            question: None,
            is_interrupt: false,
            session_boundary: false,
            agent_id: None,
            observed_at: UnixMillis(1_757_721_600_000),
            restored_unconfirmed: false,
        };
        let listed = ResponseEnvelope::new(
            RequestId::generate(),
            ResponsePayload::AgentStatusList {
                statuses: vec![AgentStatus::new(row)],
            },
        );
        let json = serde_json::to_value(&listed).unwrap();
        assert_eq!(json["type"], "agent_status_list");
        assert_eq!(json["statuses"][0]["lead"]["state"], "done");
        assert_eq!(
            serde_json::from_value::<ResponseEnvelope>(json).unwrap(),
            listed
        );

        // The subscription's answer carries the id its frames will arrive under, exactly as
        // a `stream_attach` does — the same id space, the same rules.
        let subscribe = RequestEnvelope::new(RequestPayload::AgentStatusSubscribe(
            AgentStatusSubscribe {},
        ));
        let subscribed = ResponseEnvelope::new(
            subscribe.request_id.clone(),
            ResponsePayload::AgentStatusSubscribe(AgentStatusSubscribed {
                stream_id: crate::stream::StreamId(4),
            }),
        );
        assert!(subscribed.answers(&subscribe));

        // A subscribe answered by an unsubscribe is a routing bug, not a valid reply.
        let released = ResponseEnvelope::new(
            subscribe.request_id.clone(),
            ResponsePayload::AgentStatusUnsubscribe,
        );
        assert!(!released.answers(&subscribe));

        let hook = RequestEnvelope::new(RequestPayload::AgentHook(AgentHook {
            pane_hint: None,
            event: HookEvent::new(HookEventName::Stop),
        }));
        let recorded = ResponseEnvelope::new(hook.request_id.clone(), ResponsePayload::AgentHook);
        assert!(recorded.answers(&hook));
        assert_eq!(hook.payload.verb(), "agent_hook");
    }

    #[test]
    fn the_stream_verbs_are_matched_to_their_answers() {
        let attach = RequestEnvelope::new(RequestPayload::StreamAttach(StreamAttach {
            handle: handle(),
        }));
        assert_eq!(attach.payload.verb(), "stream_attach");

        let attached = ResponseEnvelope::new(
            attach.request_id.clone(),
            ResponsePayload::StreamAttach(StreamAttached {
                handle: handle(),
                stream_id: crate::stream::StreamId(3),
            }),
        );
        assert!(attached.answers(&attach));

        let detach = RequestEnvelope::new(RequestPayload::StreamDetach(StreamDetach {
            stream_id: crate::stream::StreamId(3),
        }));
        assert_eq!(detach.payload.verb(), "stream_detach");
        let detached =
            ResponseEnvelope::new(detach.request_id.clone(), ResponsePayload::StreamDetach);
        assert!(detached.answers(&detach));

        // An attach answered with a detach is a routing bug, not a valid reply.
        assert!(!detached.answers(&attach));
    }

    #[test]
    fn a_retry_keeps_its_own_id_and_names_the_original() {
        let first = RequestEnvelope::new(create());
        let retry = RequestEnvelope::retrying(create(), first.request_id.clone());
        // Reusing the original id would make the two attempts indistinguishable, which is
        // precisely the problem being solved.
        assert_ne!(retry.request_id, first.request_id);
        assert_eq!(retry.retry_request.as_ref(), Some(&first.request_id));

        let json = serde_json::to_value(&retry).unwrap();
        assert_eq!(json["retryRequest"], first.request_id.as_str());
        assert_eq!(
            serde_json::from_value::<RequestEnvelope>(json).unwrap(),
            retry
        );
    }

    #[test]
    fn a_receipt_says_whether_the_work_ran_twice() {
        let original = RequestId::generate();
        let retry = RequestEnvelope::retrying(create(), original.clone());
        let pane = PaneKey::new("tab_1", "leaf_1").unwrap();
        let replayed = ResponseEnvelope::new(
            retry.request_id.clone(),
            ResponsePayload::SessionCreate(SessionCreated {
                handle: handle(),
                pane_key: pane.clone(),
                incarnation: Incarnation::new(&pane, 0),
            }),
        )
        .with_receipt(MutationReceipt {
            request_id: original.clone(),
            replayed: true,
        });

        // The caller correlates on what it sent; the receipt is where the original id lives.
        assert_eq!(replayed.request_id, retry.request_id);
        assert_eq!(
            replayed.receipt.as_ref().map(|r| &r.request_id),
            Some(&original)
        );
        assert!(replayed.answers(&retry));

        let json = serde_json::to_value(&replayed).unwrap();
        assert_eq!(json["receipt"]["replayed"], true);
        assert_eq!(json["type"], "session_create");
        assert_eq!(
            serde_json::from_value::<ResponseEnvelope>(json).unwrap(),
            replayed
        );
    }

    #[test]
    fn a_response_is_matched_to_its_request_by_id_and_verb() {
        let request =
            RequestEnvelope::new(RequestPayload::TerminalRead(TerminalRead::screen(handle())));
        let right = ResponseEnvelope::new(
            request.request_id.clone(),
            ResponsePayload::TerminalRead(TerminalReadResult {
                lines: vec!["hello".to_owned()],
                cursor: LineCursor(1),
                mode: ReadMode::Screen,
                truncated: false,
            }),
        );
        assert!(right.answers(&request));

        // Right id, wrong verb — a routing bug, caught rather than quietly mis-parsed.
        let wrong_verb =
            ResponseEnvelope::new(request.request_id.clone(), ResponsePayload::TerminalSend);
        assert!(!wrong_verb.answers(&request));

        // Right verb, wrong id.
        let wrong_id = ResponseEnvelope::new(
            RequestId::generate(),
            ResponsePayload::TerminalRead(TerminalReadResult {
                lines: Vec::new(),
                cursor: LineCursor::START,
                mode: ReadMode::Screen,
                truncated: false,
            }),
        );
        assert!(!wrong_id.answers(&request));

        // An error answers any verb.
        let failed = ResponseEnvelope::new(
            request.request_id.clone(),
            ResponsePayload::Error(ErrorEnvelope::new(
                ErrorCode::UnknownSession,
                "no such session",
                NextSteps::new("List the sessions the daemon currently owns.").unwrap(),
            )),
        );
        assert!(failed.answers(&request));
        assert!(failed.payload.error().is_some());
        assert!(right.payload.error().is_none());
    }

    #[test]
    fn an_ack_response_needs_no_body() {
        let envelope =
            ResponseEnvelope::new(RequestId::generate(), ResponsePayload::TerminalResize);
        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "terminal_resize");
        assert_eq!(json["receipt"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<ResponseEnvelope>(json).unwrap(),
            envelope
        );
    }

    #[test]
    fn a_list_response_carries_its_rows() {
        let envelope = ResponseEnvelope::new(
            RequestId::generate(),
            ResponsePayload::SessionList {
                sessions: vec![SessionSummary {
                    handle: handle(),
                    pane_key: PaneKey::new("tab_1", "leaf_1").unwrap(),
                    kind: SessionKind::Shell,
                    title: "pwsh".to_owned(),
                    created_at_ms: 1_757_721_600_000,
                    exit_status: None,
                }],
            },
        );
        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "session_list");
        assert_eq!(json["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(
            serde_json::from_value::<ResponseEnvelope>(json).unwrap(),
            envelope
        );
    }
}
