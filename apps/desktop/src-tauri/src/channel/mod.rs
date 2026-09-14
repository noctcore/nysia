//! The window's one multiplexed binary channel (§7.3, traps register #3).
//!
//! Every session's output reaches the webview through a single [`tauri::ipc::Channel`],
//! never through `emit`. tauri#12724 — a memory leak on sustained emits — is still open,
//! and terminal output is the definition of a sustained emit: a `cargo build` is tens of
//! thousands of them. One channel also means one place where coalescing and backpressure
//! happen, rather than one per session with thirty windows to keep in step.
//!
//! Frames from all sessions share the channel and are told apart by the stream id in each
//! header, which `nysia-proto` defines and whose size both ends read from the same generated
//! table (D-13). They are packed until the window closes ([`coalesce`]), and the daemon is
//! only allowed to produce them as fast as the webview renders them ([`credit`]).
//!
//! **The size, and not the offsets.** Proto exports `FRAME_HEADER_BYTES` and nothing finer,
//! so the two field positions the webview's decoder needs — the stream id at 1, the payload
//! length at `FRAME_HEADER_BYTES - 4` — are written locally in `apps/web/src/transport/
//! frames.ts` and derived from that total. They are the only part of the layout that is not
//! generated, and saying otherwise would be the overstatement worth avoiding: the header
//! grew from five bytes to nine when stream multiplexing landed, and a restated *total* is
//! exactly what survives that change quietly. A restated offset does not — both derivations
//! hang off the generated total, so the one number that moved still has one authority.
//!
//! ## The seam
//!
//! [`FrameSink`] exists so that everything above it is testable without a webview. The real
//! sink is a Tauri channel; the tests use a recording one. Nothing in the dispatcher knows
//! which it has, which is why the flush policy can be driven through sixteen milliseconds
//! of behaviour without sleeping.

pub mod coalesce;
pub mod credit;

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use coalesce::Coalescer;
use nysia_proto::frame::Frame;

/// Where a closed coalescing window goes.
///
/// Implemented for [`tauri::ipc::Channel`] in production and for a recording buffer in the
/// tests. The bytes are already framed and already packed — a sink's only job is delivery.
pub trait FrameSink: Send + 'static {
    /// Deliver one packed window.
    ///
    /// # Errors
    ///
    /// Returns a message describing why, which the dispatcher logs before shutting down.
    /// A sink that has failed once — the webview is gone, the channel was dropped — does
    /// not recover, so there is nothing more granular worth modelling.
    fn deliver(&self, bytes: Vec<u8>) -> Result<(), String>;
}

impl FrameSink for tauri::ipc::Channel<tauri::ipc::InvokeResponseBody> {
    fn deliver(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.send(tauri::ipc::InvokeResponseBody::Raw(bytes))
            .map_err(|error| error.to_string())
    }
}

/// What the driver thread is asked to do.
enum Command {
    /// Pack a frame into the current window.
    Frame(Box<Frame>),
    /// Deliver whatever is held and stop.
    Shutdown,
}

/// A handle on the driver thread that owns the coalescing window.
///
/// One thread rather than a mutex around the buffer, for a reason worth stating: the window
/// has a *deadline*, and a deadline needs something that can block until it expires.
/// `recv_timeout` gives that for free — the thread sleeps when the session is quiet, wakes
/// on the next frame, and wakes on its own when the 16 ms is up. A mutex-and-poll design
/// would either spin or miss the deadline.
pub struct Dispatcher {
    commands: Sender<Command>,
    driver: Option<JoinHandle<()>>,
}

impl Dispatcher {
    /// Start a driver delivering to `sink`.
    pub fn spawn(sink: impl FrameSink) -> Self {
        let (commands, inbox) = mpsc::channel::<Command>();
        let driver = thread::Builder::new()
            .name("nysia-channel".to_owned())
            .spawn(move || drive(&inbox, &sink))
            .ok();

        Self { commands, driver }
    }

    /// Queue a frame for the webview.
    ///
    /// Returns `false` once the driver has stopped — the webview is gone, or the dispatcher
    /// was shut down. Callers treat that as "stop reading this session", not as an error to
    /// report: it is the normal end of a window's life.
    pub fn send(&self, frame: Frame) -> bool {
        self.commands.send(Command::Frame(Box::new(frame))).is_ok()
    }

