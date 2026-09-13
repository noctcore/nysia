//! Stream multiplexing: binding a session's output to an id inside one binary connection.
//!
//! A client opens two connections (§3.1): a [`Control`](crate::ClientRole::Control) one
//! carrying request/response NDJSON, and a [`Stream`](crate::ClientRole::Stream) one
//! carrying length-prefixed binary output. The stream connection carries **every** session
//! the client is watching, told apart by a [`StreamId`] in each frame header.
//!
//! One connection rather than one per session, for two reasons that were already in the
//! design before the header said so. §7.3 budgets credit both per stream (512 KiB initial,
//! 2 MiB max) *and* in total (2 and 8 MiB), and a total across streams means nothing unless
//! they share a connection. And the webview gets exactly one `Channel`, so thirty sessions
//! would otherwise mean thirty sockets, thirty handshakes and thirty peer-credential checks
//! for no benefit.
//!
//! # How an id is assigned
//!
//! On the **control** connection, not the stream one — the stream connection carries no
//! JSON and has nowhere to put a request. [`StreamAttach`] names a [`SessionHandle`], the
//! daemon answers [`StreamAttached`] with the id it chose, and output for that session then
//! appears in frames carrying it.
//!
//! The two connections are tied together by the [`ClientId`](crate::ClientId) in their
//! `hello` frames: a client uses the same id for both, and that is how the daemon knows
//! which stream connection an attach on the control connection is talking about. Nothing
//! else links them, which is why the id is not optional in the handshake.
//!
//! # What an id means
//!
//! - **Scoped to one stream connection.** Two clients can both be watching the same session
//!   and will each have their own id for it, and neither id means anything on the other's
//!   connection. An id is a routing label, never an identity — that is what
//!   [`SessionHandle`] and [`PaneKey`](crate::PaneKey) are for.
//! - **Monotonic, and never reused.** The daemon hands out [`StreamId::FIRST`] and counts
//!   up. A detached id is retired, not recycled. A `u32` gives four billion attaches for the
//!   life of one connection, which nobody will exhaust, and reuse buys nothing but ambiguity
//!   — see below, where not reusing is what makes the routing rules decidable at all.
//! - **Never [`StreamId::RESERVED`].** Zero is not assigned, so a zero-filled header is
//!   refused by the codec itself rather than routed somewhere.
//!
//! # A frame whose id has no live entry
//!
//! Control and stream are separate sockets with no ordering between them, so a detach always
//! races output already in flight: the daemon's last chunk tagged 7 can be on the wire while
//! the client is processing the detach ack that removed 7 from its table. The race is
//! unavoidable, which means the protocol has to say what the loser does — and "drop the
//! connection" would punish thirty other sessions for one routine detach.
//!
//! Because ids are never reused, the two cases are **distinguishable**, and that is the whole
//! reason the no-reuse rule is worth its cost:
//!
//! | The id is | What it means | What to do |
//! |---|---|---|
//! | live in the router's table | a normal frame | route it |
//! | below the next id to assign, with no live entry | it was assigned and has since been detached — provably the detach race | **discard the frame** |
//! | at or beyond the next id to assign | never handed out on this connection — provably a desync or a protocol violation | **drop the connection** |
//!
//! [`StreamId::classify_unattached`] is that table as a function, so a router does not have
//! to re-derive it. The discard case is bounded by the in-flight window — at most what §7.3
//! allows unacknowledged — so it cannot run away; a peer that keeps sending on a retired id
//! is spending its own credit and will stop when the window closes.
//!
//! An earlier draft of this module discarded *every* unroutable frame and worried, correctly,
//! that doing so would hide a real desynchronisation inside the routine noise. That objection
//! was fatal while ids could be recycled, because then a stale frame and a bogus one could
//! carry the same number. With ids retired on detach the ambiguity is gone: the watermark
//! separates them exactly, so one case can be tolerated and the other can be fatal.
//!
//! **The same three rules apply in the other direction.** A [`FrameKind::Credit`](crate::FrameKind::Credit) frame
//! travelling daemon-ward carries a stream id in the same header and gets the same treatment
//! — a grant for a freshly detached id is discarded, a grant for an id never assigned drops
//! the connection. Flow control is not a special case, and a rule that held in only one
//! direction would be one the two ends could disagree about.
//!
//! The codec cannot enforce any of this — [`decode`](crate::frame::decode) is pure and holds
//! no router table or watermark — so the router does. The rules live here because they are
//! properties of the protocol rather than of one implementation.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::identity::SessionHandle;

