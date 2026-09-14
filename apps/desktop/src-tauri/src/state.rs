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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::channel::{Dispatcher, FrameSink};
use crate::daemon::control::Control;
use crate::daemon::stream::Stream;
use crate::daemon::{DaemonError, endpoint};
use nysia_core::rpc::{DiscoveryError, Endpoint, EnsureError, Probed, SpawnPolicy, ensure_daemon};
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

/// The file name of the runtime this window starts when nothing is listening.
///
/// One binary is both the daemon and the CLI, selected by argv (D-11), and one rule finds it
/// in both worlds a window runs in: **beside this executable**. In an installed bundle that
/// is the sidecar Tauri laid down — `nysia.exe` beside `nysia-desktop.exe` on Windows,
/// `Contents/MacOS/nysia` beside `Contents/MacOS/Nysia` in the app; in a developer's tree it
/// is the binary cargo just built in `target/<profile>`. Nothing path-shaped is baked in at
/// compile time (traps register #9), and in development it is always the freshly compiled
/// runtime rather than a copy of one.
///
/// That second half is only true because the sidecar is declared in
/// `tauri.bundle.conf.json`, which **`cargo` never reads**. `tauri_build` acts on
/// `externalBin` at compile time by deleting `target/<profile>/nysia` and copying the staged
/// file over it, and a daemon that outlived its window is running from the file being
/// deleted — so keeping the declaration out of `tauri.conf.json` is what stops every
/// `cargo build` from doing that. It is not that nothing ever does: `tauri build --config`
/// merges the file and exports `TAURI_CONFIG`, which `tauri_build` does read, so the delete
/// and the copy still happen on the bundle path — once, when somebody actually wants them.
/// See `scripts/sidecar.ts`.
const RUNTIME_BINARY: &str = if cfg!(windows) { "nysia.exe" } else { "nysia" };

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

/// Where a client looks for the runtime to start when nothing is listening.
///
/// An enum rather than an `Option<PathBuf>` because "start the one that shipped with me",
/// "start this exact file", "look beside somewhere else" and "start nothing" are four
/// different intentions, and only the first is production's. §6's rule about security
/// defaults applies to the shape:
/// the window's own path resolves the sidecar and there is no argument on it that could name
/// something else.
#[derive(Debug, Clone)]
enum RuntimeBinary {
    /// Beside this executable — the sidecar in a bundle, the sibling in `target/`.
    Sidecar,
    /// A path a test pinned, so it can prove a first launch without building a bundle.
    #[cfg(test)]
    Pinned(PathBuf),
    /// The sidecar rule, applied beside a window a test placed rather than beside this one.
    ///
    /// The seam the missing-sidecar case needs, and it exists because the obvious test is
    /// vacuous: `target/<profile>/deps` holds a `nysia` of cargo's own, so [`sidecar`] asked
    /// for the missing case *from a test runner* finds one and proves nothing. This runs the
    /// same [`beside`] — the production check, not a stand-in — against a directory that is
    /// genuinely empty.
    #[cfg(test)]
    BesideWindow(PathBuf),
    /// Do not start one.
    ///
    /// What [`Client::at`] selects. The interop harness binds a daemon of its own, and a
    /// client that quietly started a second one beside it would be testing something nobody
    /// wrote.
    #[cfg(test)]
    Never,
}

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
    /// Which runtime this client may start when nothing is listening.
    binary: RuntimeBinary,
    /// Whether a runtime this client started has yet to answer.
    ///
    /// Set when a readiness wait runs out, cleared the moment anything answers. While it is
    /// set [`Self::spawn_policy`] yields [`SpawnPolicy::Never`], so every later attempt is a
    /// **probe** and not a second spawn.
    ///
    /// That is the whole of the slow-first-launch fix, and both halves are load-bearing.
    /// Twenty seconds is a bound on one call rather than a verdict on the daemon — Defender
    /// scanning a binary it has never seen is exactly a first launch — so the window has to
    /// keep looking, which is what makes the timeout retryable. But *keep looking* is not
    /// *keep starting*: the runtime is already running, and a window that answered the
    /// timeout by spawning another one every twenty seconds would pile processes onto the
    /// machine that was too slow to start the first, which is the defect the stop semantics
    /// were added to close, reappearing under a friendlier status.
    ///
    /// Outside the client lock, and an atomic, because [`Self::connect`] reads it before the
    /// seam runs and writes it after — neither under a lock this module forbids waiting
    /// under.
    pending_runtime: Arc<AtomicBool>,
}

impl Client {
    /// A client holding no connection, which finds the daemon the way the window does —
    /// and starts one when there is none.
    pub fn new() -> Self {
        Self::configured(None, RuntimeBinary::Sidecar)
    }

    /// A client that dials `endpoint` instead of resolving one from the environment.
    ///
    /// For the interop tests, which bind a daemon of their own and must not find — or
    /// disturb — the one the developer has running. Deliberately a second named
    /// constructor rather than an argument on [`Self::new`]: the window must not be one
    /// parameter away from talking to a daemon nobody chose.
    #[cfg(test)]
    pub fn at(endpoint: Endpoint) -> Self {
        Self::configured(Some(endpoint), RuntimeBinary::Never)
    }

    /// A client that dials `endpoint` and may start `program` when nothing answers there.
    ///
    /// The proof of a first launch, and nothing else: a test has no bundle, so the sidecar
    /// rule below cannot find the binary `cargo` built two directories up. Named apart from
    /// [`Self::at`] so that a call site able to start a process is obvious in a diff.
    #[cfg(test)]
    pub fn at_with_runtime(endpoint: Endpoint, program: PathBuf) -> Self {
        Self::configured(Some(endpoint), RuntimeBinary::Pinned(program))
    }

