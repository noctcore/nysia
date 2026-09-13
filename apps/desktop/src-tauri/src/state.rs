//! The one piece of state the window holds: a socket connection and a channel.
//!
//! Under D-2 that is the *complete* list of what may live here. Sessions, terminal state,
//! projects and worktrees all belong to the daemon, and anything of theirs that appeared in
//! this struct would be a cache the window could serve stale — which is exactly the failure
//! the headless split exists to make impossible. A connection is not state the daemon
//! should own, because the daemon does not know this window exists until it connects.
//!
//! ## The lock discipline
//!
//! One rule, and it is the whole reason this module reads the way it does: **nothing waits
//! on another thread while holding the client lock.** Tearing a connection down signals its
//! reader and returns; the reader ends on its own time. A version of this file that joined
//! under the lock could be parked forever by one garbled control line — the reader was in a
//! `read` only the daemon could end, and every later command blocked behind it with no
//! crash and nothing in the log.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use nysia_proto::credit::CreditWindow;
use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::handshake::{ClientRole, DaemonIdentity};
use nysia_proto::identity::SessionHandle;
use nysia_proto::stream::{StreamAttach, StreamDetach, StreamId};
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::channel::Dispatcher;
use crate::daemon::control::Control;
use crate::daemon::stream::Stream;
use crate::daemon::{DaemonError, endpoint};

/// Which stream id belongs to which session, in both directions.
///
/// Shared between the control plane, which learns the mapping from `stream_attach`, and the
/// reader thread, which resolves every frame against it. **One table, deliberately.** Two —
/// one in the client and one inside the reader — is what made closing the first session kill
/// output for every other one: the shared table forgot the id, the reader's local copy went
/// on stamping frames with it, and the next frame looked unassignable.
#[derive(Debug)]
pub struct StreamTable {
    by_handle: HashMap<SessionHandle, StreamId>,
    by_stream: HashMap<StreamId, SessionHandle>,
    /// The lowest id this connection has not yet seen assigned.
    ///
    /// The watermark [`StreamId::classify_unattached`] needs: below it and detached is the
    /// tail of a close race, at or above it is a desync. The daemon assigns the ids, so this
    /// only ever follows what it has handed out.
    next: StreamId,
}

impl Default for StreamTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTable {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            by_handle: HashMap::new(),
            by_stream: HashMap::new(),
            next: StreamId::FIRST,
        }
    }

    /// Record an id the daemon assigned.
    pub fn remember(&mut self, stream: StreamId, handle: SessionHandle) {
        self.by_handle.insert(handle.clone(), stream);
        self.by_stream.insert(stream, handle);
        if let Some(after) = stream.next()
            && after > self.next
        {
            self.next = after;
        }
    }

    /// The id this session's frames carry, if it is attached.
    pub fn stream_for(&self, handle: &SessionHandle) -> Option<StreamId> {
        self.by_handle.get(handle).copied()
    }

    /// Whether this id currently routes to a session.
    ///
    /// What the reader asks of a frame whose id it has no credit entry for: a `true` here is
    /// an attach the control plane has recorded and the reader has not yet seen, and the
    /// reader gives it credit on the spot rather than discarding a session's opening bytes.
    pub fn is_routed(&self, stream: StreamId) -> bool {
        self.by_stream.contains_key(&stream)
    }

    /// The first id this connection has not seen assigned.
    pub fn next_to_assign(&self) -> StreamId {
        self.next
    }

    /// Forget a session. The id is **not** freed: the daemon never reissues it, and neither
    /// does the watermark, which is what lets a late frame still carrying it be recognised
    /// as the tail of a detach rather than routed to whoever inherited the number.
    pub fn forget(&mut self, handle: &SessionHandle) -> Option<StreamId> {
        let stream = self.by_handle.remove(handle)?;
        self.by_stream.remove(&stream);
        Some(stream)
    }
}

/// Everything one connection owns.
struct Connected {
    control: Control,
    /// `None` until the webview has handed over its `Channel`.
    stream: Option<Stream>,
    identity: DaemonIdentity,
}

/// The managed state every Tauri command reaches through `try_state`.
///
/// `Clone` is cheap and deliberate: a command clones this before handing work to the
/// blocking pool, so the `State` borrow ends rather than being held across an `await`.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Mutex<Option<Connected>>>,
    /// Stream ids, outside `Connected` on purpose.
    ///
    /// A table that lived with the connection would be rebuilt empty on every reconnect
    /// while the webview's view of which pane is which did not, and the two would disagree
    /// about the same session.
    table: Arc<Mutex<StreamTable>>,
    /// Signalled whenever the connection is torn down, so [`Self::wait_for_disconnect`] can
    /// block instead of polling. A condvar rather than a channel because there may be more
    /// than one waiter and every one of them wants the same edge.
    dropped: Arc<(Mutex<u64>, Condvar)>,
}