    /// Deliver what is held and stop the driver.
    ///
    /// Idempotent, and called from [`Drop`] as well, so that a dispatcher going out of scope
    /// on a panicking thread still flushes rather than dropping the last window on the
    /// floor.
    pub fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The driver loop: fill the window, close it on either trigger, deliver.
fn drive(inbox: &mpsc::Receiver<Command>, sink: &impl FrameSink) {
    let mut coalescer = Coalescer::new();

    loop {
        // An idle window has no deadline, so the thread blocks outright rather than waking
        // every 16 ms to find nothing. A filling one blocks only until its window expires.
        let command = match coalescer.deadline(Instant::now()) {
            Some(remaining) => inbox.recv_timeout(remaining).map_err(|error| match error {
                RecvTimeoutError::Timeout => None,
                RecvTimeoutError::Disconnected => Some(()),
            }),
            None => inbox.recv().map_err(|_| Some(())),
        };

        match command {
            Ok(Command::Frame(frame)) => match coalescer.push(&frame, Instant::now()) {
                Ok(Some(window)) => {
                    if deliver(sink, window).is_err() {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    // A frame past the ceiling is the writer's bug, not the channel's. The
                    // window is untouched, so dropping this one frame keeps every other
                    // session running rather than tearing the channel down for all of them.
                    tracing::error!(%error, "a terminal frame was refused by the framing layer");
                }
            },
            Ok(Command::Shutdown) => {
                if let Some(window) = coalescer.flush() {
                    let _ = deliver(sink, window);
                }
                return;
            }
            // Timed out: the 16 ms window is up.
            Err(None) => {
                if let Some(window) = coalescer.poll(Instant::now())
                    && deliver(sink, window).is_err()
                {
                    return;
                }
            }
            // Every sender is gone. Flush and stop.
            Err(Some(())) => {
                if let Some(window) = coalescer.flush() {
                    let _ = deliver(sink, window);
                }
                return;
            }
        }
    }
}

/// Hand one window to the sink, logging a failure once.
fn deliver(sink: &impl FrameSink, window: Vec<u8>) -> Result<(), ()> {
    sink.deliver(window).map_err(|error| {
        tracing::warn!(%error, "the terminal channel closed; the window has gone away");
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use nysia_proto::frame::{FrameDecoder, FrameKind};
    use nysia_proto::stream::StreamId;

    /// A sink that remembers every window it was handed.
    #[derive(Clone, Default)]
    struct Recorder {
        windows: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl Recorder {
        /// Every frame delivered so far, in order, across every window.
        fn frames(&self) -> Vec<Frame> {
            let windows = self
                .windows
                .lock()
                .expect("no test panics while holding this");
            let mut decoder = FrameDecoder::new();
            for window in windows.iter() {
                decoder.push(window);
            }
            let mut frames = Vec::new();
            while let Some(frame) = decoder.next_frame().expect("valid frames") {
                frames.push(frame);
            }
            frames
        }

        fn window_count(&self) -> usize {
            self.windows
                .lock()
                .expect("no test panics while holding this")
                .len()
        }
    }

    impl FrameSink for Recorder {
        fn deliver(&self, bytes: Vec<u8>) -> Result<(), String> {
            self.windows
                .lock()
                .map_err(|_| "recorder poisoned".to_owned())?
                .push(bytes);
            Ok(())
        }
    }

    /// Wait for `predicate`, so the tests never race the driver thread.
    fn eventually(mut predicate: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn frames_from_every_session_ride_the_one_channel_and_stay_distinguishable() {
        let recorder = Recorder::default();
        let dispatcher = Dispatcher::spawn(recorder.clone());

        for stream in [3, 4, 3, 5] {
            assert!(dispatcher.send(Frame::new(
                FrameKind::Output,
                StreamId(stream),
                b"tick".as_slice()
            )));
        }

        assert!(
            eventually(|| recorder.frames().len() == 4),
            "the window should have closed"
        );
        let streams: Vec<StreamId> = recorder.frames().iter().map(|f| f.stream).collect();
        assert_eq!(streams, [3, 4, 3, 5].map(StreamId));
    }

    #[test]
    fn a_quiet_session_still_gets_its_last_frame_delivered() {
        // The 16 ms trigger, end to end through the driver: one small frame and then
        // silence must not sit in the buffer forever.
        let recorder = Recorder::default();
        let dispatcher = Dispatcher::spawn(recorder.clone());
        dispatcher.send(Frame::new(FrameKind::Bell, StreamId(1), Vec::new()));

        assert!(eventually(|| recorder.window_count() == 1));
        assert_eq!(
            recorder.frames(),
            vec![Frame::new(FrameKind::Bell, StreamId(1), Vec::new())]
        );
    }

    #[test]
    fn shutdown_delivers_what_the_window_still_holds() {
        let recorder = Recorder::default();
        let mut dispatcher = Dispatcher::spawn(recorder.clone());
        dispatcher.send(Frame::new(
            FrameKind::Output,
            StreamId(1),
            b"tail".as_slice(),
        ));
        dispatcher.shutdown();

        assert_eq!(
            recorder.frames(),
            vec![Frame::new(
                FrameKind::Output,
                StreamId(1),
                b"tail".as_slice()
            )],
            "a dispatcher that stops must flush, not discard"
        );
        // Shutting down twice is not an error, because Drop calls it again.
        dispatcher.shutdown();
        assert!(!dispatcher.send(Frame::new(FrameKind::Bell, StreamId(1), Vec::new())));
    }

    #[test]
    fn dropping_the_dispatcher_flushes_too() {
        let recorder = Recorder::default();
        {
            let dispatcher = Dispatcher::spawn(recorder.clone());
            dispatcher.send(Frame::new(
                FrameKind::Output,
                StreamId(2),
                b"bye".as_slice(),
            ));
        }
        assert_eq!(recorder.frames().len(), 1);
    }
}