    /// A client that dials `endpoint` and looks for its runtime beside `window`.
    ///
    /// The proof that a missing sidecar does not cost a daemon that is already listening.
    /// Named apart from [`Self::at`] because it is the only constructor that can reach the
    /// production resolver's *failure*, which is the case that mattered.
    #[cfg(test)]
    pub fn at_with_sidecar_beside(endpoint: Endpoint, window: PathBuf) -> Self {
        Self::configured(Some(endpoint), RuntimeBinary::BesideWindow(window))
    }

    fn configured(endpoint: Option<Endpoint>, binary: RuntimeBinary) -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            table: Arc::new(Mutex::new(StreamTable::new())),
            next_generation: Arc::new(Mutex::new(1)),
            edges: Arc::new((Mutex::new(0), Condvar::new())),
            endpoint,
            binary,
            pending_runtime: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Where this client dials.
    fn dial(&self) -> Result<Endpoint, DaemonError> {
        match &self.endpoint {
            Some(pinned) => Ok(pinned.clone()),
            None => endpoint::endpoint(),
        }
    }

    /// What this client may start when nothing is listening.
    ///
    /// **Asked only after a probe has found nothing**, which is why the sidecar lookup — the
    /// one thing here that can fail permanently — is safe to do at all. See
    /// [`nysia_core::rpc::ensure_daemon`]'s contract, step 2.
    fn spawn_policy(&self) -> Result<SpawnPolicy, DaemonError> {
        if self.awaiting_runtime() {
            // One is already on its way. Everything this client does from here is probing.
            return Ok(SpawnPolicy::Never);
        }
        match &self.binary {
            RuntimeBinary::Sidecar => Ok(SpawnPolicy::IfAbsent {
                program: sidecar()?,
            }),
            #[cfg(test)]
            RuntimeBinary::Pinned(program) => Ok(SpawnPolicy::IfAbsent {
                program: program.clone(),
            }),
            // The leaf name is irrelevant — [`beside`] reads the parent — but it is spelled
            // the way an installed window is, so a failure message reads like a real one.
            #[cfg(test)]
            RuntimeBinary::BesideWindow(window) => Ok(SpawnPolicy::IfAbsent {
                program: beside(window)?,
            }),
            #[cfg(test)]
            RuntimeBinary::Never => Ok(SpawnPolicy::Never),
        }
    }

    /// Whether a runtime this client started has yet to answer.
    fn awaiting_runtime(&self) -> bool {
        self.pending_runtime.load(Ordering::Acquire)
    }

    /// Remember that the runtime was started and the readiness wait ran out before it bound.
    fn remember_runtime_started(&self) {
        self.pending_runtime.store(true, Ordering::Release);
    }

    /// Forget it: something answered, so there is nothing left on its way.
    fn forget_pending_runtime(&self) {
        self.pending_runtime.store(false, Ordering::Release);
    }