/// Which session a binary frame belongs to, within one stream connection.
///
/// A `u32` because the header field is four bytes; the daemon assigns small numbers and a
/// client that holds more than a handful of sessions is unusual, so the width is about
/// alignment rather than range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StreamId(pub u32);

impl StreamId {
    /// The id the daemon never assigns.
    ///
    /// It buys the codec a second structural check for free: a zero-filled buffer fails
    /// because byte 0 is not a kind, and would fail again at the stream id even if it did
    /// not. Both [`encode_into`](crate::frame::encode_into) and
    /// [`decode`](crate::frame::decode) refuse it, so a writer cannot emit what its own
    /// reader would reject.
    pub const RESERVED: Self = Self(0);

    /// The first id a stream connection hands out.
    ///
    /// One, not zero, because zero is [`RESERVED`](Self::RESERVED). Ids count up from here
    /// and are never reused, so this is also the watermark a fresh connection starts at:
    /// every id is "never assigned" until something is.
    pub const FIRST: Self = Self(1);

    /// The id as the bare number it is in the header.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The next id to hand out after this one, or `None` at the ceiling.
    ///
    /// `None` rather than wrapping: wrapping would silently reuse ids and collapse the
    /// distinction the whole routing policy rests on. A connection that has attached four
    /// billion sessions should be made to reconnect, which costs one handshake and restores
    /// a watermark that means something.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }

    /// Whether this id was ever handed out on a connection whose next id is `next_to_assign`.
    ///
    /// Sound only because ids are never reused. With recycling this comparison would be
    /// meaningless, which is the practical reason retirement is worth a monotonic counter.
    #[must_use]
    pub const fn was_assigned(self, next_to_assign: Self) -> bool {
        self.0 < next_to_assign.0
    }

    /// What to do with a frame whose id the router has **no live entry** for.
    ///
    /// Check the router's table first; this answers only the miss. `next_to_assign` is the
    /// id this connection would hand out next, which is the watermark that separates a
    /// retired id from one that never existed.
    #[must_use]
    pub const fn classify_unattached(self, next_to_assign: Self) -> UnattachedFrame {
        if self.was_assigned(next_to_assign) {
            UnattachedFrame::Discard
        } else {
            UnattachedFrame::DropConnection
        }
    }
}

/// What a router does with a frame whose stream id has no live entry.
///
/// Two answers rather than one, and the difference is the point: a detach races output
/// already in flight on a separate socket, so one of these is routine and bounded while the
/// other means the two ends disagree about what is on the connection. See the module docs
/// for why never reusing an id is what makes them tellable apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UnattachedFrame {
    /// The id was assigned and has since been detached. Drop the frame, keep the connection.
    ///
    /// Routine, and bounded by the in-flight window: a peer still sending on a retired id is
    /// spending credit it will not get back, so the noise stops on its own.
    Discard,
    /// The id was never handed out here. Drop the connection.
    ///
    /// Not a race — there is no sequence of events in which a well-behaved peer names an id
    /// the daemon has not yet minted — so it is the same class of failure as a malformed
    /// frame and gets the same answer.
    DropConnection,
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Ask for a session's output on this client's stream connection.
///
/// Sent on the control connection. The daemon picks the id — a client cannot, because ids
/// are per stream connection and the daemon is the only party that knows which are in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamAttach {
    /// Which session to start streaming.
    pub handle: SessionHandle,
}

/// The id the daemon assigned, and the session it now routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamAttached {
    /// The session that was attached — echoed, so a client with several attaches in flight
    /// does not have to remember which answer belongs to which request beyond the
    /// `requestId`.
    pub handle: SessionHandle,
    /// The id its frames will carry, on this client's stream connection only.
    pub stream_id: StreamId,
}

