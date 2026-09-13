//! The output plane: length-prefixed binary frames under a credit window.
//!
//! One thread reads the stream socket, checks each frame against the [`CreditLedger`], and
//! hands it to the [`Dispatcher`] for the webview. Render reports arrive from the
//! `terminal_ack` command, and returning that credit upstream is what keeps output flowing.
//!
//! Which session a frame belongs to is the stream id in its header, assigned by the daemon
//! in answer to a `stream_attach` on the control plane. There is no local table: the one in
//! [`crate::state::Client`] is shared with this thread, because two tables numbering the
//! same sessions independently is exactly how a closed pane took every other pane's output
//! with it.
//!
//! ## Three things this loop has to get right
//!
//! **It must never block where it cannot be woken.** The reader owns the only handle to the
//! socket, so nothing outside can interrupt a `read` in progress. Shutdown therefore signals
//! and returns; it never joins. A caller that joined would wait for a read that only the
//! daemon can end, and if it held a lock while waiting the whole window would stop — which
//! is what an earlier version of this file did.
//!
//! **It must stop reading when the *right* balance runs out.** [`CreditLedger::is_blocked`]
//! asks whether any attached stream can take another chunk, not whether the shared total is
//! spent. With one session those differ by three quarters of the budget, and the wrong
//! question wedges the session on its first burst. See [`crate::channel::credit`].
//!
//! **It must write grants from the same pass that notices they are owed.** Credit is
//! returned by this thread and no other, so anything that only happened on the render report
//! that filled a batch would never happen at all for a burst that fell short of one.

use std::io::{BufReader, Read};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nysia_proto::credit::{CreditFrame, CreditGrant, CreditWindow};
use nysia_proto::frame::{Frame, FrameDecoder, FrameKind};
use nysia_proto::stream::{StreamId, UnattachedFrame};

use super::DaemonError;
use super::endpoint::Socket;
use crate::channel::Dispatcher;
use crate::channel::credit::CreditLedger;
use crate::state::StreamTable;

/// How long a read may block before the loop looks at its signals again.
///
/// Only a backstop. The loop already wakes on every frame and blocks on its signal channel
/// whenever credit is exhausted, so in normal operation this never fires. It exists because
/// a socket that has gone quiet without closing — a daemon wedged, a pipe whose peer has
/// stopped without an EOF — would otherwise keep this thread parked forever after the
/// window has asked it to stop.
///
/// Not every platform can honour it: see [`Socket::set_read_timeout`].
const READ_POLL: Duration = Duration::from_millis(250);

/// What the reader thread is told from outside.
enum Signal {
    /// The webview rendered `bytes` of `stream` — after xterm's `write()` callback, never
    /// before.
    Rendered { stream: StreamId, bytes: u32 },
    /// A pane closed.
    Detach(StreamId),
    /// Stop.
    Stop,
}

/// A running output plane.
pub struct Stream {
    signals: Sender<Signal>,
    /// Kept only so the handle is not dropped while the thread runs; never joined. See the
    /// module docs.
    _reader: JoinHandle<()>,
}

impl Stream {
    /// Start reading `socket`, delivering output to `dispatcher`.
    ///
    /// `table` is the client's, not a copy: the reader resolves every frame's stream id
    /// against the same table the control plane writes to.
    pub fn spawn(
        socket: BufReader<Box<dyn Socket>>,
        table: Arc<Mutex<StreamTable>>,
        dispatcher: Dispatcher,
        window: CreditWindow,
        on_closed: Box<dyn Fn() + Send>,
    ) -> Result<Self, DaemonError> {
        let (signals, inbox) = mpsc::channel::<Signal>();
        let reader = thread::Builder::new()
            .name("nysia-stream".to_owned())
            .spawn(move || {
                read(socket, &table, &dispatcher, window, &inbox);
                // However this thread ends — EOF, a framing error, a stop — the connection
                // is over. Saying so is what makes `daemon_watch` fire and the store
                // reconnect; without it output simply stopped and the window went on
                // believing it was connected until some unrelated command happened to fail.
                on_closed();
            })
            .map_err(|error| DaemonError::Io(error.to_string()))?;

        Ok(Self {
            signals,
            _reader: reader,
        })
    }

