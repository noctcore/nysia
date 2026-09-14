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
//! Each [`StreamSink`] holds an allowance in bytes. Writing an output frame spends its
//! **payload** length; a [`nysia_proto::CreditAck`] from the client — sent after xterm's
//! `write()` callback, so it means *rendered*, not *received* — replenishes it, capped at
//! [`nysia_proto::CreditWindow::per_stream_max`]. When the allowance reaches zero the sink
//! is [`StreamSink::is_blocked`], the pump in [`crate::rpc::session`] stops reading the pty,
//! and the pressure travels the rest of the way on its own: the bounded queue behind
//! [`crate::pty::PtyOutput`] fills, its reader thread blocks on the send, the kernel pty
//! buffer fills, and `yes` blocks in `write(2)`.
//!
//! Payload bytes, not encoded bytes, and the two ends have to agree on that or the window
//! leaks. The nine-byte header is transport overhead the consumer never receives as
//! content: the ack is emitted by the code that has just written a payload into a terminal,
//! so it can only ever count payloads. Charging the header here would drain the allowance
//! nine bytes per frame — roughly 58,000 frames from a full default window — and strand a
//! long-lived session permanently, for a reason no log line would name.
//!
//! Only [`nysia_proto::FrameKind::Output`] frames spend credit. An exit or a bell is not
//! rendered text and is never acked, so charging for one would leak the allowance a few
//! bytes at a time until a long-lived session stalled for no reason anybody could find.
//!
//! # A connection, not a client id
//!
//! Everything below is keyed by [`ConnectionKey`] — one stream socket — rather than by
//! [`ClientId`]. A webview reload opens a *second* stream connection under the same client
//! id while the first is still open, and on Windows the reader parked on the first cannot be
//! interrupted: it wakes on the next byte, minutes later, and unbinds. Keyed by client id
//! that unbind closes the sinks of the connection that replaced it, and the window goes deaf
//! while still believing it is attached. Keyed by connection it closes exactly what it
//! opened, which is the only version of the rule that is safe to run late.
//!
//! Stream ids are numbered **per connection** for the same reason, starting at
//! [`StreamId::FIRST`]. `nysia-proto` says an id is scoped to one stream connection, and its
//! discard-versus-drop rule is defined against "the next id to assign on this connection" —
//! a watermark a client can only know if the daemon counts where the client can see it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
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

/// One stream connection, for the life of the daemon.
///
/// Two connections from the same client are different keys and share nothing — no outbox, no
/// stream ids, no sinks. That is what makes a superseded connection's teardown safe to
/// arrive whenever it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionKey(u64);

