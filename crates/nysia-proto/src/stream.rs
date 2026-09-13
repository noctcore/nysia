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
//! - **Not reused while live.** The daemon does not hand out an id that is currently
//!   attached. After a [`StreamDetach`] it may reuse one, which is the usual reason to
//!   detach before attaching the same session again rather than relying on the id to change.
//! - **Never [`StreamId::RESERVED`].** Zero is not assigned, so a zero-filled header is
//!   refused by the codec itself rather than routed somewhere.
//!
//! # A frame for an id that is not attached
//!
//! **Drop the connection.** Exactly what a malformed frame gets, and deliberately not
//! something softer.
//!
//! The tempting alternative is to skip the frame and carry on, and it is wrong. The frame
//! was still consumed from a length-prefixed stream, so the bytes after it are only in the
//! right place if the length was right — and a peer that got the id wrong has already shown
//! it disagrees about what is on this connection. Worse, a detach races against output
//! already in flight, so "unknown id" would be a *routine* event under a skip policy and a
//! genuine desynchronisation would hide inside the noise. Dropping the connection makes a
//! disagreement loud and recoverable: the client reattaches and knows what it has.
//!
//! The codec cannot enforce this — [`decode`](crate::frame::decode) is pure and holds no
//! session table — so the router does. The rule lives here because it is a property of the
//! protocol rather than of one implementation.

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

    /// The id as the bare number it is in the header.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
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
    fn zero_is_reserved_and_named_as_such() {
        assert_eq!(StreamId::RESERVED, StreamId(0));
        assert_eq!(StreamId::RESERVED.get(), 0);
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