    /// Connect, starting a daemon if none is listening, or return the identity of the one
    /// already attached.
    ///
    /// **This is where a first launch stops being an instruction to open a terminal.** The
    /// window does not reimplement spawn-if-absent: it calls
    /// [`nysia_core::rpc::ensure_daemon`], the same seam `nysia <verb>` reaches through
    /// [`nysia_core::rpc::discover`], so the spawn lock that settles two clients racing and
    /// the lease that separates a live daemon from a stale record have exactly one
    /// implementation between the two front ends (§12 q5).
    ///
    /// ## The sidecar is resolved after the first probe, never before
    ///
    /// Attaching needs no runtime; only starting one does. The lookup that finds the binary
    /// beside this executable fails **permanently** when it is not there — "reinstall
    /// Nysia", which the store obeys by stopping for the life of the window — so a window
    /// that resolved it up front refused a daemon that was listening and answering, and
    /// contradicted step 1 of the seam's own contract from inside its caller. The policy is
    /// therefore a closure the seam calls only once a probe has found nothing.
    ///
    /// ## Nothing here is done under the client lock
    ///
    /// Starting a daemon and waiting for it to answer takes seconds, and this module's one
    /// rule is that nothing waits on another thread while holding that lock. A connect that
    /// held it across the spawn would park every command, every disconnect watcher and every
    /// reconnect behind it — indistinguishable, from the webview, from the freeze the
    /// blocking-command discipline exists to prevent. So the seam runs outside the lock and
    /// the connection is installed after it, and a connect that lost a race to another
    /// command in the meantime drops its own and answers with the one that is already there.
    ///
    /// # Errors
    ///
    /// Whatever the handshake produced, or [`DaemonError::Spawn`] when there was no daemon
    /// and none could be started.
    pub fn connect(&self) -> Result<DaemonIdentity, DaemonError> {
        if let Some(identity) = self.identity() {
            return Ok(identity);
        }

        let endpoint = self.dial()?;
        // Read before the seam runs, so a failure describes the attempt that was actually
        // made: a window already waiting on a runtime it started is not a window refusing
        // to start one.
        let waiting = self.awaiting_runtime();
        // Filled in by the policy closure below, if the seam ever gets as far as asking.
        // Kept because [`spawn_failure`] has to name the program that would not run, and
        // once the call returns this cell is the only party that saw it.
        let resolved: std::cell::OnceCell<SpawnPolicy> = std::cell::OnceCell::new();

        let outcome = ensure_daemon(
            &endpoint,
            || {
                let policy = self.spawn_policy()?;
                let _ = resolved.set(policy.clone());
                Ok(policy)
            },
            || match Control::connect(endpoint.listening()) {
                Ok(control) => {
                    let identity = control.identity().clone();
                    Ok(Probed::Answering {
                        connection: control,
                        identity,
                    })
                }
                // The **only** answer that may lead to a spawn. A refused handshake means
                // something is listening, and starting a second daemon beside it is the
                // failure the seam's two-valued answer exists to prevent.
                Err(DaemonError::Unreachable { .. }) => Ok(Probed::Absent),
                Err(other) => Err(other),
            },
        );

        let ensured = match outcome {
            Ok(ensured) => ensured,
            Err(error) => {
                // The readiness wait ran out, which means the runtime **was** started and is
                // still on its way. Every later attempt probes for it rather than starting
                // a second one; see [`Self::pending_runtime`].
                if matches!(
                    error,
                    EnsureError::Discovery(DiscoveryError::NeverReady { .. })
                ) {
                    self.remember_runtime_started();
                }
                return Err(spawn_failure(
                    &Attempt {
                        endpoint: &endpoint,
                        policy: resolved.get(),
                        waiting,
                    },
                    error,
                ));
            }
        };

        // Something answered, so nothing this client started is still on its way — and if
        // this daemon dies later, the window is free to start another.
        self.forget_pending_runtime();

        if ensured.spawned {
            tracing::info!(
                endpoint = %endpoint.listening(),
                "no daemon was listening, so the window started the one it ships with"
            );
        }
        if !ensured.lease_matches {
            tracing::warn!(
                endpoint = %endpoint.listening(),
                "the daemon answering does not match the lease beside it; the record is stale"
            );
        }

        let control = ensured.connection;
        let identity = control.identity().clone();
        let mut held = self.lock()?;
        if let Some(connected) = held.as_ref() {
            // Another command connected while this one was starting a daemon. The window
            // holds one connection, so this one is dropped — outside the lock, per the rule
            // above, which is why the guard goes first.
            let settled = connected.identity.clone();
            drop(held);
            drop(control);
            return Ok(settled);
        }
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
        {
            let held = self.lock()?;
            held.as_ref().ok_or(DaemonError::Disconnected)?;
        }

        // **Claimed before the socket is opened, not after the handshake.** The daemon
        // supersedes this window's stream connection the moment it answers the *new*
        // hello, and closes the one it replaces. So between the hello being answered and
        // this line, the old reader can reach end-of-file and run `disconnect_generation`
        // while its generation is still the live one — which takes the healthy control
        // connection with it, fails this attach with `Disconnected`, and has the window
        // announce it is not connected to a daemon and reconnect.
        //
        // The natural window is small but it is not microseconds: this takes the inner
        // lock, and a control round trip still in flight from the webview being replaced —
        // a keystroke, a resize — holds that lock for the length of the round trip.
        //
        // Claiming first inverts the failure. A handshake that then fails leaves a
        // generation no reader owns: nothing can tear this connection down through it, the
        // attach returns the handshake's own error, and the store's reconnect replaces it.
        // That is the direction to be wrong in.
        //
        // Taking the superseded reader here signals it to stop — dropping a `Stream` signals
        // and returns, it never joins — before the table below is emptied under it. A reader
        // that resolves a frame against a table reset out from under it reports a desync for
        // a reload that went perfectly; one already parked inside a `read` can still surface
        // one such frame, because the signal cannot reach it until the read returns.
        let (generation, superseded) = self.supersede_stream()?;
        drop(superseded);

        let socket = endpoint::open(self.dial()?.listening())?;
        let mut reader = std::io::BufReader::new(socket);
        endpoint::handshake(&mut reader, ClientRole::Stream, &endpoint::client_id()?)?;

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

        // The table above is the source of truth, and it is written first for the reason
        // `daemon::stream::route` gives: a signal alone would race the frames the daemon is
        // already entitled to send. What the signal adds is a **wake**. The daemon flushes
        // this response before it enqueues a single replay frame, so the reader can be
        // holding one for this very id — and if it is blocked because no attached stream can
        // take another chunk, nothing else would wake it to notice the table has changed.
        {
            let Ok(held) = self.lock() else {
                return Ok(attached.stream_id);
            };
            if let Some(live) = held.as_ref().and_then(|c| c.stream.as_ref()) {
                live.attached();
            }
        }
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

/// The `nysia` runtime that ships beside the window.
///
/// **Beside this executable, and nowhere else.** Not `PATH`: the window would then start
/// whichever `nysia` a shell happened to put first, which is a daemon nobody chose and, at
/// best, a different version of the protocol this build speaks. The bundle ships one; that is
/// the one to run.
///
/// The path is checked here rather than left to `CreateProcess`, which reports a missing
/// program as a bare `NotFound` (traps register #8) — and "the app is missing the binary it
/// ships with" is worth saying in those words, because reinstalling is the fix and no amount
/// of reconnecting is.
///
/// # Errors
///
/// [`DaemonError::Spawn`], which is not retryable: a bundle that is missing its runtime will
/// still be missing it on the next attempt.
fn sidecar() -> Result<PathBuf, DaemonError> {
    let exe = std::env::current_exe().map_err(|error| DaemonError::Spawn {
        message: format!("Nysia could not find its own program file: {error}"),
        next_step: format!("Reinstall Nysia. {THEN_REOPEN}"),
    })?;
    beside(&exe)
}

/// The runtime that belongs next to `exe`, checked.
///
/// Split from [`sidecar`] so the answer for a directory with no runtime in it can be tested:
/// the one that matters is the *failure*, and a test runner cannot arrange for its own
/// executable to live somewhere else. `target/<profile>/deps` happens to hold a `nysia` of
/// cargo's own, so asking [`sidecar`] for the missing case in-process tests nothing.
fn beside(exe: &std::path::Path) -> Result<PathBuf, DaemonError> {
    let beside = exe
        .parent()
        .map(|dir| dir.join(RUNTIME_BINARY))
        .ok_or_else(|| DaemonError::Spawn {
            message: format!(
                "Nysia is installed at {}, which has no directory to hold the runtime it starts",
                exe.display()
            ),
            next_step: format!("Reinstall Nysia somewhere ordinary. {THEN_REOPEN}"),
        })?;
    if !beside.is_file() {
        return Err(DaemonError::Spawn {
            message: format!(
                "the Nysia runtime is not installed beside the app: {} does not exist",
                beside.display()
            ),
            next_step: format!(
                "Reinstall Nysia — this copy shipped without the runtime it starts, which \
                 belongs at {}. {THEN_REOPEN}",
                beside.display()
            ),
        });
    }
    Ok(beside)
}

/// A daemon that was started and has not answered **yet**.
///
/// Retryable, and the next step says so in as many words. Everything a window can do about a
/// runtime that is still starting it is already doing; what a person needs to know is that it
/// has not given up, and where to look if the wait never ends. Deliberately **not**
/// [`THEN_REOPEN`] — that sentence tells the reader the window has stopped, and here it has
/// not.
fn still_starting(endpoint: &Endpoint, message: String) -> DaemonError {
    DaemonError::Starting {
        message,
        next_step: format!(
            "Nysia is still trying. If it never connects, the runtime wrote why to {}.",
            endpoint.log_path().display()
        ),
    }
}

/// The sentence every spawn failure ends with.
///
/// **Not "run `nysia --daemon`".** That was advice for a developer: an installed app puts its
/// runtime beside itself and nothing of the sort on `PATH`, so a user who followed it got
/// "command not found" on top of the failure they already had. Where the path is known the
/// caller names it; what is common to all of them is the second half, because the window
/// **stops** on a failure it was told is permanent — `DaemonStore.run` returns rather than
/// spawning a process every twenty seconds — so fixing the cause outside the window is only
/// half of what the user has to do.
const THEN_REOPEN: &str = "Then reopen Nysia: it stops trying once starting a daemon cannot \
                           work.";

/// What one attempt to reach a daemon was allowed to do, for the sentence it has to compose.
struct Attempt<'a> {
    /// Where it dialled.
    endpoint: &'a Endpoint,
    /// The policy, if the seam got as far as resolving one.
    ///
    /// `None` when it did not, which is the ordinary case for a daemon that answered and for
    /// a probe that failed. A spawn that went wrong always has one, because a spawn cannot
    /// happen without it.
    policy: Option<&'a SpawnPolicy>,
    /// Whether a runtime this client started was already on its way when it began.
    ///
    /// The one thing that separates the two meanings of [`SpawnPolicy::Never`]: a client
    /// that may not start a daemon at all, and a window waiting for the one it started.
    waiting: bool,
}

/// What the user is told when no daemon could be reached or started.
///
/// Each arm settles the sentence **and** whether the window keeps trying, and the two have to
/// agree. The store reconnects on a retryable failure and stops on one that is not, so a
/// permanent fault reported as retryable is a window that says *Reconnecting* for ever
/// without once saying why — and a temporary one reported as permanent is the opposite
/// mistake, a window that has stopped beside a daemon that came up a moment later and works.
fn spawn_failure(attempt: &Attempt<'_>, error: EnsureError<DaemonError>) -> DaemonError {
    let discovery = match error {
        // The probe is this module's own dial, so its failure already carries the window's
        // phrasing and its own verdict on retrying. Passed through untouched.
        EnsureError::Probe(failure) => return failure,
        // So is the policy's. Resolving the sidecar composes "reinstall Nysia, then reopen
        // it" and names the file that is missing, which is worth more to a person than
        // anything this function could say about it.
        EnsureError::Policy(failure) => return failure,
        EnsureError::Discovery(discovery) => discovery,
    };
    match discovery {
        // Nothing is listening and this client did not start one — because it already has.
        // Retryable, and the message says what is being waited for rather than implying a
        // policy the user could change.
        DiscoveryError::Absent { endpoint } if attempt.waiting => still_starting(
            attempt.endpoint,
            format!("the runtime Nysia started has not answered on {endpoint} yet"),
        ),
        DiscoveryError::Absent { endpoint } => DaemonError::Unreachable {
            endpoint,
            cause: "nothing is listening, and this client may not start one".to_owned(),
        },
        // The bundle's runtime exists and still would not run: a refused execute, a binary
        // for another architecture, an antivirus holding the file. Waiting does not fix any
        // of them.
        DiscoveryError::Spawn(cause) => DaemonError::Spawn {
            message: format!("Nysia could not start its runtime: {cause}"),
            // The program, not the runtime directory. A next step that named the wrong file
            // sends the reader to look at something that was never the problem.
            next_step: match attempt.policy {
                Some(SpawnPolicy::IfAbsent { program }) => format!(
                    "Check that {} can be run on this machine. {THEN_REOPEN}",
                    program.display()
                ),
                Some(SpawnPolicy::Never) | None => {
                    format!("Start a daemon yourself. {THEN_REOPEN}")
                }
            },
        },
        // It ran, and twenty seconds was not enough. **Not a verdict**: the process exists,
        // and a first launch on a machine whose antivirus has never seen this binary is
        // exactly when a bind arrives late. The window keeps probing for it — without
        // starting another — so the sentence says what it is waiting for and where to look
        // if it never arrives.
        DiscoveryError::NeverReady { seconds, .. } => still_starting(
            attempt.endpoint,
            format!("Nysia started its runtime and it has not answered within {seconds} seconds"),
        ),
        // Somebody else is mid-spawn. Retryable, and the window's reconnect is exactly the
        // right response: the daemon they are starting is the one this window wants.
        DiscoveryError::LockTimeout { seconds } => DaemonError::Io(format!(
            "another process has been starting a daemon for {seconds} seconds"
        )),
        // Unreachable through this seam — the probe above is the only dial, and it never
        // produces a `nysia_core` client error — but reported rather than panicked on,
        // because §6 keeps panics out of a path a person can reach.
        DiscoveryError::Client(cause) => DaemonError::Io(cause.to_string()),
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

    /// The ordinary constructor goes through the resolver, and the pinned one does not.
    ///
    /// The link the interop module cannot make. Its harness pins an endpoint through
    /// [`Client::at`] so it can bind a daemon of its own, which means a resolver that
    /// answered something wrong would leave every interop test green — they would all dial
    /// the pinned value and find the daemon exactly where they put it.
    ///
    /// Asserted rather than driven through a real connect because pointing the process at a
    /// scratch runtime directory means `set_var`, and that is undefined behaviour against
    /// the `getenv` calls the sibling interop tests make continuously while resolving
    /// programs and scrubbing a session's environment.
    #[test]
    fn the_ordinary_constructor_dials_what_the_resolver_answers() {
        let resolved = endpoint::endpoint().expect("this environment resolves an endpoint");
        assert_eq!(
            Client::new().dial().expect("the window dials").listening(),
            resolved.listening(),
            "the window dials somewhere the resolver did not name"
        );

        // And the pinned constructor stays confined to what it was given, so the seam cannot
        // quietly become the path the window takes.
        let pinned = crate::interop::scratch("dial");
        assert_eq!(
            Client::at(pinned.clone())
                .dial()
                .expect("a pinned client dials"),
            pinned
        );
        assert_ne!(pinned.listening(), resolved.listening());
    }

    /// The generation is claimed **before** the socket is opened, not after the handshake.
    ///
    /// Ordering, made observable without a race. The attach below cannot get past `open` —
    /// nothing is listening on that endpoint — and the claim must already have happened by
    /// then, so the reader this attach supersedes can no longer tear anything down.
    ///
    /// Claimed after the handshake instead, the daemon's supersede-on-hello closes the old
    /// socket first, its reader reaches end-of-file while its generation is still the live
    /// one, and it takes the healthy control connection with it — the attach then fails with
    /// `Disconnected` and the window announces it is not connected to a daemon.
    #[test]
    fn a_failed_attach_has_already_claimed_its_generation() {
        struct Nowhere;
        impl FrameSink for Nowhere {
            fn deliver(&self, _bytes: Vec<u8>) -> Result<(), String> {
                Ok(())
            }
        }

        let client = Client::at(crate::interop::scratch("attach-order"));
        client.pretend_connected();
        let (superseded, _) = client.supersede_stream().expect("a connection is held");

        let refused = client.attach_channel(Nowhere);
        assert!(
            refused.is_err(),
            "this endpoint has no daemon, so the attach must not have got past `open`"
        );

        let quiet = client.edge_count();
        client.disconnect_generation(superseded);
        assert!(
            client.identity().is_some(),
            "the reader superseded by an attach that never opened a socket took the live              connection down, so the generation had not been claimed yet"
        );
        assert_eq!(client.edge_count(), quiet);
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

    /// **The first-launch proof.** No daemon anywhere, and the window reaches a live session.
    ///
    /// This is §12 q6 as a test: before the seam below existed, a machine with nothing
    /// listening gave the user a window stuck on *Reconnecting* and a `+` menu that failed
    /// with "Start the Nysia daemon, then try again". `Client::connect` dialled once and gave
    /// up; there was no second step.
    ///
    /// It goes through the window's own path — [`Client::connect`], the same call
    /// `daemon_connect` makes — rather than a harness of its own, because "the window starts
    /// a daemon" is the claim and any other entry point would be proving somebody else's.
    /// What it substitutes for production is only *which* binary may be started: a test has no
    /// bundle, so it pins the `nysia` that `cargo` has just built. The sidecar rule that
    /// production uses instead is proved separately, below.
    ///
    /// Run against the code before the fix it fails at the first line that matters, with
    /// `Unreachable` from a connect that never tried to start anything.
    #[test]
    fn a_window_with_no_daemon_anywhere_starts_one_and_reaches_a_working_session() {
        let endpoint = crate::interop::scratch("launch");
        let runtime = runtime_binary();
        assert!(
            runtime.is_file(),
            "{} was not built; `cargo test --workspace` builds it before running this",
            runtime.display()
        );
        // Nothing is listening: the endpoint is scratch, and no daemon has ever bound it.
        let client = Client::at_with_runtime(endpoint.clone(), runtime);

        let identity = client
            .connect()
            .expect("the window must start a daemon when there is none");
        assert!(
            identity.pid != std::process::id(),
            "the daemon must be a process of its own, not this one"
        );

        // A daemon that answered `hello` is not yet a *working* session, which is what a user
        // gets nothing from the app without. So: create one, type at it, and read back
        // something the shell had to compute rather than echo.
        let handle = open_shell(&client);
        assert!(
            eventually(|| !screen(&client, &handle).trim().is_empty()),
            "the shell never drew a prompt, so there is nothing to type at"
        );
        for line in token_lines() {
            type_line(&client, &handle, line);
        }
        assert!(
            eventually(|| screen(&client, &handle).contains("NYSIA-42")),
            "the session never ran what was typed at it; the screen was {:?}",
            screen(&client, &handle)
        );

        let _ = client.request(RequestPayload::SessionClose(
            nysia_proto::session::SessionClose {
                handle: handle.clone(),
            },
        ));
        client.disconnect();
        stop_daemon(&endpoint);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// **#48 as a test.** A window with no runtime beside it attaches to one that is already
    /// listening.
    ///
    /// The window resolved its sidecar *before* the first probe, and that lookup fails
    /// permanently — "reinstall Nysia", which the store obeys by stopping for the life of the
    /// window. So a developer's tree with no `nysia` staged, or a bundle that shipped without
    /// one, refused a daemon that was up and answering. Step 1 of the seam's own contract
    /// says the opposite.
    ///
    /// ## Why it is written like this
    ///
    /// The obvious version of this test passes against the broken code, which is how the
    /// defect survived: `target/<profile>/deps` holds a `nysia` of cargo's own, so a runner
    /// asking [`sidecar`] for the missing case *finds one*. The seam is
    /// [`Client::at_with_sidecar_beside`], which runs the production [`beside`] against a
    /// directory that is genuinely empty — and the resolver is asserted to fail before the
    /// connect is attempted, so a later change that made it succeed cannot leave this test
    /// quietly proving nothing.
    ///
    /// Run against the code before the fix it fails on the connect, with the `Spawn` error
    /// from a policy resolved before anything was dialled.
    #[test]
    fn a_window_whose_sidecar_is_missing_still_attaches_to_a_daemon_that_is_listening() {
        let endpoint = crate::interop::scratch("no-sidecar");
        let runtime = runtime_binary();
        assert!(
            runtime.is_file(),
            "{} was not built; `cargo test --workspace` builds it before running this",
            runtime.display()
        );

        // A daemon on this endpoint, brought up by a client that *can* start one. The client
        // under test must never start anything — it has nothing to start.
        let starter = Client::at_with_runtime(endpoint.clone(), runtime);
        let listening = starter.connect().expect("a daemon to attach to");

        // A window installed in a directory with no runtime in it.
        let bare = endpoint.runtime_dir().join("bundle-with-no-runtime");
        std::fs::create_dir_all(&bare).expect("a directory to stand in for a bundle");
        let window = bare.join(if cfg!(windows) {
            "nysia-desktop.exe"
        } else {
            "nysia-desktop"
        });
        let client = Client::at_with_sidecar_beside(endpoint.clone(), window);

        // The resolver really cannot find one, and really is permanent. Without this the
        // test could pass because a sidecar happened to be there.
        let refused = client
            .spawn_policy()
            .expect_err("nothing was installed beside that window");
        assert!(matches!(refused, DaemonError::Spawn { .. }), "{refused:?}");
        assert!(
            !refused.retryable(),
            "the resolver must be the permanent failure this test is about"
        );
        assert!(
            refused.to_string().contains(RUNTIME_BINARY),
            "the resolver must name the runtime it looked for, got {refused}"
        );

        // And the window connects anyway: attaching needs no runtime, only starting one does.
        let attached = client
            .connect()
            .expect("a daemon is listening, so a missing sidecar is beside the point");
        assert_eq!(
            attached.launch_nonce, listening.launch_nonce,
            "the window attached to something other than the daemon that was already there"
        );

        client.disconnect();
        starter.disconnect();
        stop_daemon(&endpoint);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// **#51 as a test.** A readiness timeout is not a verdict, and the window that hit one
    /// attaches when the daemon appears — having started nothing else in the meantime.
    ///
    /// Twenty seconds is a bound on one call. Defender scanning a binary it has never seen is
    /// exactly a first launch, so a daemon that binds at second twenty-five used to sit beside
    /// a window that had already told the user to reopen the app. Reopening worked, which made
    /// a timeout look like a flake.
    ///
    /// The clock is not what is under test and is not waited on. What is: the two transitions
    /// either side of it — a window that has started a runtime **probes** rather than starting
    /// a second one, and the moment anything answers it is an ordinary client again, free to
    /// start one if this daemon ever goes.
    #[test]
    fn a_window_waiting_on_a_slow_runtime_keeps_probing_and_starts_no_second_one() {
        let endpoint = crate::interop::scratch("slow-start");
        let runtime = runtime_binary();
        assert!(
            runtime.is_file(),
            "{} was not built; `cargo test --workspace` builds it before running this",
            runtime.display()
        );

        let client = Client::at_with_runtime(endpoint.clone(), runtime.clone());
        assert!(
            matches!(client.spawn_policy(), Ok(SpawnPolicy::IfAbsent { .. })),
            "a fresh window starts the runtime it ships with"
        );

        // What a readiness wait that ran out leaves behind: the runtime was started, and it
        // has not answered.
        client.remember_runtime_started();
        assert!(
            matches!(client.spawn_policy(), Ok(SpawnPolicy::Never)),
            "a window with a runtime already on its way must probe, not start a second one"
        );

        let waiting = client
            .connect()
            .expect_err("the slow runtime has not bound yet");
        assert!(
            waiting.retryable(),
            "a window that gave up here strands the daemon that is about to work: {waiting:?}"
        );
        assert!(
            waiting.to_string().contains("has not answered"),
            "the notice must say what is being waited for, got {waiting}"
        );
        let steps = waiting.next_steps().join(" ");
        assert!(
            !steps.contains("reopen Nysia"),
            "the window is still trying, so it must not say it has stopped: {steps}"
        );
        assert!(
            steps.contains(&endpoint.log_path().display().to_string()),
            "a wait that never ends has to name the log that would explain it: {steps}"
        );
        assert!(
            !endpoint.pid_record_path().exists(),
            "a second runtime was started for a daemon that was already on its way"
        );

        // The daemon appears. Nothing the user does makes that happen — here a second client
        // stands in for the slow bind — and the window's next probe is all it takes.
        let starter = Client::at_with_runtime(endpoint.clone(), runtime);
        let listening = starter.connect().expect("a daemon");

        let attached = client.connect().expect("the daemon that finally answered");
        assert_eq!(
            attached.launch_nonce, listening.launch_nonce,
            "the window found something other than the daemon that came up"
        );
        assert!(
            matches!(client.spawn_policy(), Ok(SpawnPolicy::IfAbsent { .. })),
            "a window that reached a daemon must be able to start one again if it goes"
        );

        client.disconnect();
        starter.disconnect();
        stop_daemon(&endpoint);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// A readiness timeout says the window is **still trying**, which is the opposite of what
    /// every other spawn failure says.
    ///
    /// Deliberately adjacent to the test below: those two sentences are the only thing telling
    /// a reader whether to wait or to act, and having them the wrong way round is invisible
    /// except in a test that asserts both.
    #[test]
    fn a_runtime_that_has_not_answered_yet_says_so_rather_than_that_it_failed() {
        let endpoint = crate::interop::scratch("not-yet");
        let program = endpoint.runtime_dir().join("slow-nysia-runtime");
        let policy = SpawnPolicy::IfAbsent { program };

        let waiting = spawn_failure(
            &Attempt {
                endpoint: &endpoint,
                policy: Some(&policy),
                waiting: false,
            },
            EnsureError::Discovery(DiscoveryError::NeverReady {
                endpoint: endpoint.listening().to_string(),
                seconds: 20,
            }),
        );

        assert!(
            waiting.retryable(),
            "a window that stopped here would strand a daemon that binds at second 25"
        );
        let steps = waiting.next_steps().join(" ");
        assert!(
            !steps.contains("reopen Nysia"),
            "the window has not stopped, so the notice must not say it has: {steps}"
        );
        assert!(
            steps.contains(&endpoint.log_path().display().to_string()),
            "{steps} names no log for a wait that never ends"
        );
        // A sentence the user reads. The first draft of it ran over two source lines and
        // rustfmt folded the indentation into the string, so it reached the notice list with
        // fourteen spaces in the middle of a clause.
        assert!(
            !steps.contains("  "),
            "the next step is not a sentence a person would write: {steps:?}"
        );
        assert!(
            !steps.contains("`nysia --daemon`"),
            "an installed app has no `nysia` on PATH: {steps}"
        );

        // And the same again for the attempt *after* one of these, which reaches the seam as
        // "nothing is listening and this client may not start one". Same meaning to the user,
        // so it must read the same way — not as a policy they could change.
        let probing = spawn_failure(
            &Attempt {
                endpoint: &endpoint,
                policy: Some(&SpawnPolicy::Never),
                waiting: true,
            },
            EnsureError::Discovery(DiscoveryError::Absent {
                endpoint: endpoint.listening().to_string(),
            }),
        );
        assert!(probing.retryable(), "{probing:?}");
        assert!(
            probing.to_string().contains("has not answered"),
            "got {probing}"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// The production rule, which the proof above deliberately does not use.
    ///
    /// Asserted rather than driven, because driving it would mean putting a `nysia` beside
    /// the test runner — and the value here is the *rule*: beside this executable, never
    /// `PATH`. A window that searched `PATH` would start whichever `nysia` a shell happened to
    /// put first, which is a daemon nobody chose.
    #[test]
    fn the_window_starts_the_runtime_that_ships_beside_it_and_never_one_off_the_path() {
        let exe = std::env::current_exe().expect("a test runner knows its own path");
        let beside = exe
            .parent()
            .expect("it is in a directory")
            .join(RUNTIME_BINARY);
        match sidecar() {
            Ok(found) => assert_eq!(
                found, beside,
                "the window resolved a runtime that is not the one beside it"
            ),
            // The ordinary case for a test runner: `target/debug/deps` holds no `nysia`. What
            // matters is that it said so in a sentence a person can act on, and that it did
            // not fall back to something it found elsewhere.
            Err(error) => {
                assert!(matches!(error, DaemonError::Spawn { .. }), "got {error:?}");
                assert!(
                    !error.retryable(),
                    "reinstalling is the fix; waiting is not"
                );
                assert!(
                    error.to_string().contains(&beside.display().to_string()),
                    "the message must name the path that was tried, got {error}"
                );
                assert!(
                    error.next_steps().iter().all(|step| !step.is_empty()),
                    "a spawn failure with no next step is the notice users dismiss unread"
                );
            }
        }
    }

    /// Every permanent failure names something the user can act on **and** tells them the
    /// window has stopped.
    ///
    /// Both halves, because they are one instruction: `DaemonStore.run` returns on a
    /// non-retryable failure rather than spawning a process every twenty seconds, so a next
    /// step that ended at "start one yourself" would leave the reader in front of a window
    /// that is never going to notice they did.
    ///
    /// And never `nysia --daemon` as a bare command: an installed app puts its runtime beside
    /// itself and nothing on `PATH`, so that advice answers a user with `command not found`.
    #[test]
    fn a_permanent_failure_says_what_to_fix_and_that_the_window_has_stopped() {
        let endpoint = crate::interop::scratch("steps");
        let program = endpoint.runtime_dir().join("no-such-nysia-runtime");
        let policy = SpawnPolicy::IfAbsent {
            program: program.clone(),
        };

        let attempt = Attempt {
            endpoint: &endpoint,
            policy: Some(&policy),
            waiting: false,
        };

        let failures = [spawn_failure(
            &attempt,
            EnsureError::Discovery(DiscoveryError::Spawn(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "refused",
            ))),
        )];

        for failure in failures {
            assert!(
                !failure.retryable(),
                "{failure:?} would have the window loop"
            );
            let steps = failure.next_steps().join(" ");
            assert!(
                steps.contains("reopen Nysia"),
                "{failure:?} leaves the reader waiting on a window that has stopped: {steps}"
            );
            assert!(
                !steps.contains("`nysia --daemon`"),
                "an installed app has no `nysia` on PATH: {steps}"
            );
        }

        // The sidecar's own failure names the file that is missing, which is the only thing
        // that tells a reader whether reinstalling is really the answer. Asked of a directory
        // that has no runtime in it, because the one the test runner lives in does.
        let bare = endpoint.runtime_dir().join("Nysia.exe");
        let missing = beside(&bare).expect_err("nothing was installed beside that");
        let steps = missing.next_steps().join(" ");
        assert!(steps.contains(RUNTIME_BINARY), "{steps} names no runtime");
        assert!(steps.contains("reopen Nysia"), "{steps}");
        assert!(
            !missing.retryable(),
            "a missing runtime does not appear by waiting"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// The `nysia` that `cargo` built for this run.
    ///
    /// Derived from the test runner's own path — `target/<profile>/deps/<test>.exe` — rather
    /// than from `CARGO_MANIFEST_DIR`, which must never be baked into anything path-shaped
    /// (traps register #9) and would in any case name the source tree rather than the build.
    fn runtime_binary() -> PathBuf {
        // The same name production resolves, one directory up: cargo's own output, which no
        // build of the desktop crate writes over, so what this test starts is always what
        // cargo just compiled.
        let exe = std::env::current_exe().expect("a test runner knows its own path");
        exe.parent()
            .and_then(std::path::Path::parent)
            .expect("the runner lives in target/<profile>/deps")
            .join(RUNTIME_BINARY)
    }

    /// Stop the daemon the lease beside `endpoint` describes.
    ///
    /// A daemon this process did not spawn as a child — it is detached, which is the whole
    /// point — so it is found the way any other tool would find it. Left to retire on its own
    /// it would hold its log file open for minutes, and on Windows a `remove_dir_all` against
    /// an open file fails silently, which is how a later run inherits a directory it believed
    /// was fresh.
    fn stop_daemon(endpoint: &Endpoint) {
        let Ok(Some(record)) =
            nysia_core::rpc::PidRecordFile::at(endpoint.pid_record_path()).read()
        else {
            return;
        };
        let pid = record.pid.to_string();
        let stopped = if cfg!(windows) {
            std::process::Command::new("taskkill")
                .args(["/PID", &pid, "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
        } else {
            std::process::Command::new("kill")
                .args(["-TERM", &pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
        };
        let _ = stopped;
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    /// A shell on the daemon, and its handle.
    fn open_shell(client: &Client) -> SessionHandle {
        let answer = client
            .request(RequestPayload::SessionCreate(
                nysia_proto::session::SessionCreate {
                    kind: nysia_proto::identity::SessionKind::Shell,
                    pane_key: None,
                    profile: shell_profile(),
                    cwd: None,
                    env_overrides: std::collections::BTreeMap::new(),
                    cols: 100,
                    rows: 30,
                },
            ))
            .expect("the daemon the window started creates a session");
        match answer {
            ResponsePayload::SessionCreate(created) => created.handle,
            other => panic!(
                "session_create was answered with a {} payload",
                other.verb()
            ),
        }
    }

    /// `cmd` on Windows, the platform's own shell elsewhere.
    ///
    /// Neither needs PowerShell 7 to be installed, which a machine running this may not have.
    fn shell_profile() -> Option<nysia_proto::session::ShellProfile> {
        if cfg!(windows) {
            Some(nysia_proto::session::ShellProfile::Cmd)
        } else {
            None
        }
    }

    /// Lines that make the shell **compute** `NYSIA-42`.
    ///
    /// Computed, never typed: a token that also appears in the line as typed is satisfied by
    /// the kernel's echo, with the shell having run nothing at all.
    fn token_lines() -> Vec<&'static str> {
        if cfg!(windows) {
            // `cmd` expands `%NYS%` as it parses the line, so the assignment is its own
            // command — which also keeps `42` out of everything that is typed.
            vec!["set /a NYS=6*7", "echo NYSIA-%NYS%"]
        } else {
            vec![r#"echo "NYSIA-$((6*7))""#]
        }
    }

    fn type_line(client: &Client, handle: &SessionHandle, text: &str) {
        client
            .request(RequestPayload::TerminalSend(
                nysia_proto::terminal::TerminalSend {
                    handle: handle.clone(),
                    text: text.to_owned(),
                    enter: true,
                    interrupt: false,
                },
            ))
            .expect("the daemon accepts input");
    }

    /// The session's rendered screen, as the CLI and the hooks read it.
    fn screen(client: &Client, handle: &SessionHandle) -> String {
        match client.request(RequestPayload::TerminalRead(
            nysia_proto::terminal::TerminalRead::screen(handle.clone()),
        )) {
            Ok(ResponsePayload::TerminalRead(read)) => read.lines.join("\n"),
            _ => String::new(),
        }
    }

    /// Poll `check` until it holds or half a minute passes. `true` if it held.
    fn eventually(mut check: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if check() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        check()
    }
}
