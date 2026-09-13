//! The output plane: length-prefixed binary frames under a credit window.
//!
//! One thread reads the stream socket, checks each frame against the [`CreditLedger`], and
//! hands it to the [`Dispatcher`] for the webview. A second path — the `terminal_ack`
//! command — feeds render acknowledgements back in, which is what returns credit upstream.
//!
//! The reason the reader is a thread rather than a task is the backpressure design itself.
//! At zero credit it simply **stops reading the socket**. The socket buffer fills, the
//! daemon's write blocks, the daemon stops draining the PTY, the kernel buffer fills, and
//! the child blocks in `write(2)`. Nothing is dropped and nothing is buffered without
//! bound, and it works because a blocked thread is a real thing the OS understands.
//!
//! ## The one thing that is not settled yet
//!
//! [`attach_frame`] is where a proto frame becomes a channel frame, and it is the only
//! place that has to change when W1 lands the stream-tagged header and the control-plane
//! attach verb. Everything above it — the ledger, the dispatcher, the webview's decoder —
//! already works in terms of stream ids and does not move.

use std::collections::HashMap;
use std::io::Read;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use nysia_proto::credit::{CreditFrame, CreditGrant, CreditWindow};
use nysia_proto::frame::{Frame, FrameDecoder as ProtoDecoder, FrameKind};
use nysia_proto::identity::SessionHandle;

use super::DaemonError;
use super::endpoint::Socket;
use crate::channel::Dispatcher;
use crate::channel::credit::CreditLedger;
use crate::channel::framing::{ChannelFrame, StreamId};

/// Which stream id belongs to which session, in both directions.
///
/// The forward map is what [`attach_frame`] needs; the reverse is what an ack from the
/// webview needs, because the webview knows only stream ids and a [`CreditGrant`] has to
/// name a [`SessionHandle`].
#[derive(Debug, Default)]
pub struct StreamTable {
    by_handle: HashMap<SessionHandle, StreamId>,
    by_stream: HashMap<StreamId, SessionHandle>,
    next: StreamId,
}

impl StreamTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for `handle`, assigning one if this is the first frame for it.
    ///
    /// Ids are dense and monotonic rather than derived from the handle, so the webview can
    /// index an array with one instead of hashing a 41-character string on every frame.
    pub fn id_for(&mut self, handle: &SessionHandle) -> StreamId {
        if let Some(&existing) = self.by_handle.get(handle) {
            return existing;
        }
        let assigned = self.next;
        self.next = self.next.wrapping_add(1);
        self.by_handle.insert(handle.clone(), assigned);
        self.by_stream.insert(assigned, handle.clone());
        assigned
    }

    /// Which session a stream id belongs to.
    pub fn handle_for(&self, stream: StreamId) -> Option<&SessionHandle> {
        self.by_stream.get(&stream)
    }

    /// Forget a session, so a closed pane's id does not keep its entry alive forever.
    pub fn forget(&mut self, handle: &SessionHandle) -> Option<StreamId> {
        let stream = self.by_handle.remove(handle)?;
        self.by_stream.remove(&stream);
        Some(stream)
    }
}

/// Decide which session a frame off the socket belongs to.
///
/// **This is the shim.** `nysia-proto`'s header is `[kind][len][payload]` today, with no
/// session discriminator, because it was written when a stream connection was assumed to
/// carry one session. The coordinator has settled that it carries several, with a `u32`
/// stream id in the header assigned by a control-plane attach verb, and W1 is landing that
/// change.
///
/// Until it does, this function is where the gap lives, and it is deliberately the *only*
/// place: a single connection with a single attached session is the one case the current
/// header can express unambiguously, so that is what it expresses. When the real header
/// arrives, this function is deleted and the reader takes the id straight off the frame.
/// Nothing else in this file, in [`crate::channel`], or in the webview changes, because
/// they are all already written in terms of stream ids.
fn attach_frame(frame: Frame, attached: &SessionHandle, table: &mut StreamTable) -> ChannelFrame {
    ChannelFrame::new(frame.kind, table.id_for(attached), frame.payload)
}

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
    reader: Option<JoinHandle<()>>,
}

