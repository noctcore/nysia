//! Terminal output on the wire: length-prefixed binary frames under a credit window.
//!
//! A stream is the daemon writing a session's raw bytes to one client. It runs on its own
//! connection — `role: "stream"` in the handshake — because it is a firehose and control is
//! request/response, and multiplexing them would let a stalled terminal delay a session
//! close.
//!
//! # What the credit window actually does
//!
//! [`crate::rpc`]'s module docs state the direction; this is the mechanism.
//!
//! Each [`StreamSink`] holds an allowance in bytes. Writing an output frame spends it;
//! a [`nysia_proto::CreditAck`] from the client — sent after xterm's `write()` callback, so
//! it means *rendered*, not *received* — replenishes it, capped at
//! [`nysia_proto::CreditWindow::per_stream_max`]. When the allowance reaches zero the sink
//! is [`StreamSink::is_blocked`], the pump in [`crate::rpc::session`] stops reading the pty,
//! and the pressure travels the rest of the way on its own: the bounded queue behind
//! [`crate::pty::PtyOutput`] fills, its reader thread blocks on the send, the kernel pty
//! buffer fills, and `yes` blocks in `write(2)`.
//!
//! Only [`nysia_proto::FrameKind::Output`] frames spend credit. An exit or a bell is not
//! rendered text and is never acked, so charging for one would leak the allowance a few
//! bytes at a time until a long-lived session stalled for no reason anybody could find.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, PoisonError};

use nysia_proto::{
    ClientId, CreditAck, CreditFrame, CreditGrant, CreditWindow, Frame, FrameKind, StreamId,
};

/// How many frames may be queued for one client before the daemon treats it as stalled.
///
/// A second line of defence behind the credit window rather than the primary one: credit is
/// measured in bytes and is what the design specifies, but a client that has gone away
/// without closing its socket acks nothing and would otherwise sit on a full queue forever.
const OUTBOX_FRAMES: usize = 256;

/// What became of a frame handed to a sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// The frame is on its way and the allowance has been spent.
    Sent,
    /// The sink has no allowance or no room. The caller must keep the bytes and retry — and
    /// must stop reading the pty until it can, which is the whole point.
    WouldBlock,
    /// The client is gone. The sink can be forgotten.
    Closed,
}

/// One client's view of one session's output.
#[derive(Debug)]
pub struct StreamSink {
    stream_id: StreamId,
    frames: tokio::sync::mpsc::Sender<Vec<u8>>,
    credit: AtomicI64,
    window: CreditWindow,
    closed: AtomicBool,
}

impl StreamSink {
    /// A sink writing to `frames`, opening with `window`'s initial per-stream allowance.
    #[must_use]
    pub fn new(
        stream_id: StreamId,
        frames: tokio::sync::mpsc::Sender<Vec<u8>>,
        window: CreditWindow,
    ) -> Self {
        Self {
            stream_id,
            frames,
            credit: AtomicI64::new(i64::from(window.per_stream_initial)),
            window,
            closed: AtomicBool::new(false),
        }
    }

    /// The id the daemon assigned this stream.
    #[must_use]
    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    /// The window in force.
    #[must_use]
    pub fn window(&self) -> CreditWindow {
        self.window
    }

    /// Bytes the daemon may still send before it must stop reading.
    #[must_use]
    pub fn credit(&self) -> i64 {
        self.credit.load(Ordering::Acquire)
    }