impl ConnectionKey {
    /// The key as the bare number it is, for logs.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ConnectionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a stream connection gets when it binds.
#[derive(Debug)]
pub struct BoundStream {
    /// This connection's key, which every later call about it names.
    pub key: ConnectionKey,
    /// The frames to write to the socket, in order.
    pub outbox: tokio::sync::mpsc::Receiver<Vec<u8>>,
    /// Raised when a newer connection has taken this client id over.
    ///
    /// The reader must select on it. A parked named-pipe read cannot be cancelled on
    /// Windows, so without a second wake path a superseded connection holds its task, its
    /// slot in the client count, and its socket — and the client's own reader stays parked
    /// waiting for an EOF that only arrives when this side lets go.
    pub superseded: Arc<tokio::sync::Notify>,
}

/// A stream the daemon has just opened, and the connection it rides.
#[derive(Debug, Clone)]
pub struct AttachedStream {
    /// The connection that owns it. Named here so a failed attach rolls back against the
    /// connection it actually used — by the time it fails, the client id may name a newer
    /// one.
    pub connection: ConnectionKey,
    /// The sink the session writes into.
    pub sink: Arc<StreamSink>,
}

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
    delivered: AtomicUsize,
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
            delivered: AtomicUsize::new(0),
        }
    }

    /// The id the daemon assigned this stream, on its own connection.
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

    /// How far into the pump's current coalesced buffer this sink has got.
    ///
    /// The pump coalesces once and offers the same buffer to every sink it feeds, so "how far
    /// did this one get" is per-sink state and has to live here. Without it a sink that stops
    /// halfway on a spent allowance makes the whole buffer be re-offered from the start next
    /// turn: bytes already sent are sent again, and under sustained backpressure it never
    /// converges, because the buffer grows to the pump's flush ceiling while each turn
    /// delivers only an allowance's worth of its head. That is the shape of a `yes` flood
    /// once the opening window is spent, which is to say the case the window exists for.
    #[must_use]
    pub fn delivered(&self) -> usize {
        self.delivered.load(Ordering::Acquire)
    }

    /// Record how far into that buffer this sink has now got.
    pub fn set_delivered(&self, bytes: usize) {
        self.delivered.store(bytes, Ordering::Release);
    }

    /// Send `frame`, spending allowance for the output bytes it carries.
    ///
    /// The allowance is charged the **payload** length, never the encoded length. The client
    /// acks from inside its renderer, which has only ever seen payloads, so charging the
    /// nine-byte header here would put the two ends on different units and drain the window
    /// by nine bytes for every frame that ever flows. The socket's own overhead is bounded
    /// separately, by [`OUTBOX_FRAMES`].
    pub fn send(&self, frame: &Frame) -> SendOutcome {
        if self.is_closed() {
            return SendOutcome::Closed;
        }
        let spends = frame.kind == FrameKind::Output;
        if spends && self.is_blocked() {
            return SendOutcome::WouldBlock;
        }
        let cost = i64::try_from(frame.payload.len()).unwrap_or(i64::MAX);
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

/// One stream connection: its outbox, its own id counter, and its own sinks.
#[derive(Debug)]
struct StreamConnection {
    /// The client id this connection announced, so a bind can find the one it supersedes and
    /// an unbind can tell whether it is still the current one.
    client: String,
    /// Frames waiting to be written to this socket.
    frames: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// The next id to assign **on this connection**, which is also the watermark
    /// [`StreamId::classify_unattached`] is defined against.
    next_id: StreamId,
    /// Every live sink on this connection, so an ack can find the one it belongs to and a
    /// disconnect closes exactly these.
    ///
    /// Held here rather than inferred from whether a sink's channel has closed: that
    /// inference is true only *eventually*, because the receiver is dropped when the
    /// connection's write task is actually cancelled rather than when the cancellation is
    /// requested. A session stalled on a sink nobody will ever ack is the failure D-1
    /// promises not to have, so it is not left to a race.
    sinks: HashMap<u32, Arc<StreamSink>>,
    /// Raised once, when a newer connection takes this client id over.
    superseded: Arc<tokio::sync::Notify>,
}

impl StreamConnection {
    /// Close every sink on this connection. It can never be acked again.
    fn close(&mut self) {
        for (_, sink) in self.sinks.drain() {
            sink.close();
        }
    }
}

/// Every stream the daemon is writing, and every connection able to carry one.
///
/// Keyed by [`ConnectionKey`]; a [`ClientId`] only names the connection that is *currently*
/// serving it. The id is not an authority — §3.2 proves identity from peer credentials — and
/// is not used as one here: it routes frames to a connection that has already been
/// authenticated, and the worst a peer can do by naming somebody else's id is take delivery
/// of its own frames on its own socket.
#[derive(Debug)]
pub struct StreamRegistry {
    window: CreditWindow,
    inner: Mutex<Inner>,
}

impl Default for StreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry's contents, behind one lock.
#[derive(Debug)]
struct Inner {
    /// Every stream connection this daemon has open.
    connections: HashMap<ConnectionKey, StreamConnection>,
    /// The connection currently serving each client id: the one a fresh attach uses.
    current: HashMap<String, ConnectionKey>,
    /// The next connection key. Never reused within a daemon's life, so a teardown that
    /// arrives late cannot name a connection that has since taken its number.
    next_connection: u64,
}

impl StreamRegistry {
    /// An empty registry serving [`CreditWindow::DEFAULT`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(CreditWindow::DEFAULT)
    }

    /// An empty registry serving `window`.
    ///
    /// The window is the daemon's to choose — `nysia-proto` sends it to the client in the
    /// opening grant precisely so it can be tuned without a protocol change and without the
    /// two ends holding separate copies of the constants. An incoherent window would
    /// deadlock rather than merely misbehave, so one is refused here and
    /// [`CreditWindow::DEFAULT`] is served instead.
    #[must_use]
    pub fn with_window(window: CreditWindow) -> Self {
        let window = if window.is_coherent() {
            window
        } else {
            tracing::error!("ignored an incoherent credit window; serving the defaults");
            CreditWindow::DEFAULT
        };
        Self {
            window,
            inner: Mutex::new(Inner {
                connections: HashMap::new(),
                current: HashMap::new(),
                next_connection: 1,
            }),
        }
    }

    /// The window every sink this registry opens is given.
    #[must_use]
    pub fn window(&self) -> CreditWindow {
        self.window
    }

    /// Register a stream connection for `client` and hand back its outbox.
    ///
    /// A connection that binds while the same client id already has one **supersedes** it:
    /// the older connection's sinks are closed, its outbox is dropped, and its reader is
    /// woken. That is the webview reload — a second socket under one id — and the older
    /// socket can never ack again, so a sink left attached to it would spend its allowance
    /// and stall the session it feeds. The older connection's own teardown still runs, and
    /// still closes only what it owned, so it does not matter how late it arrives.
    pub fn bind(&self, client: &ClientId) -> BoundStream {
        let (tx, rx) = tokio::sync::mpsc::channel(OUTBOX_FRAMES);
        let superseded = Arc::new(tokio::sync::Notify::new());
        let mut inner = self.lock();
        let key = ConnectionKey(inner.next_connection);
        inner.next_connection = inner.next_connection.saturating_add(1);

        if let Some(previous) = inner.current.insert(client.as_str().to_owned(), key)
            && let Some(mut connection) = inner.connections.remove(&previous)
        {
            connection.close();
            // A permit rather than a broadcast: the superseded reader may not be waiting yet,
            // and a wake it misses is a task parked for the life of the daemon.
            connection.superseded.notify_one();
            tracing::debug!(
                client = client.as_str(),
                superseded = previous.get(),
                replacement = key.get(),
                "a second stream connection took over this client id"
            );
        }

        inner.connections.insert(
            key,
            StreamConnection {
                client: client.as_str().to_owned(),
                frames: tx,
                next_id: StreamId::FIRST,
                sinks: HashMap::new(),
                superseded: Arc::clone(&superseded),
            },
        );
        BoundStream {
            key,
            outbox: rx,
            superseded,
        }
    }

    /// Forget one stream connection and close every sink that fed it.
    ///
    /// Only its own. A connection that was superseded while its reader was parked is torn
    /// down here whenever that reader finally wakes, and by then the client id belongs to
    /// somebody else — so the client id is cleared only while it still names this connection.
    pub fn unbind(&self, key: ConnectionKey) {
        let mut inner = self.lock();
        let Some(mut connection) = inner.connections.remove(&key) else {
            return;
        };
        if inner.current.get(&connection.client) == Some(&key) {
            inner.current.remove(&connection.client);
        }
        connection.close();
    }

    /// Open a stream for `client`, or `None` when it has no stream connection bound.
    ///
    /// Refusing rather than queueing is deliberate. A sink with nowhere to write fills, runs
    /// out of credit, and stalls the session — so a client that attached before opening its
    /// stream connection would silently freeze its own terminal. Being told to open the
    /// connection first is a better outcome than a pane that never paints.
    pub fn attach(&self, client: &ClientId) -> Option<AttachedStream> {
        let window = self.window;
        let mut inner = self.lock();
        let key = *inner.current.get(client.as_str())?;
        let connection = inner.connections.get_mut(&key)?;
        let stream_id = connection.next_id;
        connection.next_id = stream_id.next()?;
        let sink = Arc::new(StreamSink::new(
            stream_id,
            connection.frames.clone(),
            window,
        ));
        connection.sinks.insert(stream_id.get(), Arc::clone(&sink));
        Some(AttachedStream {
            connection: key,
            sink,
        })
    }

    /// Close and forget a stream on `client`'s current connection, reporting whether it was
    /// there.
    pub fn detach(&self, client: &ClientId, stream_id: StreamId) -> bool {
        let key = { self.lock().current.get(client.as_str()).copied() };
        key.is_some_and(|key| self.detach_on(key, stream_id))
    }

    /// Close and forget a stream on one named connection, reporting whether it was there.
    ///
    /// By key rather than by client id, for the rollback after an attach whose answer never
    /// reached the client: the connection that was attached is the one that has to be undone,
    /// even if a newer one has taken the client id over since.
    pub fn detach_on(&self, key: ConnectionKey, stream_id: StreamId) -> bool {
        let mut inner = self.lock();
        let Some(connection) = inner.connections.get_mut(&key) else {
            return false;
        };
        match connection.sinks.remove(&stream_id.get()) {
            Some(sink) => {
                sink.close();
                true
            }
            None => false,
        }
    }

    /// Apply a credit frame that arrived on one named connection.
    ///
    /// Named, because an id means nothing off its own connection: two clients — or one
    /// client across a reload — both hold [`StreamId::FIRST`], and an unscoped lookup would
    /// credit whichever of them the map happened to hold.
    ///
    /// A [`CreditFrame::Grant`] arriving from a client is ignored rather than acted on: the
    /// daemon is the producer, it holds the window, and a consumer that could hand itself an
    /// allowance would be able to turn the backpressure off from the outside.
    pub fn apply_credit(
        &self,
        key: ConnectionKey,
        stream_id: StreamId,
        credit: &CreditFrame,
    ) -> bool {
        let CreditFrame::Ack(CreditAck { bytes }) = credit else {
            tracing::debug!(
                connection = key.get(),
                stream = stream_id.get(),
                "ignored a credit grant from a client; the daemon owns the window"
            );
            return false;
        };
        let sink = {
            let inner = self.lock();
            inner
                .connections
                .get(&key)
                .and_then(|connection| connection.sinks.get(&stream_id.get()))
                .map(Arc::clone)
        };
        let Some(sink) = sink else {
            return false;
        };
        sink.replenish(*bytes);
        true
    }

    /// The next id `client`'s current connection would hand out, which is the watermark its
    /// discard-versus-drop rule is defined against.
    #[must_use]
    pub fn next_stream_id(&self, client: &ClientId) -> Option<StreamId> {
        let inner = self.lock();
        let key = inner.current.get(client.as_str())?;
        inner.connections.get(key).map(|state| state.next_id)
    }

    /// How many stream connections this registry has bound, ever.
    ///
    /// Monotonic, and the only unambiguous answer to "has that socket finished binding?" — a
    /// count cannot answer it, because a supersede moves that in the opposite direction.
    ///
    /// A connection is bound *before* its hello is answered, so by the time a client's connect
    /// returns this has already counted it. That ordering is the contract the client relies
    /// on: it may attach the instant it is told the connection exists, and the attach must
    /// find this connection rather than the one it replaced.
    #[must_use]
    pub fn bound(&self) -> u64 {
        self.lock().next_connection.saturating_sub(1)
    }

    /// How many stream connections are open.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.lock().connections.len()
    }

    /// How many streams are open, across every connection.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock()
            .connections
            .values()
            .map(|connection| connection.sinks.len())
            .sum()
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
    async fn credit_is_charged_in_payload_bytes_so_an_ack_returns_exactly_what_was_spent() {
        // The units have to match the consumer's. It acks from inside the renderer, which has
        // only ever seen payloads, so a header charged here drains the window nine bytes per
        // frame until a long session stalls for good.
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let window = CreditWindow::DEFAULT;
        let sink = StreamSink::new(StreamId::FIRST, tx, window);
        let opening = sink.credit();

        let payload = 700i64;
        let frames = 32i64;
        for _ in 0..frames {
            assert_eq!(
                sink.send(&output(StreamId::FIRST, payload as usize)),
                SendOutcome::Sent
            );
        }
        assert_eq!(
            sink.credit(),
            opening - payload * frames,
            "the allowance should have moved by the payloads and nothing else"
        );

        // What the client acks is what it rendered, and that returns the window exactly.
        for _ in 0..frames {
            sink.replenish(payload as u32);
        }
        assert_eq!(
            sink.credit(),
            opening,
            "a fully acked stream must be back to a full allowance, not nine bytes short per \
             frame"
        );
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
    async fn a_client_that_disconnects_takes_its_own_streams_and_nobody_else_s() {
        // The sink of a client that has gone can never be acked, so leaving it attached would
        // spend its allowance and stall the session it feeds for the daemon's whole life.
        let registry = StreamRegistry::new();
        let mine = client("mine");
        let theirs = client("theirs");
        let my_stream = registry.bind(&mine);
        let _their_stream = registry.bind(&theirs);

        let my_sink = registry.attach(&mine).expect("attaches").sink;
        let their_sink = registry.attach(&theirs).expect("attaches").sink;
        assert_eq!(registry.len(), 2);

        registry.unbind(my_stream.key);
        assert!(my_sink.is_closed(), "my stream should have gone with me");
        assert!(
            !their_sink.is_closed(),
            "somebody else's stream must not go with me"
        );
        assert_eq!(registry.len(), 1);
        assert!(
            registry.attach(&mine).is_none(),
            "and my hub should be gone too"
        );
    }

    #[tokio::test]
    async fn ids_are_numbered_per_connection_and_start_at_first_on_each() {
        // Proto scopes an id to one stream connection, and defines the client's
        // discard-versus-drop rule against "the next id to assign on this connection". A
        // daemon-global counter makes that watermark unknowable from the client's side.
        let registry = StreamRegistry::new();
        let me = client("nysia-test");
        let theirs = client("somebody-else");
        let _mine = registry.bind(&me);
        let _theirs = registry.bind(&theirs);

        let first = registry.attach(&me).expect("attaches");
        let second = registry.attach(&me).expect("attaches");
        assert_eq!(first.sink.stream_id(), StreamId::FIRST);
        assert_eq!(second.sink.stream_id(), StreamId(2));

        // Another connection starts its own count. Two live sinks both called 1 is the point:
        // an id is a routing label on one socket, never an identity.
        let ours = registry.attach(&theirs).expect("attaches");
        assert_eq!(ours.sink.stream_id(), StreamId::FIRST);
        assert_ne!(ours.connection, first.connection);
    }

    #[tokio::test]
    async fn a_second_connection_under_one_client_id_does_not_take_the_first_one_s_down() {
        // A webview reload. The superseded reader cannot be interrupted on Windows, so its
        // teardown arrives whenever it arrives — and must close only what it opened.
        let registry = StreamRegistry::new();
        let me = client("nysia-test");

        let first = registry.bind(&me);
        let stale = registry.attach(&me).expect("attaches").sink;

        let second = registry.bind(&me);
        assert_ne!(first.key, second.key);
        assert!(
            stale.is_closed(),
            "a superseded connection can never ack again, so its sinks must not stay attached"
        );

        let live = registry.attach(&me).expect("attaches");
        assert_eq!(
            live.sink.stream_id(),
            StreamId::FIRST,
            "a fresh connection counts from the start"
        );
        assert_eq!(live.connection, second.key);

        // The late teardown. It must not touch the connection that replaced it.
        registry.unbind(first.key);
        assert!(!live.sink.is_closed(), "the live stream must have survived");
        assert!(
            registry.attach(&me).is_some(),
            "and the client must still have somewhere to route output to"
        );
    }

    #[tokio::test]
    async fn a_superseded_connection_is_woken_rather_than_left_parked() {
        let registry = StreamRegistry::new();
        let me = client("nysia-test");
        let first = registry.bind(&me);
        let waiting = Arc::clone(&first.superseded);
        let _second = registry.bind(&me);
        // A permit, not a broadcast: this waiter registers after the notify and still gets it.
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting.notified())
            .await
            .expect("a superseded connection is told, or its reader parks forever");
    }

    #[tokio::test]
    async fn attaching_needs_a_stream_connection_and_detaching_is_scoped_to_one() {
        let registry = StreamRegistry::new();
        let me = client("nysia-test");
        assert!(
            registry.attach(&me).is_none(),
            "attaching with no stream connection must be refused, not queued"
        );

        let _stream = registry.bind(&me);
        let first = registry.attach(&me).expect("attaches");
        let _second = registry.attach(&me).expect("attaches");
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.next_stream_id(&me), Some(StreamId(3)));

        assert!(registry.detach(&me, first.sink.stream_id()));
        assert!(
            !registry.detach(&me, first.sink.stream_id()),
            "detaching twice is not an error but is not a second success either"
        );
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.next_stream_id(&me),
            Some(StreamId(3)),
            "a detached id is retired, never recycled"
        );
    }

    #[tokio::test]
    async fn an_ack_finds_its_stream_on_its_own_connection_and_a_grant_is_ignored() {
        let registry = StreamRegistry::new();
        let me = client("nysia-test");
        let theirs = client("somebody-else");
        let mine = registry.bind(&me);
        let ours = registry.bind(&theirs);
        let attached = registry.attach(&me).expect("attaches");
        let sink = Arc::clone(&attached.sink);
        let id = sink.stream_id();
        let elsewhere = registry.attach(&theirs).expect("attaches").sink;
        assert_eq!(elsewhere.stream_id(), id, "both connections count from one");

        sink.send(&output(id, 4096));
        let spent = sink.credit();
        assert!(registry.apply_credit(mine.key, id, &CreditFrame::Ack(CreditAck { bytes: 4096 })));
        assert!(sink.credit() > spent);

        // The same id on somebody else's connection is somebody else's stream.
        elsewhere.send(&output(id, 4096));
        let theirs_spent = elsewhere.credit();
        assert!(registry.apply_credit(ours.key, id, &CreditFrame::Ack(CreditAck { bytes: 4096 })));
        assert!(elsewhere.credit() > theirs_spent);

        // A client granting itself credit would be turning the backpressure off from the
        // outside, so the daemon does not act on one.
        let before = sink.credit();
        assert!(!registry.apply_credit(
            mine.key,
            id,
            &CreditFrame::Grant(CreditGrant {
                bytes: u32::MAX,
                window: CreditWindow::DEFAULT,
            })
        ));
        assert_eq!(sink.credit(), before);

        // An ack for a stream that is gone is answered with `false`, never a panic: the
        // detach race is routine, and proto says most such frames are simply discarded.
        registry.detach(&me, id);
        assert!(!registry.apply_credit(mine.key, id, &CreditFrame::Ack(CreditAck { bytes: 1 })));
    }

    #[tokio::test]
    async fn a_registry_serves_the_window_it_was_given_and_refuses_an_incoherent_one() {
        let tight = CreditWindow {
            per_stream_initial: 2 * 1024,
            per_stream_max: 4 * 1024,
            total_initial: 2 * 1024,
            total_max: 4 * 1024,
            pending_cap: 2 * 1024,
            ack_batch: 512,
            chunk: 256,
        };
        assert!(tight.is_coherent());
        let registry = StreamRegistry::with_window(tight);
        let me = client("nysia-test");
        let _stream = registry.bind(&me);
        let sink = registry.attach(&me).expect("attaches").sink;
        assert_eq!(sink.window(), tight);
        assert_eq!(sink.credit(), i64::from(tight.per_stream_initial));

        // An incoherent window deadlocks rather than merely misbehaving, so it is refused at
        // the door instead of discovered in a stall.
        let broken = CreditWindow {
            chunk: 0,
            ..CreditWindow::DEFAULT
        };
        assert_eq!(
            StreamRegistry::with_window(broken).window(),
            CreditWindow::DEFAULT
        );
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
