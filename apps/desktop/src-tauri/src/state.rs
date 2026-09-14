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

use crate::channel::{Dispatcher, FrameSink};
use crate::daemon::control::Control;
use crate::daemon::stream::Stream;
use crate::daemon::{DaemonError, endpoint};
use nysia_core::rpc::Endpoint;
use nysia_proto::credit::CreditWindow;
use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::handshake::{ClientRole, DaemonIdentity};
use nysia_proto::identity::SessionHandle;
use nysia_proto::stream::{StreamAttach, StreamDetach, StreamId};

/// How long [`Client::attach_session`] keeps retrying a refusal the daemon called retryable.
///
/// Long enough to cover a daemon binding its stream hub just after the handshake, short
/// enough that a session which genuinely cannot be attached still says so while the user is
/// looking at the pane.
const ATTACH_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// The pause between those attempts.
const ATTACH_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

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

/// Which **stream** connection a message is about.
///
/// Handed to a reader thread so that when it finally ends — possibly long after it was
/// superseded, because a parked read cannot be interrupted on Windows — it can say *which*
/// connection died rather than tearing down whatever happens to be live.
///
/// Per stream connection rather than per control connection, and the distinction is the
/// whole value of the type: a webview reload opens a second *stream* socket over the control
/// socket it already holds, so a number minted alongside the control connection is the same
/// on both sides of the supersede and separates nothing.
type Generation = u64;

/// Everything one connection owns.
struct Connected {
    control: Control,
    /// `None` until the webview has handed over its `Channel`.
    stream: Option<Stream>,
    identity: DaemonIdentity,
    /// Which stream connection the live reader belongs to.
    ///
    /// `None` between [`Client::connect`] and the first [`Client::attach_channel`]: the
    /// control socket is held but nothing is reading output over it yet, so no reader may
    /// claim this connection as its own.
    stream_generation: Option<Generation>,
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
    /// The generation of the next stream connection, so a superseded reader can be
    /// recognised. Outside `Connected` because it has to outlive every connection it numbers.
    next_generation: Arc<Mutex<Generation>>,
    /// Counts connection-lifecycle edges — a tear-down, and a stream connection replaced by
    /// a reload — so [`Self::wait_for_disconnect`] can block instead of polling.
    ///
    /// A condvar rather than a channel because there may be more than one waiter and every
    /// one of them wants the same edge. A count rather than a flag because a waiter has to
    /// be able to tell "no edge yet" from "an edge I already saw", which is the same
    /// distinction that keeps a caller racing a disconnect from waiting for one that has
    /// already passed.
    edges: Arc<(Mutex<u64>, Condvar)>,
    /// Where to dial, when it is not to be resolved from the environment.
    ///
    /// `None` for the window, which is the whole point of [`Self::new`]: production has one
    /// daemon and finds it the way every other client does. [`Self::at`] pins one instead,
    /// and is named so that a call site which has left the ordinary path is obvious in a
    /// diff and greppable afterwards.
    endpoint: Option<Endpoint>,
}

impl Client {
    /// A client holding no connection, which finds the daemon the way the window does.
    pub fn new() -> Self {
        Self::pinned(None)
    }

    /// A client that dials `endpoint` instead of resolving one from the environment.
    ///
    /// For the interop tests, which bind a daemon of their own and must not find — or
    /// disturb — the one the developer has running. Deliberately a second named
    /// constructor rather than an argument on [`Self::new`]: the window must not be one
    /// parameter away from talking to a daemon nobody chose.
    #[cfg(test)]
    pub fn at(endpoint: Endpoint) -> Self {
        Self::pinned(Some(endpoint))
    }