    /// Whether the allowance is spent.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.credit() <= 0
    }

    /// Whether the client has gone.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.frames.is_closed()
    }

    /// Mark the sink closed, so the pump stops counting it.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Apply a client's ack, returning the allowance that results.
    ///
    /// Saturating at [`CreditWindow::per_stream_max`]: a client that acked more than it was
    /// ever sent — whether by a bug or on purpose — must not be able to talk the daemon into
    /// an unbounded allowance, which would turn the window off altogether.
    pub fn replenish(&self, bytes: u32) -> i64 {
        let ceiling = i64::from(self.window.per_stream_max);
        let mut current = self.credit.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(i64::from(bytes)).min(ceiling);
            match self.credit.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(seen) => current = seen,
            }
        }
    }

    /// Send `frame`, spending allowance for output bytes.
    ///
    /// Encoding happens here rather than in the caller so the allowance is charged for the
    /// bytes that actually go on the wire, header included — a million one-byte frames cost
    /// ten megabytes of socket and would otherwise be charged one megabyte of credit.
    pub fn send(&self, frame: &Frame) -> SendOutcome {
        if self.is_closed() {
            return SendOutcome::Closed;
        }
        let spends = frame.kind == FrameKind::Output;
        if spends && self.is_blocked() {
            return SendOutcome::WouldBlock;
        }
        let Ok(encoded) = nysia_proto::encode(frame) else {
            // A frame this daemon composed that will not encode is a bug in the daemon, not
            // a state the client can do anything about. Dropping it keeps the session alive;
            // the trace is where it is answered for.
            tracing::error!(
                stream = frame.stream.get(),
                kind = frame.kind.as_str(),
                payload = frame.payload.len(),
                "dropped a frame that exceeds the wire's payload ceiling"
            );
            return SendOutcome::Sent;
        };
        let cost = i64::try_from(encoded.len()).unwrap_or(i64::MAX);
        match self.frames.try_send(encoded) {
            Ok(()) => {
                if spends {
                    self.credit.fetch_sub(cost, Ordering::AcqRel);
                }
                SendOutcome::Sent
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => SendOutcome::WouldBlock,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.close();
                SendOutcome::Closed
            }
        }
    }

    /// The opening grant, which is how the client learns the window without holding a copy
    /// of the constants.
    #[must_use]
    pub fn opening_grant(&self) -> Frame {
        let grant = CreditFrame::Grant(CreditGrant {
            bytes: self.window.per_stream_initial,
            window: self.window,
        });
        credit_frame(self.stream_id, &grant)
    }
}

/// Encode a credit frame for `stream`.
///
/// The stream id lives in the frame header and nowhere else — proto is explicit that two
/// routing keys in one frame is a bug waiting for somebody to pick the wrong one.
fn credit_frame(stream: StreamId, credit: &CreditFrame) -> Frame {
    let payload = serde_json::to_vec(credit).unwrap_or_default();
    Frame::new(FrameKind::Credit, stream, payload)
}

/// Every stream the daemon is writing, and every client connection able to carry one.
///
/// Keyed by [`ClientId`] because that is what pairs a client's two connections. The id is
/// *not* an authority — §3.2 proves identity from peer credentials — and it is not used as
/// one here: it routes frames to a connection that has already been authenticated, and the
/// worst a peer can do by naming somebody else's id is take delivery of its own frames on
/// its own socket, because a hub is replaced by whoever last bound it.
#[derive(Debug)]
pub struct StreamRegistry {
    inner: Mutex<Inner>,
}

impl Default for StreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry's contents, behind one lock.
///
/// `StreamId` has no `Default` on purpose — proto reserves zero for "no stream" — so the
/// starting id is named here rather than derived.
#[derive(Debug)]
struct Inner {
    /// The outbox of each client that has a stream connection open.
    hubs: HashMap<String, tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// Every live sink, by stream id, so an ack can find the one it belongs to.
    sinks: HashMap<u32, Arc<StreamSink>>,
    /// The next id to assign. Never reused within a daemon's life.
    next_id: StreamId,
}