impl Client {
    /// A client holding no connection.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            table: Arc::new(Mutex::new(StreamTable::new())),
            dropped: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    /// Connect, or return the identity of the daemon already attached.
    ///
    /// # Errors
    ///
    /// Whatever the handshake produced.
    pub fn connect(&self) -> Result<DaemonIdentity, DaemonError> {
        let mut held = self.lock()?;
        if let Some(connected) = held.as_ref() {
            return Ok(connected.identity.clone());
        }

        let control = Control::connect()?;
        let identity = control.identity().clone();
        *held = Some(Connected {
            control,
            stream: None,
            identity: identity.clone(),
        });
        Ok(identity)
    }

    /// Which daemon is attached, if any.
    pub fn identity(&self) -> Option<DaemonIdentity> {
        self.lock()
            .ok()
            .and_then(|held| held.as_ref().map(|c| c.identity.clone()))
    }

    /// Issue a control verb.
    ///
    /// A dropped connection is torn down here rather than left in place, so the next
    /// [`Self::connect`] starts a fresh one instead of reusing a socket that will never
    /// answer again.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] when nothing is attached, or whatever the daemon said.
    pub fn request(&self, payload: RequestPayload) -> Result<ResponsePayload, DaemonError> {
        let outcome = {
            let held = self.lock()?;
            let connected = held.as_ref().ok_or(DaemonError::Disconnected)?;
            connected.control.request(payload)
        };

        if let Err(error) = &outcome
            && matches!(
                error,
                DaemonError::Io(_) | DaemonError::Protocol(_) | DaemonError::Disconnected
            )
        {
            self.disconnect();
        }
        outcome
    }

    /// Take the webview's channel and open the output connection.
    ///
    /// **Attaching sessions is a separate step.** This opens the stream socket and starts
    /// the reader; it deliberately requires no session to exist yet, because the connect
    /// sequence has to work against a daemon holding none. An earlier version demanded a
    /// session here and was called before the only verb that could have produced one, so it
    /// failed on every attempt, in that order, forever — no daemon could ever be reached.
    ///
    /// Calling this twice replaces the connection, which is what a webview reload produces.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] if no daemon is attached, or whatever the handshake
    /// produced.
    pub fn attach_channel(&self, channel: Channel<InvokeResponseBody>) -> Result<(), DaemonError> {
        let socket = {
            // The lock is not held across the connect: opening a socket and shaking hands
            // can block for as long as the daemon takes, and every other command would
            // queue behind it.
            let held = self.lock()?;
            held.as_ref().ok_or(DaemonError::Disconnected)?;
            endpoint::open(&endpoint::endpoint()?)?
        };

        let mut reader = std::io::BufReader::new(socket);
        endpoint::handshake(
            &mut reader,
            ClientRole::Stream,
            &endpoint::client_id(ClientRole::Stream)?,
        )?;

        let notify = self.clone();
        let stream = Stream::spawn(
            reader,
            Arc::clone(&self.table),
            Dispatcher::spawn(channel),
            CreditWindow::DEFAULT,
            Box::new(move || notify.disconnect()),
        )?;

        // Replacing the old reader drops it, which signals it and returns — see `Stream`'s
        // `Drop`. Nothing here waits for that thread.
        let mut held = self.lock()?;
        let connected = held.as_mut().ok_or(DaemonError::Disconnected)?;
        connected.stream = Some(stream);
        Ok(())
    }

    /// Ask the daemon to route a session's output, and give the id its credit.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] if no stream is attached, or whatever the daemon said.
    pub fn attach_session(&self, handle: SessionHandle) -> Result<StreamId, DaemonError> {
        // An id this window already holds is the answer. Asking again would spend a second
        // one on the same session and leave the first routing frames nothing reads — the
        // daemon never reissues an id, so the waste is permanent for the connection.
        if let Some(existing) = self
            .table
            .lock()
            .ok()
            .and_then(|table| table.stream_for(&handle))
        {
            return Ok(existing);
        }

        let answer = self.request(RequestPayload::StreamAttach(StreamAttach {
            handle: handle.clone(),
        }))?;
        let ResponsePayload::StreamAttach(attached) = answer else {
            return Err(DaemonError::Protocol(format!(
                "stream_attach was answered with a {} payload",
                answer.verb()
            )));
        };

        if let Ok(mut table) = self.table.lock() {
            table.remember(attached.stream_id, attached.handle.clone());
        }

        // Nothing is signalled to the reader: it resolves attachment from the table above,
        // which cannot race the frames the daemon is already entitled to send. See
        // `daemon::stream::route`.
        Ok(attached.stream_id)
    }

    /// Stop routing a session's output and release its credit.
    ///
    /// The control-plane detach is best effort: the common caller is `session_close`, and a
    /// session that has already gone takes its stream with it.
    pub fn detach_session(&self, handle: &SessionHandle) {
        let Some(stream) = self.table.lock().ok().and_then(|mut t| t.forget(handle)) else {
            return;
        };

        {
            let Ok(held) = self.lock() else { return };
            if let Some(live) = held.as_ref().and_then(|c| c.stream.as_ref()) {
                live.detach(stream);
            }
        }

        let _ = self.request(RequestPayload::StreamDetach(StreamDetach {
            stream_id: stream,
        }));
    }