    fn pinned(endpoint: Option<Endpoint>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            table: Arc::new(Mutex::new(StreamTable::new())),
            next_generation: Arc::new(Mutex::new(1)),
            edges: Arc::new((Mutex::new(0), Condvar::new())),
            endpoint,
        }
    }

    /// Where this client dials.
    fn dial(&self) -> Result<Endpoint, DaemonError> {
        match &self.endpoint {
            Some(pinned) => Ok(pinned.clone()),
            None => endpoint::endpoint(),
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

        let control = Control::connect(self.dial()?.listening())?;
        let identity = control.identity().clone();
        *held = Some(Connected {
            control,
            stream: None,
            identity: identity.clone(),
            // No generation yet, deliberately. Generations number *stream* connections, and
            // this has opened none — see [`Self::supersede_stream`].
            stream_generation: None,
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

    /// Take the webview's channel — or any other [`FrameSink`] — and open the output
    /// connection.
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
    pub fn attach_channel(&self, sink: impl FrameSink) -> Result<(), DaemonError> {
        let socket = {
            // The lock is not held across the connect: opening a socket and shaking hands
            // can block for as long as the daemon takes, and every other command would
            // queue behind it.
            let held = self.lock()?;
            held.as_ref().ok_or(DaemonError::Disconnected)?;
            endpoint::open(self.dial()?.listening())?
        };

        let mut reader = std::io::BufReader::new(socket);
        endpoint::handshake(&mut reader, ClientRole::Stream, &endpoint::client_id()?)?;

        // Claim this stream connection and take the reader it supersedes, **before** the
        // table below is emptied. Dropping the superseded `Stream` signals it to stop — it
        // does not join it, see `Stream`'s `Drop` — and doing that first is what keeps an
        // ordinary supersede out of the log: the old reader resolves every frame's id
        // against the shared table, so resetting the table under it makes the frame it is
        // holding look like one the daemon never assigned, and it reports a desync for a
        // reload that went perfectly. A reader already parked inside a `read` can still
        // surface one such frame; the signal cannot reach it until the read returns.
        let (generation, superseded) = self.supersede_stream()?;
        drop(superseded);

        // **Every id this window held is void.** Proto scopes a `StreamId` to one stream
        // connection — "neither id means anything on the other's connection" — so ids
        // learned over the connection being replaced describe nothing here. Keeping them
        // was how a reconnect left every existing pane silent for the life of the process:
        // `attach_session` answered from the table without a round trip, the daemon was
        // never asked to route anything on the new connection, and the chrome said ready.
        if let Ok(mut table) = self.table.lock() {
            *table = StreamTable::new();
        }

        let notify = self.clone();
        let stream = Stream::spawn(
            reader,
            Arc::clone(&self.table),
            Dispatcher::spawn(sink),
            CreditWindow::DEFAULT,
            Box::new(move || notify.disconnect_generation(generation)),
        )?;

        // The reader this replaces was signalled above, so this only installs the new one.
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

        // Retried while the daemon says the attempt could still work. The two connections
        // are independent: `connect` returns once the daemon has answered the control
        // `hello`, which is a moment before it has served the *stream* connection's
        // handshake and bound the hub that a stream id would route to. An attach in that gap
        // is refused with `retryable`, and the refusal is marked so precisely because the
        // race is ordinary rather than a fault.
        //
        // Today a `session_list` round trip happens to sit in front of this and usually
        // hides the gap. That is luck, not ordering — nothing makes it true, and a window
        // whose first attach lost the race put a pane on screen that no output ever reached.
        let answer = self.attach_once(&handle)?;
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

    /// Ask the daemon to route `handle`, retrying while it says another attempt could work.
    ///
    /// Bounded, and deliberately short: the only refusal this is meant to ride out is the
    /// daemon binding the stream hub a moment after answering the handshake. Anything still
    /// refusing after [`ATTACH_RETRY_BUDGET`] is a real answer and belongs in front of the
    /// user, not in a loop.
    ///
    /// [`Self::request`] tears the connection down on an `Io` or `Protocol` failure, and
    /// those are not retried here — reusing a socket that will never answer again is the
    /// thing that teardown exists to prevent.
    fn attach_once(&self, handle: &SessionHandle) -> Result<ResponsePayload, DaemonError> {
        let deadline = std::time::Instant::now() + ATTACH_RETRY_BUDGET;
        loop {
            let attempt = self.request(RequestPayload::StreamAttach(StreamAttach {
                handle: handle.clone(),
            }));
            let Err(refusal) = attempt else {
                return attempt;
            };
            let worth_retrying =
                matches!(&refusal, DaemonError::Daemon(envelope) if envelope.is_retryable());
            if !worth_retrying || std::time::Instant::now() >= deadline {
                return Err(refusal);
            }
            std::thread::sleep(ATTACH_RETRY_PAUSE);
        }
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

    /// Block until the connection is torn down, or until a reload replaces its output
    /// connection.
    ///
    /// How the webview learns the daemon went away while no command was in flight, without
    /// `emit` (tauri#12724). Returns immediately when nothing is attached, so a caller that
    /// races a disconnect is not left waiting for an edge that already passed.
    ///
    /// **A supersede wakes it too, and that is not a rounding error.** This parks a thread
    /// from the blocking pool, and the webview that invoked it is gone after a reload — so a
    /// wait that only ever ended on a disconnect left one more thread parked for every
    /// reload, against a connection that may stay up for days. Enough of them and the pool
    /// the *live* webview's commands run on is empty, and the window stops answering with
    /// nothing in the log to say why.
    ///
    /// The replacement webview cannot be woken by its own attach: [`Self::attach_channel`]
    /// runs to completion inside the connect sequence, and the watch is only invoked once
    /// that sequence has returned. What wakes here is a watcher from a webview that no
    /// longer exists, and its answer goes nowhere.
    pub fn wait_for_disconnect(&self) {
        let (count, signal) = &*self.edges;
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
        self.announce_edge();
    }

    /// Tear the connection down **only if** `generation` still names its live stream.
    ///
    /// What a dying reader calls. A reader superseded by a webview reload keeps its socket
    /// until the next byte or EOF — a parked read cannot be interrupted on Windows — so it
    /// can run this long after the window has moved on. Without the check it would take
    /// whichever connection was live, and a reload followed by one frame on the old pipe was
    /// enough to knock out a healthy one with no daemon fault anywhere.
    ///
    /// The comparison is against the *stream* generation for the reason
    /// [`Self::supersede_stream`] gives: a reload leaves the control connection alone, so
    /// anything numbered per control connection matches on both sides of the supersede and
    /// lets exactly the reader this guard exists to stop through.
    pub fn disconnect_generation(&self, generation: Generation) {
        let taken = self.lock().ok().and_then(|mut held| {
            let live = held
                .as_ref()
                .is_some_and(|c| c.stream_generation == Some(generation));
            if live { held.take() } else { None }
        });

        // Nothing to announce when the generation did not match: the connection that died
        // was already gone, and waking `daemon_watch` would have the store reconnect away
        // from a connection that is working.
        if taken.is_some() {
            drop(taken);
            self.announce_edge();
        }
    }

    /// Wake everything waiting on a connection-lifecycle edge.
    fn announce_edge(&self) {
        let (count, signal) = &*self.edges;
        if let Ok(mut current) = count.lock() {
            *current = current.wrapping_add(1);
        }
        signal.notify_all();
    }

    /// Pretend a control connection is live, for tests that have no daemon.
    ///
    /// It holds no stream, exactly as it would between [`Self::connect`] and the first
    /// [`Self::attach_channel`]. Nothing behind it is real; that is enough for the one thing
    /// these tests ask — whether a dying reader is allowed to tear this down.
    #[cfg(test)]
    fn pretend_connected(&self) {
        if let Ok(mut held) = self.lock() {
            *held = Some(Connected {
                control: Control::detached(),
                stream: None,
                identity: DaemonIdentity {
                    pid: std::process::id(),
                    started_at_ms: 0,
                    launch_nonce: "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
                        .parse()
                        .expect("a well-formed nonce"),
                    app_version: "0.1.0".to_owned(),
                },
                stream_generation: None,
            });
        }
    }

    /// How many connection-lifecycle edges have passed, which is what `daemon_watch` waits
    /// on.
    #[cfg(test)]
    fn edge_count(&self) -> u64 {
        let (count, _) = &*self.edges;
        count.lock().map(|held| *held).unwrap_or_default()
    }

    /// Claim the generation for a stream connection replacing the one held, and hand back
    /// the reader it supersedes so the caller can drop it outside the lock.
    ///
    /// **The mint belongs here, not in [`Self::connect`].** A webview reload is a `connect`
    /// that returns early — the control socket is already held — followed by an
    /// [`Self::attach_channel`] that opens a second *stream* socket over it. A generation
    /// taken alongside the control connection is therefore the same number for the reader a
    /// reload abandons and for the reader that replaces it, [`Self::disconnect_generation`]
    /// matches, and the abandoned reader tears down a healthy connection when it finally
    /// wakes — which, against a daemon that leaves the superseded socket open, is minutes
    /// later with nothing to point at.
    ///
    /// The generation is recorded **before** the reader is spawned, so a reader that dies in
    /// the gap between its spawn and its installation can still say which connection died.
    /// Recording it afterwards left that reader's callback unmatched and the connection
    /// standing with nothing reading it.
    fn supersede_stream(&self) -> Result<(Generation, Option<Stream>), DaemonError> {
        // Outside the client lock: `take_generation` takes a lock of its own.
        let generation = self.take_generation();
        let mut held = self.lock()?;
        let connected = held.as_mut().ok_or(DaemonError::Disconnected)?;
        connected.stream_generation = Some(generation);
        let superseded = connected.stream.take();
        drop(held);

        // Outside the lock, and after it: a watcher parked by the webview this supersedes
        // has to be let go, or a reload leaks one blocking-pool thread every time. See
        // [`Self::wait_for_disconnect`].
        self.announce_edge();
        Ok((generation, superseded))
    }

    /// The generation for a stream connection being opened now.
    fn take_generation(&self) -> Generation {
        let Ok(mut next) = self.next_generation.lock() else {
            // A poisoned counter can only make generations collide, which costs a
            // superseded reader's tear-down check its precision. Refusing to connect over
            // it would be the larger failure.
            return 0;
        };
        let generation = *next;
        *next = next.wrapping_add(1);
        generation
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

    /// A reader superseded by a **webview reload** must not take the live connection.
    ///
    /// Built the way the reload actually happens, which is the whole point: *one* control
    /// connection — `connect` returns early when it already holds one — and two stream
    /// connections opened over it, because a reload re-runs `attach_channel` and nothing
    /// else. A generation minted alongside the control connection gives both readers the
    /// same number, so the guard in `disconnect_generation` matches and the abandoned reader
    /// tears down its own replacement when the superseded pipe finally sees a byte or an EOF
    /// — against a daemon that leaves that socket open, minutes later and with no cause
    /// anyone can see.
    ///
    /// A version of this test that took a fresh generation twice by hand passed on that
    /// broken code: two generations is what the *daemon-fault* path produces, and that path
    /// was never the one in question.
    #[test]
    fn a_reader_superseded_by_a_reload_cannot_tear_down_its_replacement() {
        let client = Client::new();
        client.pretend_connected();

        // The first `attach_channel` over that control connection. It supersedes nothing.
        let (superseded, none) = client.supersede_stream().expect("a connection is held");
        assert!(none.is_none(), "there was no reader to supersede yet");

        // The reload: the same control connection, a second stream connection over it.
        let (live, _) = client
            .supersede_stream()
            .expect("the control connection is untouched by a reload");
        assert_ne!(
            superseded, live,
            "a reload took the same generation twice, so the reader it abandoned is \
             indistinguishable from the one that replaced it"
        );

        // The disconnect counter is what `daemon_watch` waits on, so it is also the proof
        // that nothing woke the store.
        let quiet = client.edge_count();
        client.disconnect_generation(superseded);
        assert!(
            client.identity().is_some(),
            "the reader a reload abandoned took the connection that replaced it down with it"
        );
        assert_eq!(
            client.edge_count(),
            quiet,
            "nothing may wake the reconnect loop away from a connection that is working"
        );

        // The reader that does own the live stream connection still tears it down: this is
        // the daemon-fault path, and the guard must not have cost it.
        client.disconnect_generation(live);
        assert!(client.identity().is_none());
        assert!(client.edge_count() > quiet);
    }

    #[test]
    fn replacing_the_stream_connection_releases_the_watcher_a_reload_left_parked() {
        // `daemon_watch` parks a thread of the blocking pool, and the webview that invoked
        // it is gone after a reload. A wait that only ever ended on a disconnect leaked one
        // of those threads per reload against a connection that may stay up for days —
        // until the pool the live webview's commands run on was empty and the window stopped
        // answering, with nothing in the log.
        let client = Client::new();
        client.pretend_connected();

        let first = client.edge_count();
        let _ = client.supersede_stream().expect("a connection is held");
        assert!(
            client.edge_count() > first,
            "the first attach woke nothing, so a watcher parked before it would never return"
        );

        let second = client.edge_count();
        let _ = client
            .supersede_stream()
            .expect("the connection is still held");
        assert!(
            client.edge_count() > second,
            "a reload left its predecessor's watcher parked"
        );

        // And the connection itself is untouched: waking a watcher is not a disconnect.
        assert!(client.identity().is_some());
    }

    #[test]
    fn a_connection_with_no_reader_yet_cannot_be_torn_down_by_an_earlier_one() {
        // The gap between `connect` and the first `attach_channel`: the control socket is
        // held and no stream connection has been opened over it, so no reader may claim it.
        let client = Client::new();
        let stale = client.take_generation();
        client.pretend_connected();

        client.disconnect_generation(stale);
        assert!(
            client.identity().is_some(),
            "a reader from an earlier connection claimed one that has opened no stream yet"
        );
    }

    #[test]
    fn generations_are_distinct_for_each_stream_connection() {
        // They are the only thing separating a superseded reader from a live one.
        let client = Client::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            assert!(
                seen.insert(client.take_generation()),
                "a generation repeated"
            );
        }
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