    /// Report that the webview has rendered `bytes` of `stream`.
    ///
    /// Returns `false` once the reader has stopped. Called from the `terminal_ack` command,
    /// which the webview invokes from inside xterm's `write()` callback — the whole point
    /// of the window is that credit tracks what has been *rendered*, and a message sitting
    /// in a queue has not been.
    pub fn rendered(&self, stream: StreamId, bytes: u32) -> bool {
        self.signals
            .send(Signal::Rendered { stream, bytes })
            .is_ok()
    }

    /// Report that a pane closed, returning its share of the shared budget.
    pub fn detach(&self, stream: StreamId) -> bool {
        self.signals.send(Signal::Detach(stream)).is_ok()
    }
}

impl Drop for Stream {
    /// Ask the reader to stop, and **do not wait for it**.
    ///
    /// Joining here is the deadlock this design exists to avoid. The reader owns the only
    /// handle to the socket, so nothing outside it can interrupt a `read` that is already in
    /// progress; a join would therefore block until the daemon said something, and callers
    /// drop a `Stream` while holding the client lock. One garbled control line on an
    /// otherwise idle daemon was enough to park the window forever, with no crash and
    /// nothing in the log.
    ///
    /// What the thread costs after this returns is one socket and one buffer, and it ends at
    /// the next read timeout or EOF, whichever comes first.
    fn drop(&mut self) {
        let _ = self.signals.send(Signal::Stop);
    }
}

/// Apply one signal. `false` means stop.
fn apply(
    signal: Signal,
    ledger: &mut CreditLedger,
    table: &Arc<Mutex<StreamTable>>,
    socket: &mut BufReader<Box<dyn Socket>>,
) -> bool {
    match signal {
        Signal::Stop => false,
        Signal::Detach(stream) => {
            ledger.detach(stream);
            true
        }
        Signal::Rendered { stream, bytes } => {
            // Same reason `route` resolves attachment from the table: an ack can arrive for
            // a stream this ledger has not opened yet, and treating it as a stranger would
            // throw its credit away for good. `UnknownStream` only means what it says once
            // the table agrees the id no longer routes anywhere.
            if !ledger.is_attached(stream)
                && table.lock().is_ok_and(|table| table.is_routed(stream))
            {
                ledger.attach(stream);
            }
            render_and_grant(ledger, socket, stream, bytes)
        }
    }
}

/// Return credit for `bytes` the webview rendered on `stream`. `false` means stop.
fn render_and_grant(
    ledger: &mut CreditLedger,
    socket: &mut BufReader<Box<dyn Socket>>,
    stream: StreamId,
    bytes: u32,
) -> bool {
    match ledger.render(stream, bytes) {
        // An ack for a stream that has already been detached is the tail of a close
        // race, not a fault: the pane went away while its last frames were still in
        // flight. There is no credit to return, and nothing to report.
        Err(crate::channel::credit::CreditError::UnknownStream(_)) | Ok(None) => true,
        Ok(Some(grant)) => send_grant(socket, stream, &grant).is_ok(),
        Err(error) => {
            // A webview claiming credit it was never given has made the ceiling
            // unenforceable. There is nothing to salvage.
            tracing::error!(%error, "the webview broke the credit window");
            false
        }
    }
}

/// Return any credit that has been earned but not yet granted.
///
/// Walked on every pass rather than only when a render report fills a batch: this thread is
/// the only one that can write to the socket, so a burst that ended just short of a batch
/// would otherwise sit on its credit until more output arrived — which it never would,
/// because the daemon was waiting for exactly that grant.
fn flush_grants(ledger: &mut CreditLedger, socket: &mut BufReader<Box<dyn Socket>>) -> bool {
    for stream in ledger.streams_owing_a_grant() {
        let Some(grant) = ledger.drain_grant(stream) else {
            continue;
        };
        if send_grant(socket, stream, &grant).is_err() {
            return false;
        }
    }
    true
}