impl Stream {
    /// Start reading `socket`, delivering `attached`'s output to `dispatcher`.
    ///
    /// `attached` is the shim's parameter — see [`attach_frame`]. It disappears with the
    /// shim.
    pub fn spawn(
        socket: Box<dyn Socket>,
        attached: SessionHandle,
        dispatcher: Dispatcher,
        window: CreditWindow,
    ) -> Self {
        let (signals, inbox) = mpsc::channel::<Signal>();
        let grants = signals.clone();
        let _ = grants;

        let reader = thread::Builder::new()
            .name("nysia-stream".to_owned())
            .spawn(move || read(socket, attached, dispatcher, window, &inbox))
            .ok();

        Self { signals, reader }
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

    /// Stop the reader.
    pub fn shutdown(&mut self) {
        let _ = self.signals.send(Signal::Stop);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The reader loop.
fn read(
    mut socket: Box<dyn Socket>,
    attached: SessionHandle,
    dispatcher: Dispatcher,
    window: CreditWindow,
    signals: &mpsc::Receiver<Signal>,
) {
    let mut ledger = CreditLedger::new(window);
    let mut table = StreamTable::new();
    let mut decoder = ProtoDecoder::new();
    let mut buffer = vec![0u8; window.chunk as usize];

    ledger.attach(table.id_for(&attached), attached.clone());

    loop {
        // Drain every signal first. Acks are what return credit, so processing them before
        // deciding whether to read is what lets a blocked reader unblock itself.
        while let Ok(signal) = signals.try_recv() {
            match signal {
                Signal::Stop => return,
                Signal::Detach(stream) => ledger.detach(stream),
                Signal::Rendered { stream, bytes } => {
                    match ledger.render(stream, bytes) {
                        Ok(Some(grant)) => {
                            if send_grant(&mut socket, &grant).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            // A webview claiming credit it was never given has made the
                            // ceiling unenforceable. There is nothing to salvage.
                            tracing::error!(%error, "the webview broke the credit window");
                            return;
                        }
                    }
                }
            }
        }

        if ledger.is_blocked() {
            // Zero credit: stop reading and wait for the webview to catch up. This is the
            // stall propagating back to the child, and it is the design working rather than
            // a fault. `recv` blocks, so a blocked reader costs nothing.
            match signals.recv() {
                Ok(Signal::Stop) | Err(_) => return,
                Ok(Signal::Detach(stream)) => ledger.detach(stream),
                Ok(Signal::Rendered { stream, bytes }) => match ledger.render(stream, bytes) {
                    Ok(Some(grant)) => {
                        if send_grant(&mut socket, &grant).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(%error, "the webview broke the credit window");
                        return;
                    }
                },
            }
            continue;
        }

        let read = match socket.read(&mut buffer) {
            Ok(0) => {
                tracing::info!("the daemon closed the output stream");
                return;
            }
            Ok(read) => read,
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

            if frame.kind == FrameKind::Credit {
                adopt_window(&mut ledger, &frame.payload);
                continue;
            }

            let frame = attach_frame(frame, &attached, &mut table);
            let size = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX);
            if let Err(error) = ledger.receive(frame.stream, size) {
                tracing::error!(%error, "the daemon ignored the credit window");
                return;
            }
            if !dispatcher.send(frame) {
                // The webview is gone. Nothing downstream will ever render again.
                return;
            }
        }
    }
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
fn send_grant(socket: &mut Box<dyn Socket>, grant: &CreditGrant) -> Result<(), DaemonError> {
    use std::io::Write;

    let payload = serde_json::to_vec(&CreditFrame::Grant(grant.clone()))
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    let wire = nysia_proto::frame::encode(&Frame::new(FrameKind::Credit, payload))
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    socket
        .write_all(&wire)
        .and_then(|()| socket.flush())
        .map_err(|error| DaemonError::Io(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(n: u8) -> SessionHandle {
        format!("sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f{n:02}")
            .parse()
            .expect("a well-formed handle")
    }

    #[test]
    fn a_session_keeps_one_stream_id_for_its_whole_life() {
        // The webview indexes surfaces by stream id, so an id that changed between frames
        // would split one session's output across two panes.
        let mut table = StreamTable::new();
        let first = table.id_for(&handle(1));
        assert_eq!(table.id_for(&handle(1)), first);
        assert_eq!(table.id_for(&handle(1)), first);
    }

    #[test]
    fn distinct_sessions_get_distinct_ids_and_map_back() {
        let mut table = StreamTable::new();
        let one = table.id_for(&handle(1));
        let two = table.id_for(&handle(2));
        assert_ne!(one, two);
        assert_eq!(table.handle_for(one), Some(&handle(1)));
        assert_eq!(table.handle_for(two), Some(&handle(2)));
        assert_eq!(table.handle_for(999), None);
    }

    #[test]
    fn forgetting_a_session_clears_both_directions() {
        // Both, because a stale reverse entry would let an ack for a closed pane return
        // credit to a stream that no longer exists.
        let mut table = StreamTable::new();
        let stream = table.id_for(&handle(1));
        assert_eq!(table.forget(&handle(1)), Some(stream));
        assert_eq!(table.handle_for(stream), None);
        assert_eq!(table.forget(&handle(1)), None);
    }

    #[test]
    fn the_shim_carries_kind_and_payload_through_untouched() {
        // The shim may only add the stream id. If it ever altered a payload, the webview's
        // parser would receive bytes no terminal produced.
        let mut table = StreamTable::new();
        let frame = Frame::new(FrameKind::Output, b"\x1b[31mred\x1b[0m".as_slice());
        let attached = attach_frame(frame.clone(), &handle(1), &mut table);

        assert_eq!(attached.kind, frame.kind);
        assert_eq!(attached.payload, frame.payload);
        assert_eq!(attached.stream, table.id_for(&handle(1)));
    }

    #[test]
    fn an_incoherent_window_from_the_daemon_is_ignored() {
        let mut ledger = CreditLedger::new(CreditWindow::DEFAULT);
        let deadlocking = CreditWindow {
            ack_batch: CreditWindow::DEFAULT.pending_cap + 1,
            ..CreditWindow::DEFAULT
        };
        let payload = serde_json::to_vec(&CreditFrame::Grant(CreditGrant {
            handle: handle(1),
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
            handle: handle(1),
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