/// Stop routing a stream id, freeing it for reuse.
///
/// By id rather than by handle: the id is what the stream connection is keyed on, and a
/// session can in principle be attached more than once by a client that wants two views of
/// it. Detaching by handle would be ambiguous in exactly that case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamDetach {
    /// Which id to release.
    pub stream_id: StreamId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> SessionHandle {
        "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60".parse().unwrap()
    }

    #[test]
    fn a_stream_id_is_a_bare_number_on_the_wire() {
        assert_eq!(serde_json::to_string(&StreamId(7)).unwrap(), "7");
        assert_eq!(serde_json::from_str::<StreamId>("7").unwrap(), StreamId(7));
        assert_eq!(StreamId(7).to_string(), "7");
        assert_eq!(StreamId(7).get(), 7);
    }

    #[test]
    fn zero_is_reserved_and_allocation_starts_above_it() {
        assert_eq!(StreamId::RESERVED, StreamId(0));
        assert_eq!(StreamId::RESERVED.get(), 0);
        assert_eq!(StreamId::FIRST, StreamId(1));
        assert_ne!(StreamId::FIRST, StreamId::RESERVED);
    }

    #[test]
    fn ids_count_up_and_stop_rather_than_wrapping() {
        assert_eq!(StreamId::FIRST.next(), Some(StreamId(2)));
        assert_eq!(StreamId(41).next(), Some(StreamId(42)));
        // Wrapping would silently recycle ids and collapse the distinction every routing
        // rule below rests on, so the ceiling is a hard stop.
        assert_eq!(StreamId(u32::MAX).next(), None);
    }

    #[test]
    fn a_retired_id_is_discarded_and_an_unminted_one_is_fatal() {
        // The connection has handed out 1..=6 and would hand out 7 next.
        let next = StreamId(7);

        // The detach race: 3 was assigned, has been detached, and a frame for it is still in
        // flight. Dropping the connection here would punish every other session on it.
        assert_eq!(
            StreamId(3).classify_unattached(next),
            UnattachedFrame::Discard
        );
        assert_eq!(
            StreamId(6).classify_unattached(next),
            UnattachedFrame::Discard
        );

        // 7 has not been minted yet, so no well-behaved peer can name it. That is a desync,
        // and it gets the malformed-frame answer.
        assert_eq!(
            StreamId(7).classify_unattached(next),
            UnattachedFrame::DropConnection
        );
        assert_eq!(
            StreamId(4_000_000_000).classify_unattached(next),
            UnattachedFrame::DropConnection
        );

        assert!(StreamId(6).was_assigned(next));
        assert!(!StreamId(7).was_assigned(next));
    }

    #[test]
    fn a_fresh_connection_has_assigned_nothing() {
        // Before the first attach the watermark is FIRST, so every id is "never minted" —
        // including FIRST itself. A frame arriving before any attach is a desync, not a race.
        let next = StreamId::FIRST;
        assert!(!StreamId::FIRST.was_assigned(next));
        assert_eq!(
            StreamId::FIRST.classify_unattached(next),
            UnattachedFrame::DropConnection
        );
    }

    #[test]
    fn the_routing_decision_names_itself_on_the_wire() {
        // Exported so a client can log or report the decision in the daemon's own words
        // rather than inventing a second vocabulary for it.
        assert_eq!(
            serde_json::to_string(&UnattachedFrame::Discard).unwrap(),
            "\"discard\""
        );
        assert_eq!(
            serde_json::to_string(&UnattachedFrame::DropConnection).unwrap(),
            "\"drop_connection\""
        );
    }

    #[test]
    fn the_attach_verbs_round_trip() {
        let attach = StreamAttach { handle: handle() };
        let json = serde_json::to_value(&attach).unwrap();
        assert_eq!(json["handle"], "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60");
        assert_eq!(
            serde_json::from_value::<StreamAttach>(json).unwrap(),
            attach
        );

        let attached = StreamAttached {
            handle: handle(),
            stream_id: StreamId(3),
        };
        let json = serde_json::to_value(&attached).unwrap();
        assert_eq!(json["streamId"], 3);
        assert_eq!(
            serde_json::from_value::<StreamAttached>(json).unwrap(),
            attached
        );

        let detach = StreamDetach {
            stream_id: StreamId(3),
        };
        let json = serde_json::to_value(&detach).unwrap();
        assert_eq!(json["streamId"], 3);
        assert_eq!(
            serde_json::from_value::<StreamDetach>(json).unwrap(),
            detach
        );
    }
}