/// The reader loop.
fn read(
    mut socket: BufReader<Box<dyn Socket>>,
    table: &Arc<Mutex<StreamTable>>,
    dispatcher: &Dispatcher,
    window: CreditWindow,
    signals: &mpsc::Receiver<Signal>,
) {
    let mut ledger = CreditLedger::new(window);
    let mut decoder = FrameDecoder::new();
    let mut buffer = vec![0u8; window.chunk as usize];

    // Best effort: where the platform supports it the read wakes periodically so a stop
    // signal is honoured even against a peer that has gone silent without closing.
    let _ = socket.get_ref().set_read_timeout(Some(READ_POLL));

    loop {
        while let Ok(signal) = signals.try_recv() {
            if !apply(signal, &mut ledger, table, &mut socket) {
                return;
            }
        }
        if !flush_grants(&mut ledger, &mut socket) {
            return;
        }

        if ledger.is_blocked() {
            // No attached stream can take another chunk. Stop reading and wait for the
            // webview to catch up — this is the stall propagating back to the child, and it
            // is the design working rather than a fault. Blocking on the signal channel
            // rather than on the socket is the whole point: the thing that will free us is a
            // render report, not a frame.
            match signals.recv() {
                Err(_) => return,
                Ok(signal) => {
                    if !apply(signal, &mut ledger, table, &mut socket) {
                        return;
                    }
                }
            }
            continue;
        }

        let read = match socket.get_mut().read(&mut buffer) {
            Ok(0) => {
                tracing::info!("the daemon closed the output stream");
                return;
            }
            Ok(read) => read,
            Err(error) if timed_out(&error) => continue,
            Err(error) => {
                tracing::warn!(%error, "the output stream failed");
                return;
            }
        };
        decoder.push(&buffer[..read]);

        loop {
            let frame = match decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    // No delimiter to resynchronise on. Tear the connection down.
                    tracing::error!(%error, "the output stream framing is corrupt");
                    return;
                }
            };

            match route(frame, table, &mut ledger, dispatcher) {
                Routed::Continue => {}
                Routed::DropConnection => return,
            }
        }
    }
}

/// What routing one frame decided.
enum Routed {
    /// Keep reading.
    Continue,
    /// The peer and this client disagree about what is on the wire.
    DropConnection,
}

/// Send one frame to its session, or decide what to do because it has none.
fn route(
    frame: Frame,
    table: &Arc<Mutex<StreamTable>>,
    ledger: &mut CreditLedger,
    dispatcher: &Dispatcher,
) -> Routed {
    if frame.kind == FrameKind::Credit {
        adopt_window(ledger, &frame.payload);
        return Routed::Continue;
    }

    if !ledger.is_attached(frame.stream) {
        // Attachment is resolved from the **shared** table rather than from a signal of its
        // own. A signal would race the frames it authorises — the daemon may write output
        // the instant it has answered `stream_attach`, and a reader that had not yet drained
        // its inbox would discard a session's opening bytes as unroutable. Reading the table
        // the control plane has already written cannot lose that race, and it is the same
        // table, so the two can no longer disagree about which id belongs to whom.
        //
        // The ordering this rests on is the protocol's own: the daemon picks the id and the
        // client learns it from the `stream_attach` response, so an id in a frame is either
        // one this window was told about, one it has since retired, or one that was never
        // assigned at all.
        let (known, next) = match table.lock() {
            Ok(table) => (table.is_routed(frame.stream), table.next_to_assign()),
            Err(_) => (false, StreamId::FIRST),
        };
        if known {
            ledger.attach(frame.stream);
            return deliver(frame, ledger, dispatcher);
        }

        // `classify_unattached` is proto's call, not this file's, and the difference it
        // draws is the difference between losing one frame and losing every session on the
        // connection. An id this connection already handed out is the tail of a detach race
        // and nothing is wrong; one it never handed out is a desync.
        return match frame.stream.classify_unattached(next) {
            UnattachedFrame::DiscardFrame => {
                tracing::debug!(stream = %frame.stream, "discarding a frame for a detached stream");
                Routed::Continue
            }
            UnattachedFrame::DropConnection => {
                tracing::error!(
                    stream = %frame.stream,
                    "the daemon sent a frame for a stream it never assigned"
                );
                Routed::DropConnection
            }
        };
    }

    deliver(frame, ledger, dispatcher)
}