impl StreamRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                hubs: HashMap::new(),
                sinks: HashMap::new(),
                next_id: StreamId::FIRST,
            }),
        }
    }

    /// Register a client's stream connection and hand back its outbox.
    ///
    /// Binding twice replaces the first: a client that reconnected its stream connection
    /// wants the new one, and leaving the old sender in place would write every frame into a
    /// socket nobody is reading.
    pub fn bind(&self, client: &ClientId) -> tokio::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = tokio::sync::mpsc::channel(OUTBOX_FRAMES);
        self.lock().hubs.insert(client.as_str().to_owned(), tx);
        rx
    }

    /// Forget a client's stream connection and close every sink that fed it.
    pub fn unbind(&self, client: &ClientId) {
        let mut inner = self.lock();
        inner.hubs.remove(client.as_str());
        // The sinks go too. A sink whose connection is gone can never be acked, so leaving
        // it attached would stall the session it feeds for as long as the daemon lives —
        // which is precisely the failure D-1 promises not to have.
        inner.sinks.retain(|_, sink| {
            if sink.is_closed() {
                sink.close();
                false
            } else {
                true
            }
        });
    }

    /// Open a stream for `client`, or `None` when it has no stream connection bound.
    ///
    /// Refusing rather than queueing is deliberate. A sink with nowhere to write fills, runs
    /// out of credit, and stalls the session — so a client that attached before opening its
    /// stream connection would silently freeze its own terminal. Being told to open the
    /// connection first is a better outcome than a pane that never paints.
    pub fn attach(&self, client: &ClientId) -> Option<Arc<StreamSink>> {
        let mut inner = self.lock();
        let frames = inner.hubs.get(client.as_str())?.clone();
        let stream_id = inner.next_id;
        inner.next_id = stream_id.next()?;
        let sink = Arc::new(StreamSink::new(stream_id, frames, CreditWindow::DEFAULT));
        inner.sinks.insert(stream_id.get(), Arc::clone(&sink));
        Some(sink)
    }

    /// Close and forget a stream, reporting whether it was there.
    pub fn detach(&self, stream_id: StreamId) -> bool {
        match self.lock().sinks.remove(&stream_id.get()) {
            Some(sink) => {
                sink.close();
                true
            }
            None => false,
        }
    }

    /// Apply a credit frame from a client.
    ///
    /// A [`CreditFrame::Grant`] arriving from a client is ignored rather than acted on: the
    /// daemon holds the window, and a peer that could hand itself an allowance would be able
    /// to turn the backpressure off from the outside.
    pub fn apply_credit(&self, stream_id: StreamId, credit: &CreditFrame) -> bool {
        let CreditFrame::Ack(CreditAck { bytes }) = credit else {
            tracing::debug!(
                stream = stream_id.get(),
                "ignored a credit grant from a client; the daemon owns the window"
            );
            return false;
        };
        let Some(sink) = self.lock().sinks.get(&stream_id.get()).map(Arc::clone) else {
            return false;
        };
        sink.replenish(*bytes);
        true
    }

    /// How many streams are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().sinks.len()
    }

    /// Whether no stream is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The lock, treating poisoning as recoverable.
    ///
    /// A panic while the registry was locked leaves it consistent — every operation on it is
    /// a map insert or remove — and refusing to serve every later stream because one thread
    /// panicked would turn a bug into an outage.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(name: &str) -> ClientId {
        name.parse().expect("a well-formed client id")
    }

    fn output(stream: StreamId, bytes: usize) -> Frame {
        Frame::new(FrameKind::Output, stream, vec![b'x'; bytes])
    }

    #[tokio::test]
    async fn a_sink_spends_its_allowance_and_then_refuses_to_send() {
        // The whole backpressure claim in one test: once the allowance is gone the daemon is
        // told to stop, rather than buffering on the client's behalf.
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let window = CreditWindow::DEFAULT;
        let sink = StreamSink::new(StreamId::FIRST, tx, window);

        let chunk = window.chunk as usize;
        let mut sent = 0usize;
        while sink.send(&output(StreamId::FIRST, chunk)) == SendOutcome::Sent {
            sent += chunk;
            assert!(
                sent <= window.per_stream_initial as usize + chunk,
                "ran away"
            );
        }
        assert!(sink.is_blocked(), "the allowance should be spent");
        assert_eq!(
            sink.send(&output(StreamId::FIRST, chunk)),
            SendOutcome::WouldBlock
        );

        // And an ack lets it go again, which is what makes the stall temporary rather than
        // terminal.
        rx.recv().await.expect("a frame was queued");
        sink.replenish(window.ack_batch);
        assert!(!sink.is_blocked());
        assert_eq!(sink.send(&output(StreamId::FIRST, 16)), SendOutcome::Sent);
    }

    #[tokio::test]
    async fn a_frame_that_is_not_output_never_spends_credit() {
        // Exits and bells are not rendered text and are never acked. Charging for them would
        // leak the allowance a few bytes at a time until a long-lived session stalled.
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let sink = StreamSink::new(StreamId::FIRST, tx, CreditWindow::DEFAULT);
        let before = sink.credit();
        assert_eq!(
            sink.send(&Frame::empty(FrameKind::Bell, StreamId::FIRST)),
            SendOutcome::Sent
        );
        assert_eq!(
            sink.send(&Frame::new(
                FrameKind::Exit,
                StreamId::FIRST,
                b"{}".to_vec()
            )),
            SendOutcome::Sent
        );
        assert_eq!(sink.credit(), before);
    }

    #[tokio::test]
    async fn an_ack_larger_than_the_window_cannot_turn_the_window_off() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let window = CreditWindow::DEFAULT;
        let sink = StreamSink::new(StreamId::FIRST, tx, window);
        sink.replenish(u32::MAX);
        assert_eq!(sink.credit(), i64::from(window.per_stream_max));
    }

    #[tokio::test]
    async fn a_closed_client_stops_the_sink_rather_than_stalling_it() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let sink = StreamSink::new(StreamId::FIRST, tx, CreditWindow::DEFAULT);
        drop(rx);
        assert_eq!(sink.send(&output(StreamId::FIRST, 8)), SendOutcome::Closed);
        assert!(sink.is_closed());
    }

    #[tokio::test]
    async fn attaching_needs_a_stream_connection_and_hands_back_rising_ids() {
        let registry = StreamRegistry::new();
        let me = client("nysia-test");
        assert!(
            registry.attach(&me).is_none(),
            "attaching with no stream connection must be refused, not queued"
        );

        let _outbox = registry.bind(&me);
        let first = registry.attach(&me).expect("attaches");
        let second = registry.attach(&me).expect("attaches");
        assert_eq!(first.stream_id(), StreamId::FIRST);
        assert_ne!(first.stream_id(), second.stream_id());
        assert_eq!(registry.len(), 2);

        assert!(registry.detach(first.stream_id()));
        assert!(
            !registry.detach(first.stream_id()),
            "detaching twice is not an error but is not a second success either"
        );
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn an_ack_finds_its_stream_and_a_grant_from_a_client_is_ignored() {
        let registry = StreamRegistry::new();
        let me = client("nysia-test");
        let _outbox = registry.bind(&me);
        let sink = registry.attach(&me).expect("attaches");
        let id = sink.stream_id();

        sink.send(&output(id, 4096));
        let spent = sink.credit();
        assert!(registry.apply_credit(id, &CreditFrame::Ack(CreditAck { bytes: 4096 })));
        assert!(sink.credit() > spent);

        // A client granting itself credit would be turning the backpressure off from the
        // outside, so the daemon does not act on one.
        let before = sink.credit();
        assert!(!registry.apply_credit(
            id,
            &CreditFrame::Grant(CreditGrant {
                bytes: u32::MAX,
                window: CreditWindow::DEFAULT,
            })
        ));
        assert_eq!(sink.credit(), before);

        // An ack for a stream that is gone is answered with `false`, never a panic: the
        // detach race is routine, and proto says most such frames are simply discarded.
        registry.detach(id);
        assert!(!registry.apply_credit(id, &CreditFrame::Ack(CreditAck { bytes: 1 })));
    }

    #[tokio::test]
    async fn the_opening_grant_carries_the_window_so_the_client_holds_no_constants() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let sink = StreamSink::new(StreamId::FIRST, tx, CreditWindow::DEFAULT);
        let frame = sink.opening_grant();
        assert_eq!(frame.kind, FrameKind::Credit);
        assert_eq!(frame.stream, StreamId::FIRST);
        let credit: CreditFrame = serde_json::from_slice(&frame.payload).expect("decodes");
        match credit {
            CreditFrame::Grant(grant) => {
                assert_eq!(grant.window, CreditWindow::DEFAULT);
                assert_eq!(grant.bytes, CreditWindow::DEFAULT.per_stream_initial);
            }
            CreditFrame::Ack(_) => panic!("the opening frame is a grant"),
        }
    }
}
