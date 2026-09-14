//! Credit-window backpressure (§7.3).
//!
//! A PTY can produce output faster than a webview can render it. Without a brake, `yes`
//! fills the daemon's buffers, then the IPC queue, then memory. The brake is a credit
//! window.
//!
//! # Which way each frame goes
//!
//! **The producer grants; the consumer acks.** Said once, here, because saying it twice is
//! how the two ends came to disagree:
//!
//! - The **producer** is the daemon. It owns the window, and a [`CreditGrant`] travels
//!   producer → consumer, opening a stream and saying how many bytes may follow.
//! - The **consumer** is the client. A [`CreditAck`] travels consumer → producer, after
//!   xterm's `write()` callback rather than on arrival, because the window tracks what has
//!   been *rendered* and a message sitting in a queue has not been.
//!
//! "Reader" and "writer" are the words to avoid: the daemon is the reader of the PTY and the
//! writer of the socket, so either name picks out whichever end the speaker had in mind.
//!
//! A grant arriving from a consumer is not a grant. The party that owns the window is the
//! only one that may enlarge it — a consumer able to credit itself could turn the
//! backpressure off from the outside, which is the one thing the window exists to prevent.
//!
//! # And what it is counted in
//!
//! **Payload bytes**, in both directions. Not the encoded length: the frame header is
//! transport overhead the consumer never receives as content, and the ack is emitted by the
//! code that has just written a payload into a terminal, so it can only ever count payloads.
//! A producer charging the header instead drains its own allowance by the header size on
//! every frame — tens of thousands of frames into a session, permanently, for a reason
//! nothing in a log would name.
//!
//! At zero credit the daemon stops reading the PTY. The kernel's PTY buffer then fills and
//! the child blocks. That is the complete answer to a `yes` flood, and it works without
//! dropping a byte.
//!
//! The defaults are Orca's production numbers, lifted rather than guessed:
//!
//! | | Per stream | Total |
//! |---|---|---|
//! | Initial | 512 KiB | 2 MiB |
//! | Maximum | 2 MiB | 8 MiB |
//!
//! plus a 256 KiB pending cap, a 192 KiB ACK batch, and a 48 KiB chunk.
//!
//! A [`CreditGrant`] carries the [`CreditWindow`] in force rather than leaving the client
//! to hold its own copy of those numbers. ts-rs exports types and not values, so a
//! hardcoded TypeScript copy would be a second authority on the protocol — which is exactly
//! what D-13 exists to prevent. Sending the window costs a few dozen bytes once per ACK
//! batch, which is to say once per 192 KiB.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One kibibyte, so the constants below read as the design writes them.
const KIB: u32 = 1024;
/// One mebibyte.
const MIB: u32 = 1024 * KIB;

/// The parameters of a credit window.
///
/// Sent to the client rather than agreed by convention, so the daemon can tune the window
/// — for a hidden pane, for a slow renderer — without a protocol change and without the
/// client and the daemon disagreeing about what is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreditWindow {
    /// Bytes a freshly attached stream may send before its first ack.
    pub per_stream_initial: u32,
    /// The most credit one stream may accumulate.
    pub per_stream_max: u32,
    /// Bytes all streams together may send before any ack.
    pub total_initial: u32,
    /// The most credit all streams together may accumulate.
    pub total_max: u32,
    /// The most unacknowledged data the producer may hold queued for one stream.
    pub pending_cap: u32,
    /// The consumer batches acks until it has rendered this much, so a busy stream does not
    /// spend its bandwidth on acknowledgements.
    pub ack_batch: u32,
    /// The largest slice of PTY output the producer puts in one frame.
    pub chunk: u32,
}

impl CreditWindow {
    /// Orca's production values, which §7.3 adopts as Nysia's defaults.
    pub const DEFAULT: Self = Self {
        per_stream_initial: 512 * KIB,
        per_stream_max: 2 * MIB,
        total_initial: 2 * MIB,
        total_max: 8 * MIB,
        pending_cap: 256 * KIB,
        ack_batch: 192 * KIB,
        chunk: 48 * KIB,
    };

    /// Whether the window's parameters can all hold at once.
    ///
    /// A window that grants a stream more than it grants every stream together, or that
    /// batches acks beyond what it will hold pending, deadlocks: the producer stops at the
    /// pending cap and the consumer is still short of enough bytes to ack. Cheap to check
    /// and expensive to debug, so it is checkable here rather than discovered in a stall.
    #[must_use]
    pub const fn is_coherent(self) -> bool {
        self.per_stream_initial <= self.per_stream_max
            && self.total_initial <= self.total_max
            && self.per_stream_initial <= self.total_initial
            && self.per_stream_max <= self.total_max
            && self.ack_batch <= self.pending_cap
            && self.chunk <= self.pending_cap
            && self.chunk > 0
    }
}