/// Spend the frame's credit and hand it to the webview.
fn deliver(frame: Frame, ledger: &mut CreditLedger, dispatcher: &Dispatcher) -> Routed {
    let size = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX);
    if let Err(error) = ledger.receive(frame.stream, size) {
        tracing::error!(%error, "the daemon ignored the credit window");
        return Routed::DropConnection;
    }
    if dispatcher.send(frame) {
        Routed::Continue
    } else {
        // The webview is gone. Nothing downstream will ever render again.
        Routed::DropConnection
    }
}

/// Whether an error is a read deadline expiring rather than a real failure.
fn timed_out(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// Adopt a window the daemon advertised on a grant.
fn adopt_window(ledger: &mut CreditLedger, payload: &[u8]) {
    match serde_json::from_slice::<CreditFrame>(payload) {
        Ok(CreditFrame::Grant(grant)) => {
            if !ledger.adopt(grant.window) {
                tracing::warn!("the daemon advertised an incoherent credit window; ignoring it");
            }
        }
        // An ack travelling daemon-to-client would mean the daemon thinks it is the reader.
        Ok(CreditFrame::Ack(_)) => {
            tracing::warn!("the daemon sent a credit ack on the output stream; ignoring it");
        }
        Err(error) => tracing::warn!(%error, "a credit frame did not parse"),
    }
}

/// Send a grant back up the stream socket.
fn send_grant(
    socket: &mut BufReader<Box<dyn Socket>>,
    stream: StreamId,
    grant: &CreditGrant,
) -> Result<(), DaemonError> {
    use std::io::Write;

    let payload = serde_json::to_vec(&CreditFrame::Grant(*grant))
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    let wire = nysia_proto::frame::encode(&Frame::new(FrameKind::Credit, stream, payload))
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;

    // Through `get_mut`, because the reader half of this `BufReader` holds bytes the daemon
    // has already sent and unwrapping it to write would throw them away.
    let out = socket.get_mut();
    out.write_all(&wire)
        .and_then(|()| out.flush())
        .map_err(|error| DaemonError::Io(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nysia_proto::identity::SessionHandle;

    use super::*;
    use crate::channel::FrameSink;

    fn handle(n: u8) -> SessionHandle {
        format!("sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f{n:02}")
            .parse()
            .expect("a well-formed handle")
    }

    /// A socket that replays canned frames and records what was written back.
    ///
    /// Once its script runs out it **parks**, the way a real socket does against a daemon
    /// that is alive but has nothing to say. That is deliberate and load-bearing: a fake
    /// that returned a timeout instead would let the reader loop round and drain its signals
    /// anyway, and the wedge these tests exist to catch would never reproduce. Windows named
    /// pipes cannot be given a read deadline at all (see `Socket::set_read_timeout`), so
    /// parking is the honest model of the platform this is developed on.
    ///
    /// It unparks when the harness drops its end, and then reports EOF — which is what a
    /// socket does when its peer goes away.
    struct Wire {
        incoming: Cursor<Vec<u8>>,
        written: Arc<Mutex<Vec<u8>>>,
        /// Held open until the harness drops the sending half.
        park: Option<mpsc::Receiver<()>>,
    }

    impl Read for Wire {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let read = self.incoming.read(buf)?;
            if read > 0 {
                return Ok(read);
            }
            if let Some(park) = &self.park {
                // Blocks until the harness lets go. A real socket behaves exactly this way,
                // and it is the only behaviour under which a reader that asks the wrong
                // question about credit actually gets stuck.
                let _ = park.recv();
            }
            Ok(0)
        }
    }

    impl std::io::Write for Wire {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut written) = self.written.lock() {
                written.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Socket for Wire {
        fn set_read_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A sink that counts deliveries and never fails.
    #[derive(Clone, Default)]
    struct Counting(Arc<AtomicUsize>);

    impl FrameSink for Counting {
        fn deliver(&self, _bytes: Vec<u8>) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Harness {
        /// Dropping this unparks a reader waiting on an exhausted script.
        _open: Option<mpsc::Sender<()>>,
        stream: Stream,
        written: Arc<Mutex<Vec<u8>>>,
        closed: Arc<AtomicUsize>,
        delivered: Arc<AtomicUsize>,
        table: Arc<Mutex<StreamTable>>,
    }

    impl Harness {
        /// Block until the reader has stopped consuming, so a test acks what was delivered
        /// rather than racing the thread that delivers it.
        fn quiesce(&self) {
            let mut seen = usize::MAX;
            for _ in 0..200 {
                let now = self.delivered.load(Ordering::SeqCst);
                if now == seen && now > 0 {
                    return;
                }
                seen = now;
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Start a reader over `incoming`, parking rather than reporting EOF when `idle`.
    fn start(incoming: Vec<u8>, idle: bool, attached: &[StreamId]) -> Harness {
        let written = Arc::new(Mutex::new(Vec::new()));
        let table = Arc::new(Mutex::new(StreamTable::new()));
        {
            let mut held = table.lock().expect("fresh");
            for (index, stream) in attached.iter().enumerate() {
                held.remember(*stream, handle(index as u8 + 1));
            }
        }

        let sink = Counting::default();
        let delivered = Arc::clone(&sink.0);
        let (open, park) = mpsc::channel::<()>();
        let socket = BufReader::new(Box::new(Wire {
            incoming: Cursor::new(incoming),
            written: Arc::clone(&written),
            park: idle.then_some(park),
        }) as Box<dyn Socket>);

        let closed = Arc::new(AtomicUsize::new(0));
        let flag = Arc::clone(&closed);
        let stream = Stream::spawn(
            socket,
            Arc::clone(&table),
            Dispatcher::spawn(sink),
            CreditWindow::DEFAULT,
            Box::new(move || {
                flag.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("the reader thread starts");

        Harness {
            _open: idle.then_some(open),
            stream,
            written,
            closed,
            delivered,
            table,
        }
    }

    /// Wait for `predicate`, so a test never races the reader thread.
    fn eventually(mut predicate: impl FnMut() -> bool) -> bool {
        for _ in 0..400 {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn output(stream: StreamId, len: usize) -> Vec<u8> {
        nysia_proto::frame::encode(&Frame::new(FrameKind::Output, stream, vec![b'x'; len]))
            .expect("a frame under the ceiling")
    }

    /// HIGH 3, at the level the bug actually lived: the loop, not the ledger.
    ///
    /// One stream, spent to its per-stream ceiling while the shared total is still three
    /// quarters full, against a daemon that has correctly stopped sending. The earlier loop
    /// asked only whether the total was spent, went back into `read`, and never wrote the
    /// grant that would have freed it. Nothing about the ledger was wrong, which is why a
    /// test on the ledger alone could not see this.
    #[test]
    fn a_stream_at_its_own_ceiling_still_gets_its_grant_written() {
        let window = CreditWindow::DEFAULT;
        let stream_id = StreamId(1);

        // Fill the stream's own credit exactly, then go quiet without closing.
        let mut wire = Vec::new();
        let mut sent = 0;
        while sent + window.chunk <= window.per_stream_initial {
            wire.extend_from_slice(&output(stream_id, window.chunk as usize));
            sent += window.chunk;
        }
        let harness = start(wire, true, &[stream_id]);

        // Wait for the reader to consume the burst, so the ack below reports bytes that were
        // really delivered. A webview cannot ack what it has not been sent, and a test that
        // raced ahead of the reader would be exercising a sequence no client can produce.
        harness.quiesce();

        // The webview renders a full batch. The reader is the only thing that can write the
        // grant, and at this point it is out of per-stream credit — which is exactly the
        // state the old loop could not get out of.
        assert!(harness.stream.rendered(stream_id, window.ack_batch));

        assert!(
            eventually(|| !harness.written.lock().map(|w| w.is_empty()).unwrap_or(true)),
            "no grant was ever written: the reader wedged with credit owed"
        );
    }

    #[test]
    fn a_reader_that_loses_its_socket_says_the_connection_is_over() {
        // Otherwise output simply stops and the window goes on believing it is connected
        // until some unrelated control command happens to fail.
        let harness = start(Vec::new(), false, &[]);
        assert!(
            eventually(|| harness.closed.load(Ordering::SeqCst) > 0),
            "EOF must report the connection closed"
        );
    }

    #[test]
    fn dropping_a_stream_returns_immediately_even_while_a_read_is_parked() {
        // The deadlock this design exists to avoid: callers drop a `Stream` while holding
        // the client lock, and the reader can be parked in a `read` only the daemon can end.
        let harness = start(Vec::new(), true, &[]);
        let started = std::time::Instant::now();
        drop(harness.stream);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "dropping the stream blocked for {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_frame_for_a_stream_the_daemon_never_assigned_drops_the_connection() {
        // Proto draws this line, not this file: an id past the watermark is a desync, and
        // carrying on would mean trusting a peer that disagrees about what is on the wire.
        let harness = start(output(StreamId(9), 16), true, &[]);
        assert!(
            eventually(|| harness.closed.load(Ordering::SeqCst) > 0),
            "an unassignable stream id must close the connection"
        );
        drop(harness.table);
    }

    #[test]
    fn a_frame_for_a_detached_stream_is_discarded_without_closing_anything() {
        // The tail of a detach race. Taking the connection down here would take every other
        // session with it for an entirely ordinary close.
        let stream_id = StreamId(1);
        let harness = start(output(stream_id, 16), true, &[stream_id]);
        assert!(harness.stream.detach(stream_id));

        // Give the reader time to see both the detach and the frame.
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            harness.closed.load(Ordering::SeqCst),
            0,
            "an ordinary detach must not close the connection"
        );
    }

    #[test]
    fn an_incoherent_window_from_the_daemon_is_ignored() {
        let mut ledger = CreditLedger::new(CreditWindow::DEFAULT);
        let deadlocking = CreditWindow {
            ack_batch: CreditWindow::DEFAULT.pending_cap + 1,
            ..CreditWindow::DEFAULT
        };
        let payload = serde_json::to_vec(&CreditFrame::Grant(CreditGrant {
            bytes: 1024,
            window: deadlocking,
        }))
        .expect("serialisable");

        adopt_window(&mut ledger, &payload);
        assert_eq!(
            ledger.window(),
            CreditWindow::DEFAULT,
            "a window that would deadlock every stream must not be adopted"
        );
    }

    #[test]
    fn a_narrower_window_from_the_daemon_is_adopted() {
        // The daemon is entitled to shrink the window for a slow renderer without a
        // protocol change; that is why the grant carries it (D-13).
        let mut ledger = CreditLedger::new(CreditWindow::DEFAULT);
        let narrower = CreditWindow {
            per_stream_initial: 64 * 1024,
            ..CreditWindow::DEFAULT
        };
        let payload = serde_json::to_vec(&CreditFrame::Grant(CreditGrant {
            bytes: 1024,
            window: narrower,
        }))
        .expect("serialisable");

        adopt_window(&mut ledger, &payload);
        assert_eq!(ledger.window(), narrower);
    }

    #[test]
    fn a_credit_payload_that_does_not_parse_leaves_the_window_alone() {
        let mut ledger = CreditLedger::new(CreditWindow::DEFAULT);
        adopt_window(&mut ledger, b"{}");
        assert_eq!(ledger.window(), CreditWindow::DEFAULT);
    }
}
