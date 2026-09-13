//! The one piece of state the window holds: a socket connection and a channel.
//!
//! Under D-2 that is the *complete* list of what may live here. Sessions, terminal state,
//! projects and worktrees all belong to the daemon, and anything of theirs that appeared in
//! this struct would be a cache the window could serve stale — which is exactly the failure
//! the headless split exists to make impossible. A connection is not state the daemon
//! should own, because the daemon does not know this window exists until it connects.

use std::sync::{Arc, Condvar, Mutex};

use nysia_proto::credit::CreditWindow;
use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::handshake::DaemonIdentity;
use nysia_proto::identity::SessionHandle;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::channel::Dispatcher;
use crate::channel::framing::StreamId;
use crate::daemon::control::Control;
use crate::daemon::stream::{Stream, StreamTable};
use crate::daemon::{DaemonError, endpoint};

/// Everything the window holds, behind one lock.
struct Connected {
    control: Control,
    /// `None` until the webview has handed over its `Channel` — the window can be
    /// connected and listing sessions before anything is rendered.
    stream: Option<Stream>,
    table: StreamTable,
    identity: DaemonIdentity,
}

/// The managed state every Tauri command reaches through `try_state`.
///
/// `Clone` is cheap and deliberate: a command clones this before handing work to the
/// blocking pool, so the `State` borrow ends at the `await` rather than being held across
/// it.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Mutex<Option<Connected>>>,
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
            table: StreamTable::new(),
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

    /// Take the webview's channel and start delivering output over it.
    ///
    /// Replaces any channel already attached, because that is what a webview reload
    /// produces and the old one must not be left receiving frames nobody reads.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] if no daemon is attached.
    pub fn attach_channel(&self, channel: Channel<InvokeResponseBody>) -> Result<(), DaemonError> {
        let mut held = self.lock()?;
        let connected = held.as_mut().ok_or(DaemonError::Disconnected)?;

        // Dropping the old stream stops its reader and flushes its dispatcher; a reload
        // that leaked one would leave a thread reading into a channel with no listener.
        connected.stream = None;

        let socket = endpoint::open(&endpoint::endpoint()?)?;
        let mut reader = std::io::BufReader::new(socket);
        endpoint::handshake(
            &mut reader,
            nysia_proto::handshake::ClientRole::Stream,
            &endpoint::client_id(nysia_proto::handshake::ClientRole::Stream)?,
        )?;

        // TODO(W1): the stream connection attaches to one session until the control-plane
        // attach verb and the stream-tagged header land. See `daemon::stream::attach_frame`
        // — that function and this call site are the only two places that change.
        let attached = connected
            .table
            .handle_for(0)
            .cloned()
            .unwrap_or_else(SessionHandle::generate);

        connected.stream = Some(Stream::spawn(
            Box::new(reader.into_inner()),
            attached,
            Dispatcher::spawn(channel),
            CreditWindow::DEFAULT,
        ));
        Ok(())
    }

    /// Report bytes the webview has rendered, returning credit upstream.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] if no stream is attached.
    pub fn rendered(&self, stream: StreamId, bytes: u32) -> Result<(), DaemonError> {
        let held = self.lock()?;
        let connected = held.as_ref().ok_or(DaemonError::Disconnected)?;
        let live = connected
            .stream
            .as_ref()
            .ok_or(DaemonError::Disconnected)?
            .rendered(stream, bytes);
        if live {
            Ok(())
        } else {
            Err(DaemonError::Disconnected)
        }
    }

    /// Forget a closed session, returning its share of the shared credit budget.
    pub fn forget(&self, handle: &SessionHandle) {
        let Ok(mut held) = self.lock() else { return };
        let Some(connected) = held.as_mut() else {
            return;
        };
        if let Some(stream) = connected.table.forget(handle)
            && let Some(live) = connected.stream.as_ref()
        {
            live.detach(stream);
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
    pub fn disconnect(&self) {
        if let Ok(mut held) = self.lock() {
            *held = None;
        }
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
            client.rendered(0, 128),
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
    fn forgetting_a_session_on_a_disconnected_client_is_not_an_error() {
        // `session_close` calls this before the round trip, and a window closing during a
        // disconnect is entirely ordinary.
        let client = Client::new();
        client.forget(&SessionHandle::generate());
    }

    #[test]
    fn disconnecting_is_idempotent() {
        let client = Client::new();
        client.disconnect();
        client.disconnect();
        assert!(client.identity().is_none());
    }
}