impl Default for CreditWindow {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The producer tells the consumer it may send more — daemon to client.
///
/// Counted in payload bytes, like the [`CreditAck`] that replenishes it.
///
/// It names no session. A credit frame rides the stream connection, whose header already
/// carries a [`StreamId`](crate::StreamId) — and two routing keys in one frame is a bug
/// waiting for someone to pick the wrong one, because nothing stops a handle and an id
/// disagreeing. The header is authoritative; this payload is the number and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreditGrant {
    /// How many further payload bytes the producer may send, on top of what it already has.
    pub bytes: u32,
    /// The window in force, so the client never holds its own copy of the constants.
    pub window: CreditWindow,
}

/// The consumer tells the producer how much of its allowance it has rendered — client to
/// daemon.
///
/// Sent after xterm's `write()` callback, not on arrival: the point of the window is to
/// track what has been *rendered*, and a message sitting in a queue has not been. Counted in
/// payload bytes, which is all a renderer ever sees.
///
/// Like [`CreditGrant`], it names no session: the frame header's stream id is the routing
/// key on this connection, and a second one could only ever disagree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreditAck {
    /// Payload bytes rendered since the last ack. Batched to [`CreditWindow::ack_batch`].
    pub bytes: u32,
}

/// The payload of a [`crate::FrameKind::Credit`] frame.
///
/// Both directions share the kind byte, so the payload says which one it is. Which *stream*
/// it is comes from the frame header, in both directions — see
/// [`crate::stream`] for what happens when that id names nothing live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "credit", rename_all = "snake_case")]
#[ts(export)]
pub enum CreditFrame {
    /// Producer to consumer: you may send more.
    Grant(CreditGrant),
    /// Consumer to producer: I have rendered this much.
    Ack(CreditAck),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_numbers_the_design_names() {
        let window = CreditWindow::DEFAULT;
        assert_eq!(window.per_stream_initial, 524_288);
        assert_eq!(window.per_stream_max, 2_097_152);
        assert_eq!(window.total_initial, 2_097_152);
        assert_eq!(window.total_max, 8_388_608);
        assert_eq!(window.pending_cap, 262_144);
        assert_eq!(window.ack_batch, 196_608);
        assert_eq!(window.chunk, 49_152);
        assert_eq!(CreditWindow::default(), window);
    }

    #[test]
    fn the_defaults_are_coherent() {
        assert!(CreditWindow::DEFAULT.is_coherent());
    }

    #[test]
    fn an_incoherent_window_is_recognised_before_it_deadlocks() {
        // Batching acks beyond the pending cap stalls: the producer stops at the cap and
        // the consumer is still short of enough bytes to ack.
        assert!(
            !CreditWindow {
                ack_batch: CreditWindow::DEFAULT.pending_cap + 1,
                ..CreditWindow::DEFAULT
            }
            .is_coherent()
        );
        // One stream may not be granted more than every stream together.
        assert!(
            !CreditWindow {
                per_stream_initial: CreditWindow::DEFAULT.total_initial + 1,
                ..CreditWindow::DEFAULT
            }
            .is_coherent()
        );
        assert!(
            !CreditWindow {
                per_stream_max: CreditWindow::DEFAULT.total_max + 1,
                ..CreditWindow::DEFAULT
            }
            .is_coherent()
        );
        // An initial larger than a maximum never refills.
        assert!(
            !CreditWindow {
                per_stream_initial: CreditWindow::DEFAULT.per_stream_max + 1,
                per_stream_max: CreditWindow::DEFAULT.per_stream_max,
                total_initial: CreditWindow::DEFAULT.total_max,
                ..CreditWindow::DEFAULT
            }
            .is_coherent()
        );
        // A zero chunk sends nothing, forever.
        assert!(
            !CreditWindow {
                chunk: 0,
                ..CreditWindow::DEFAULT
            }
            .is_coherent()
        );
    }

    #[test]
    fn a_grant_carries_the_window_so_the_client_holds_no_copy() {
        let grant = CreditGrant {
            bytes: 196_608,
            window: CreditWindow::DEFAULT,
        };
        let json = serde_json::to_value(grant).unwrap();
        assert_eq!(json["window"]["ackBatch"], 196_608);
        assert_eq!(json["window"]["perStreamInitial"], 524_288);
        // No session key in the payload: the frame header's stream id is the only routing
        // key, so there is nothing here that could disagree with it.
        assert!(json.get("handle").is_none());
        assert!(json.get("streamId").is_none());
        assert_eq!(serde_json::from_value::<CreditGrant>(json).unwrap(), grant);
    }

    #[test]
    fn a_credit_frame_says_which_direction_it_is() {
        let grant = CreditFrame::Grant(CreditGrant {
            bytes: 524_288,
            window: CreditWindow::DEFAULT,
        });
        let ack = CreditFrame::Ack(CreditAck { bytes: 196_608 });
        assert_eq!(serde_json::to_value(grant).unwrap()["credit"], "grant");
        assert_eq!(serde_json::to_value(ack).unwrap()["credit"], "ack");
        for frame in [grant, ack] {
            assert_eq!(
                serde_json::from_value::<CreditFrame>(serde_json::to_value(frame).unwrap())
                    .unwrap(),
                frame
            );
        }
        assert!(
            serde_json::from_value::<CreditFrame>(serde_json::json!({
                "credit": "refund",
                "bytes": 1,
            }))
            .is_err()
        );
    }
}