    /// Report bytes the webview has rendered, returning credit upstream.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] if no stream is attached.
    pub fn rendered(&self, stream: StreamId, bytes: u32) -> Result<(), DaemonError> {
        let held = self.lock()?;
        let live = held
            .as_ref()
            .and_then(|c| c.stream.as_ref())
            .ok_or(DaemonError::Disconnected)?;
        if live.rendered(stream, bytes) {
            Ok(())
        } else {
            Err(DaemonError::Disconnected)
        }
    }

    /// Block until the connection is torn down.
    ///
    /// How the webview learns the daemon went away while no command was in flight, without
    /// `emit` (tauri#12724). Returns immediately when nothing is attached, so a caller that
    /// races a disconnect is not left waiting for an edge that already passed.
    pub fn wait_for_disconnect(&self) {
        let (count, signal) = &*self.dropped;
        let Ok(observed) = count.lock() else { return };
        if self.identity().is_none() {
            return;
        }
        let seen = *observed;
        let _unused = signal.wait_while(observed, |current| *current == seen);
    }

    /// Tear the connection down and wake every waiter.
    ///
    /// The connection is taken out under the lock and dropped **after** it is released.
    /// Dropping a `Connected` drops its `Stream`, and although that only signals rather than
    /// joins, doing it outside the lock keeps the rule in this module's docs exact: nothing
    /// touches another thread's lifetime while holding the client lock.
    pub fn disconnect(&self) {
        let taken = self.lock().ok().and_then(|mut held| held.take());
        drop(taken);

        let (count, signal) = &*self.dropped;
        if let Ok(mut current) = count.lock() {
            *current = current.wrapping_add(1);
        }
        signal.notify_all();
    }

    /// The lock, with a poisoned mutex reported rather than panicked on.
    ///
    /// A panic here would unwind across wry's `extern "C"` boundary and become `abort()`
    /// (traps register #2), so the one place that could panic is the one place that must
    /// not.
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<Connected>>, DaemonError> {
        self.inner
            .lock()
            .map_err(|_| DaemonError::Io("the daemon client's lock was poisoned".to_owned()))
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
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
    fn a_fresh_client_holds_nothing_and_says_so() {
        let client = Client::new();
        assert!(client.identity().is_none());
        assert!(matches!(
            client.request(RequestPayload::SessionList(
                nysia_proto::session::SessionList {}
            )),
            Err(DaemonError::Disconnected)
        ));
        assert!(matches!(
            client.rendered(StreamId(1), 128),
            Err(DaemonError::Disconnected)
        ));
    }

    #[test]
    fn waiting_for_a_disconnect_returns_at_once_when_nothing_is_attached() {
        // Otherwise the store's watch would hang forever on a window that never connected,
        // and the reconnect loop would never run.
        let client = Client::new();
        client.wait_for_disconnect();
    }

    #[test]
    fn detaching_a_session_on_a_disconnected_client_is_not_an_error() {
        // `session_close` calls this, and a window closing during a disconnect is ordinary.
        let client = Client::new();
        client.detach_session(&handle(1));
    }

    #[test]
    fn disconnecting_is_idempotent_and_returns_promptly() {
        let client = Client::new();
        let started = std::time::Instant::now();
        client.disconnect();
        client.disconnect();
        assert!(client.identity().is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn the_watermark_follows_the_ids_the_daemon_assigns() {
        // What `classify_unattached` separates a retired id from one that never existed.
        let mut table = StreamTable::new();
        assert_eq!(table.next_to_assign(), StreamId::FIRST);

        table.remember(StreamId(1), handle(1));
        assert_eq!(table.next_to_assign(), StreamId(2));

        // Ids need not arrive in order — another client's attaches move the daemon's
        // counter — so the watermark takes the highest it has seen, not the latest.
        table.remember(StreamId(7), handle(2));
        assert_eq!(table.next_to_assign(), StreamId(8));
        table.remember(StreamId(4), handle(3));
        assert_eq!(table.next_to_assign(), StreamId(8));
    }

    #[test]
    fn forgetting_a_session_clears_both_directions_but_not_the_watermark() {
        // The id stays spent. A late frame still carrying it is then recognisable as the
        // tail of a detach rather than routed to whoever inherited the number.
        let mut table = StreamTable::new();
        table.remember(StreamId(1), handle(1));
        table.remember(StreamId(2), handle(2));

        assert_eq!(table.forget(&handle(1)), Some(StreamId(1)));
        assert_eq!(table.stream_for(&handle(1)), None);
        assert_eq!(table.next_to_assign(), StreamId(3));
        assert_eq!(table.forget(&handle(1)), None);

        assert_eq!(
            table.stream_for(&handle(2)),
            Some(StreamId(2)),
            "closing one session must not disturb another"
        );
    }
}
