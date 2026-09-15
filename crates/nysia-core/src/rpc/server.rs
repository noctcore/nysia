//! The daemon: the accept loop, the handshake, the verb dispatch and idle retire.
//!
//! This is the process D-1 is about. It owns the PTYs, the terminal state and — in later
//! waves — the store, git and orchestration. Every consumer is a client of this one socket:
//! the window, `nysia <verb>`, and the hooks. **The window has no privileged path**, and the
//! way that is enforced is that there is no other path to have.
//!
//! # The handshake, in the order it is checked
//!
//! 1. The first frame must be a readable `hello`. Anything else is
//!    [`RejectReason::Malformed`] — including a frame that is a perfectly good *verb*,
//!    because a connection that starts mid-conversation has not agreed what the
//!    conversation is.
//! 2. The version must fall inside [`ProtocolRange::attachable`]. §3.1: Orca is on v36 and
//!    still advertises `[1..36]`, so a newer app adopts an older daemon rather than
//!    orphaning its sessions. Nysia ships the same range check with a one-element range;
//!    widening it is what makes an in-place upgrade non-disruptive.
//! 3. The peer must run as this daemon's own account (§3.2). Refused peers are told
//!    [`RejectReason::Unauthorized`] and never told what the daemon saw.
//! 4. A retiring daemon answers [`RejectReason::ShuttingDown`], which is the only reason
//!    carrying `retryable: true` — the next connection reaches a fresh daemon, where the
//!    other three would reproduce exactly.
//!
//! # Idle retire, and why it is not a verb
//!
//! §3.1 sketches `shutdownIfIdle` as something a caller asks for. `nysia-proto` ships no such
//! verb, and proto is the sole authority on the wire (D-13), so this daemon retires on its
//! own judgement rather than on request: after the last client disconnects, if it holds no
//! session and nothing is in flight, it waits out a grace period and exits.
//!
//! The condition is the conservative one. **A daemon holding a session never retires**,
//! however long nobody is looking at it — that is the entire point of D-1, and an idle timer
//! that could kill a running build would make the guarantee worthless. The grace period is
//! armed only once a client has connected and gone, so a daemon that was just spawned does
//! not exit in the moment before its spawner dials in.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nysia_proto::{
    AgentStatusList, AgentStatusSubscribe, AgentStatusSubscribed, ClientId, ClientRole,
    CreditFrame, CreditWindow, DaemonIdentity, ErrorCode, ErrorEnvelope, FrameDecoder, FrameKind,
    HelloAccepted, HelloRejected, HelloRequest, HelloResponse, LaunchNonce, MutationReceipt,
    PROTOCOL_VERSION, PaneKey, ProtocolRange, RejectReason, RequestEnvelope, RequestId,
    RequestPayload, ResponseEnvelope, ResponsePayload, SessionList, StreamAttached, StreamId,
};
use tokio::io::AsyncWriteExt;

use crate::rpc::agent_status::AgentStatusService;
use crate::rpc::control::{ControlError, ControlReader, ControlWriter};
use crate::rpc::endpoint::Endpoint;
use crate::rpc::errors::{IntoEnvelope, envelope};
use crate::rpc::lease::PidRecordFile;
use crate::rpc::log_file::{self, Trimmed};
use crate::rpc::peer::{CallerSession, PeerCredentials};
use crate::rpc::session::{OwnedSession, SessionRegistry};
use crate::rpc::stream::{BoundStream, ConnectionKey, CreditOutcome, StreamRegistry, StreamSink};
use crate::rpc::transport::{Connection, Listener, TransportError};

/// How long a connected peer has to send its `hello`.
///
/// A connection that says nothing holds a slot and, on Windows, a pipe instance. Ten seconds
/// is far longer than a handshake needs and short enough that a peer cannot accumulate them.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the daemon stays up after the last client leaves, holding nothing.
const DEFAULT_IDLE_RETIRE: Duration = Duration::from_secs(30);

/// How often idle is re-checked.
const IDLE_TICK: Duration = Duration::from_millis(250);

/// How often the log beside the endpoint is measured against its cap.
///
/// Not every write, and not the idle tick either. A `stat` is cheap but a rotation copies up
/// to [`log_file::MAX_LOG_BYTES`], and the thing being defended against is a file that grows
/// over *days* — half a minute of overshoot against an 8 MiB cap is noise, where four checks
/// a second would put a syscall on the daemon's quietest path for nothing.
const LOG_TRIM_INTERVAL: Duration = Duration::from_secs(30);

/// The database file, beside the endpoint in the runtime directory.
///
/// One name in one place: the daemon opens it and a test asserting against it has to be able
/// to name the same file without copying a literal.
pub const STORE_FILE: &str = "nysia.db";

/// How many mutation receipts are kept before the oldest is forgotten.
///
/// A receipt only has to outlive the reconnect that a retry crosses, which is seconds. Keeping
/// them forever would be a map that grows for the daemon's whole life to answer a question
/// nobody will ask again.
const RECEIPT_CAPACITY: usize = 512;

/// Why the daemon could not start or keep serving.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The endpoint could not be resolved.
    #[error(transparent)]
    Endpoint(#[from] crate::rpc::endpoint::EndpointResolveError),
    /// The endpoint could not be bound, or a connection could not be accepted.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The adoption lease could not be written.
    #[error(transparent)]
    Lease(#[from] crate::rpc::lease::LeaseError),
    /// The status store could not be opened or migrated.
    ///
    /// Fatal rather than degraded. A daemon serving sessions with no store would answer
    /// every status verb with an internal error and lose every hook that fired at it, which
    /// looks to a person exactly like hooks being installed wrongly (§2). Failing at the bind
    /// says which of the two it is, once, in the place that can still be read.
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

/// What a daemon needs to know before it binds.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Where to listen, and where the lease belongs.
    pub endpoint: Endpoint,
    /// The Nysia version this build came from, reported in the handshake.
    ///
    /// Reported rather than asserted: D-11 lets the GUI and the daemon run different
    /// versions, and that is what an in-place upgrade requires.
    pub app_version: String,
    /// How long to stay up with no clients and no sessions. `None` never retires.
    pub idle_retire_after: Option<Duration>,
    /// The credit window every stream this daemon opens is given.
    ///
    /// Configurable because `nysia-proto` sends the window to the client in the opening
    /// grant rather than having it hold a copy of the constants — which is exactly what
    /// makes tuning it a daemon-side decision instead of a protocol change. An incoherent
    /// window would deadlock, so [`StreamRegistry::with_window`] refuses one and serves
    /// [`CreditWindow::DEFAULT`] instead.
    pub credit_window: CreditWindow,
    /// Where the D-12 store lives.
    ///
    /// Beside the endpoint by default, so a daemon on an isolated runtime directory gets an
    /// isolated database without anybody having to remember to move it — which is what lets
    /// a test drive real persistence against a real socket with one environment variable.
    pub store_path: PathBuf,
}

impl DaemonConfig {
    /// A configuration for `endpoint` with this crate's version and the default idle grace.
    #[must_use]
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            idle_retire_after: Some(DEFAULT_IDLE_RETIRE),
            credit_window: CreditWindow::DEFAULT,
            store_path: endpoint.runtime_dir().join(STORE_FILE),
            endpoint,
        }
    }
}

/// The long-lived runtime.
#[derive(Debug)]
pub struct Daemon {
    identity: DaemonIdentity,
    endpoint: Endpoint,
    lease: PidRecordFile,
    idle_retire_after: Option<Duration>,
    sessions: Arc<SessionRegistry>,
    streams: Arc<StreamRegistry>,
    status: Arc<AgentStatusService>,
    receipts: Mutex<Receipts>,
    clients: AtomicUsize,
    in_flight: AtomicUsize,
    served_anyone: AtomicBool,
    shutting_down: AtomicBool,
    idle_since: Mutex<Option<Instant>>,
    trimming_log: AtomicBool,
}

impl Daemon {
    /// Bind the endpoint and write the lease.
    ///
    /// The lease goes down *after* the bind, so a record never describes a daemon that is not
    /// listening — a client that read one and then failed to connect would have no way to
    /// tell "starting" from "crashed".
    ///
    /// **The spool is drained here**, after the store is open and before the first connection
    /// can be accepted. §2.3 says the drain happens on daemon start, and "start" has to mean
    /// before anything is served: a client that read a status between the bind and the drain
    /// would be told a pane has no status and then told it has one, for a row that was on
    /// disk the whole time.
    ///
    /// The bind comes first so that the loser of a start race does no work at all — it has
    /// no endpoint, so it has no business opening the winner's database or claiming the
    /// spool the winner is about to drain.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Transport`] when the endpoint is already held — which a spawner
    /// reads as "somebody else won the race" — [`ServerError::Store`] when the database
    /// cannot be opened, and [`ServerError::Lease`] when the record cannot be written.
    pub fn bind(config: DaemonConfig) -> Result<(Arc<Self>, Listener), ServerError> {
        let listener = Listener::bind(&config.endpoint)?;
        let status = Arc::new(AgentStatusService::open(&config.store_path)?);
        drain_spool(config.endpoint.runtime_dir(), &status);
        let identity = DaemonIdentity {
            pid: std::process::id(),
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                }),
            launch_nonce: LaunchNonce::generate(),
            app_version: config.app_version,
        };
        let lease = PidRecordFile::at(config.endpoint.pid_record_path());
        lease.write(&identity)?;

        Ok((
            Arc::new(Self {
                identity,
                endpoint: config.endpoint,
                lease,
                idle_retire_after: config.idle_retire_after,
                sessions: Arc::new(SessionRegistry::new()),
                streams: Arc::new(StreamRegistry::with_window(config.credit_window)),
                status,
                receipts: Mutex::new(Receipts::default()),
                clients: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                served_anyone: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
                idle_since: Mutex::new(None),
                trimming_log: AtomicBool::new(false),
            }),
            listener,
        ))
    }

    /// Who this daemon is, as the handshake reports it.
    #[must_use]
    pub fn identity(&self) -> &DaemonIdentity {
        &self.identity
    }

    /// Where this daemon listens.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The sessions this daemon owns.
    #[must_use]
    pub fn sessions(&self) -> &Arc<SessionRegistry> {
        &self.sessions
    }

    /// The streams this daemon is writing.
    ///
    /// Exposed so a test can wait for a disconnect the daemon has actually *observed* rather
    /// than for one the client merely initiated — the two are a socket apart, and asserting
    /// across that gap is how a test starts passing for the wrong reason.
    #[must_use]
    pub fn streams(&self) -> &Arc<StreamRegistry> {
        &self.streams
    }

    /// The agent status this daemon serves, and the store behind it.
    #[must_use]
    pub fn status(&self) -> &Arc<AgentStatusService> {
        &self.status
    }

    /// Rotate the log beside this endpoint if it has outgrown
    /// [`log_file::MAX_LOG_BYTES`].
    ///
    /// Called from [`Self::serve`] every [`LOG_TRIM_INTERVAL`], and public so a test can put
    /// the question to a *bound* daemon: the schedule is a constant, but "this daemon knows
    /// which file it is filling" is the part worth checking, and a test that called
    /// [`log_file::trim`] with a path of its own would have proved nothing about the wiring.
    ///
    /// Blocking file IO. [`Self::serve`] hands it to a blocking thread rather than awaiting
    /// it in the select, because copying 8 MiB on the accept loop is a stall the whole daemon
    /// feels.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem said. A caller is entitled to ignore it: a failed rotation
    /// leaves the log exactly as it was, which is the state this call exists to improve
    /// rather than one it can make worse.
    pub fn trim_log(&self) -> std::io::Result<Trimmed> {
        let path = self.endpoint.log_path();
        let trimmed = log_file::trim(&path)?;
        if let Trimmed::Rotated { bytes } = trimmed {
            // Into the freshly emptied file, so the first thing a reader finds at the top is
            // where the rest of it went. Without this a log that has rotated looks like a
            // daemon that started thirty seconds ago.
            tracing::info!(
                path = %path.display(),
                moved_to = %log_file::rotation_path(&path, 1).display(),
                bytes,
                "the log passed its cap; the previous contents were rotated aside"
            );
        }
        Ok(trimmed)
    }

    /// Serve until the daemon retires or is interrupted.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Transport`] only when accepting stops working altogether. A
    /// single connection that fails — a peer the kernel will not name, a malformed
    /// handshake — is logged and dropped, because one bad client must not take a daemon full
    /// of live sessions with it.
    pub async fn serve(self: &Arc<Self>, mut listener: Listener) -> Result<(), ServerError> {
        tracing::info!(
            endpoint = %self.endpoint.listening(),
            pid = self.identity.pid,
            "nysiad listening"
        );
        let mut ticker = tokio::time::interval(IDLE_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut log_ticker = tokio::time::interval(LOG_TRIM_INTERVAL);
        log_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(connection) => {
                        let daemon = Arc::clone(self);
                        tokio::spawn(async move { daemon.serve_connection(connection).await });
                    }
                    Err(err) => {
                        // Dropping the listener over one failed accept would take every live
                        // session with it. Logging and going round again is the behaviour D-1
                        // asks for.
                        tracing::warn!(%err, "an accept failed; continuing to serve");
                    }
                },
                _ = ticker.tick() => {
                    if self.should_retire() {
                        tracing::info!("retiring: no clients, no sessions, nothing in flight");
                        break;
                    }
                }
                _ = log_ticker.tick() => self.spawn_log_trim(),
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("interrupted; shutting down");
                    break;
                }
            }
        }
        self.shutdown();
        Ok(())
    }

    /// Stop taking connections, tear down every session and remove the lease.
    ///
    /// Removing the lease is the *clean* path only. A daemon that crashes leaves one behind,
    /// which is exactly why nothing may read its presence as proof of life.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.sessions.close_all();
        self.lease.remove();
    }

    /// Hand a log trim to a blocking thread, unless one is already running.
    ///
    /// Two guards, and they answer different questions. `spawn_blocking` keeps an 8 MiB copy
    /// off the accept loop; the flag keeps a copy that outlasts its interval from being
    /// joined by the next tick, which would rotate twice for one overflow and throw away a
    /// generation of log nobody meant to lose.
    fn spawn_log_trim(self: &Arc<Self>) {
        if self.trimming_log.swap(true, Ordering::AcqRel) {
            return;
        }
        // A guard rather than a store at the end of the closure. Every path through
        // `trim_log` is a `?`-propagated `io::Error`, so a panic there is unlikely rather
        // than reachable — but the cost of being wrong is that the flag is never cleared and
        // the daemon stops trimming for the rest of its life, which under D-1 is days. That
        // is a worse failure than the one the flag exists to prevent, and it would be silent.
        let guard = TrimInProgress(Arc::clone(self));
        tokio::task::spawn_blocking(move || {
            if let Err(err) = guard.0.trim_log() {
                tracing::warn!(%err, "could not rotate the log; it will be tried again");
            }
        });
    }

    /// Whether the idle-retire condition has held for the whole grace period.
    fn should_retire(&self) -> bool {
        let Some(grace) = self.idle_retire_after else {
            return false;
        };
        // A daemon nobody has spoken to yet must not retire out from under the client that is
        // about to dial in — which is the common case immediately after spawn-if-absent.
        let idle = self.served_anyone.load(Ordering::Acquire)
            && self.clients.load(Ordering::Acquire) == 0
            && self.in_flight.load(Ordering::Acquire) == 0
            && self.sessions.is_empty();

        let mut since = lock(&self.idle_since);
        if !idle {
            *since = None;
            return false;
        }
        match *since {
            Some(started) => started.elapsed() >= grace,
            None => {
                *since = Some(Instant::now());
                false
            }
        }
    }

    /// Handshake one connection and then serve it in whichever role it asked for.
    async fn serve_connection(self: Arc<Self>, connection: Connection) {
        let peer = connection.peer().cloned();
        let (reader, writer) = connection.split();
        self.serve_io(ControlReader::new(reader), ControlWriter::new(writer), peer)
            .await;
    }

    /// A connection's whole life, over any pair of streams.
    ///
    /// Split out from [`Self::serve_connection`] so a test can drive it over an in-memory pipe
    /// narrower than one line — which is the only way to hold the daemon *inside* the write
    /// of its own hello answer and look at what a client racing that answer would find.
    async fn serve_io<R, W>(
        self: &Arc<Self>,
        mut reader: ControlReader<R>,
        mut writer: ControlWriter<W>,
        peer: Option<PeerCredentials>,
    ) where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let Some(hello) = self.vet(&mut reader, &mut writer, peer.as_ref()).await else {
            return;
        };

        // **A stream connection is bound before its hello is answered, and after it is
        // vetted.** Both halves of that sentence are load-bearing.
        //
        // Before the answer, because a client may attach the instant its connect returns, and
        // its connect returns when this answer arrives. Bound afterwards, there is a window in
        // which the client believes it has a stream connection and the daemon still routes its
        // name to the previous one — which on a webview reload is the connection this one is
        // about to supersede. The attach is not refused, so nothing tells the client to retry;
        // it succeeds against a sink the supersede then closes, and the pane is silent forever
        // while the window says ready.
        //
        // After the vet, because binding supersedes whatever held that client id. A peer that
        // failed the account check must not be able to take down somebody else's live stream
        // by naming their id in a hello that is about to be refused.
        let bound = match hello.role {
            ClientRole::Stream => Some(self.streams.bind(&hello.client_id)),
            ClientRole::Control => None,
        };

        if writer
            .write_frame(&HelloResponse::Accepted(HelloAccepted::new(
                self.identity.clone(),
            )))
            .await
            .is_err()
        {
            // The client never learned it was accepted, so it will not ack anything on this
            // connection. Leaving it bound would leave the client id pointing at a socket
            // nobody is reading.
            if let Some(bound) = bound {
                self.streams.unbind(bound.key);
            }
            return;
        }

        self.served_anyone.store(true, Ordering::Release);
        self.clients.fetch_add(1, Ordering::AcqRel);
        // `bound` is `Some` exactly for a stream connection, so it is the role from here on.
        // One discriminator rather than two, because two could disagree — and the way they
        // would disagree is by serving a stream connection that was never bound.
        match bound {
            Some(bound) => {
                self.serve_stream(reader, writer, &hello.client_id, bound)
                    .await;
            }
            None => {
                // The ancestry walk is here rather than above the bind: it reads the process
                // table, and nothing that slow belongs between a bind and the answer that
                // tells the client the bind has happened.
                let caller = Caller::new(peer, &hello.client_id, &self.sessions);
                self.serve_control(reader, writer, &caller).await;
            }
        }
        self.clients.fetch_sub(1, Ordering::AcqRel);
    }

    /// Read the first frame and decide. `None` means the connection is finished.
    ///
    /// Refusals are written here, because there is nothing to do after one. An *acceptance* is
    /// not: the caller writes it, so that whatever the accepted connection owes the client can
    /// be in place before the client is told the connection exists.
    async fn vet<R, W>(
        &self,
        reader: &mut ControlReader<R>,
        writer: &mut ControlWriter<W>,
        peer: Option<&PeerCredentials>,
    ) -> Option<HelloRequest>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        // A peer that connects and says nothing holds a slot - and, on Windows, a pipe
        // instance. The timeout is what stops it accumulating them.
        let first = match tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            reader.read_frame::<HelloRequest>(),
        )
        .await
        {
            Ok(read) => read,
            Err(_) => Err(ControlError::Truncated),
        };

        let (hello, rejection) = match first {
            Ok(Some(hello)) => {
                let reason = self.reject_reason(&hello, peer);
                (Some(hello), reason)
            }
            // A clean disconnect before the handshake is a port scan or a health check, not
            // something to answer.
            Ok(None) => return None,
            Err(err) => (
                None,
                Some(RejectReason::Malformed {
                    detail: err.to_string(),
                }),
            ),
        };

        if let Some(reason) = rejection {
            tracing::debug!(reason = ?reason, "refused a hello");
            let _ = writer
                .write_frame(&HelloResponse::Rejected(HelloRejected::new(reason)))
                .await;
            return None;
        }
        hello
    }

    /// Why this `hello` should be refused, or `None` to accept it.
    fn reject_reason(
        &self,
        hello: &HelloRequest,
        peer: Option<&PeerCredentials>,
    ) -> Option<RejectReason> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Some(RejectReason::ShuttingDown);
        }
        // §3.1's range check. One element today; widening it is how a newer app adopts an
        // older daemon instead of orphaning its sessions.
        if !ProtocolRange::attachable().contains(hello.version) {
            return Some(RejectReason::UnsupportedVersion {
                daemon: PROTOCOL_VERSION,
                attachable: ProtocolRange::attachable(),
            });
        }
        match peer {
            Some(peer) if peer.authorize() => None,
            // A peer the kernel would not name is refused exactly as one from another account
            // is. "The kernel did not say" and "the kernel said it is you" must never collapse
            // into the same answer.
            Some(peer) => Some(RejectReason::Unauthorized {
                detail: peer.refusal_detail(),
            }),
            None => Some(RejectReason::Unauthorized {
                detail: "the kernel would not identify the connecting process".to_owned(),
            }),
        }
    }

    /// Serve request/response verbs until the client goes away.
    async fn serve_control<R, W>(
        self: &Arc<Self>,
        mut reader: ControlReader<R>,
        mut writer: ControlWriter<W>,
        caller: &Caller,
    ) where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let request = match reader.read_frame::<RequestEnvelope>().await {
                Ok(Some(request)) => request,
                // The disconnect D-1 is about. Nothing is torn down, nothing is fenced, and
                // the sessions this client was watching go on running.
                Ok(None) => break,
                Err(err) => {
                    // There is no request id to correlate an answer with, so this frame is
                    // answered on a generated one and the connection is closed. A client that
                    // correlates strictly ignores it; a person with a socket tool reads it.
                    tracing::debug!(%err, "closing a control connection over a malformed frame");
                    let _ = writer
                        .write_frame(&ResponseEnvelope::new(
                            RequestId::generate(),
                            ResponsePayload::Error(envelope(
                                ErrorCode::InvalidRequest,
                                err.to_string(),
                                "send one JSON request envelope per line",
                                &["reconnect and send a `hello` before any verb"],
                            )),
                        ))
                        .await;
                    break;
                }
            };
            self.in_flight.fetch_add(1, Ordering::AcqRel);
            let mut deferred = None;
            let response = self.dispatch(request, caller, &mut deferred).await;
            let written = writer.write_frame(&response).await.is_ok();
            // Decision D, and the whole of it: **the attach response goes out before any frame
            // of the replay is enqueued.** The client can only record the id by parsing this
            // response, and control and stream are separate sockets with nothing ordering
            // them — so a replay enqueued first can beat the answer that explains it, and a
            // client applying proto's own rule classifies those frames as an id never
            // assigned and drops the connection. Nothing orders two sockets, so the side that
            // *can* provide the ordering has to.
            if let Some(pending) = deferred {
                if written {
                    pending.start(&self.status).await;
                } else {
                    // The client will never learn the id, so nothing may be routed to it. A
                    // sink left open here can never be acked and would stall its session.
                    let request = pending.request().clone();
                    pending.abandon(&self.streams);
                    // And the receipt goes with it. An attach's whole effect is the id in its
                    // answer, so a receipt kept here would replay that id to a retry after the
                    // sink behind it has been given up — an attach into nothing, reported as a
                    // success. Narrow to the attach on purpose: every other mutation has a
                    // durable effect that survives the failed write, and forgetting those
                    // would have a retry do the work twice.
                    lock(&self.receipts).forget(&caller.fingerprint, &request);
                }
            }
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            if !written {
                break;
            }
        }
    }

    /// Answer one verb, replaying a receipt when this is a retry.
    async fn dispatch(
        self: &Arc<Self>,
        request: RequestEnvelope,
        caller: &Caller,
        deferred: &mut Option<Deferred>,
    ) -> ResponseEnvelope {
        let incoming = request.request_id.clone();
        let is_mutation = request.payload.is_mutation();
        tracing::debug!(
            verb = request.payload.verb(),
            client = %caller.client_id,
            // What §3.2 bought: a verb from an agent is attributable to the session it was
            // typed in, proved by the process tree rather than by an echoed token. `None` is
            // the window or a person at a shell, and is not a problem.
            from_session = ?caller.session.as_ref().map(|from| from.incarnation.as_str()),
            "serving a verb"
        );

        // The answer to "did it land?". The caller cannot tell a lost reply from work that
        // never happened, and those need opposite responses — so a retry names the original
        // and gets the original answer rather than doing the work twice.
        if is_mutation
            && let Some(original) = request.retry_request.clone()
            && let Some(stored) = lock(&self.receipts).get(&caller.fingerprint, &original)
        {
            return ResponseEnvelope::new(incoming, stored).with_receipt(MutationReceipt {
                request_id: original,
                replayed: true,
            });
        }

        let payload = self.run(request.payload, caller, &incoming, deferred).await;
        if is_mutation && payload.error().is_none() {
            lock(&self.receipts).put(&caller.fingerprint, &incoming, payload.clone());
        }
        let response = ResponseEnvelope::new(incoming.clone(), payload);
        if is_mutation {
            response.with_receipt(MutationReceipt {
                request_id: incoming,
                replayed: false,
            })
        } else {
            response
        }
    }

    /// Run one verb against the registries.
    async fn run(
        self: &Arc<Self>,
        payload: RequestPayload,
        caller: &Caller,
        request_id: &RequestId,
        deferred: &mut Option<Deferred>,
    ) -> ResponsePayload {
        let sessions = Arc::clone(&self.sessions);
        match payload {
            RequestPayload::SessionCreate(request) => {
                // Spawning opens a pty and starts three threads. On a runtime worker that
                // would stall every other session sharing it.
                let client = caller.client_id.clone();
                blocking(move || match sessions.create(&request) {
                    Ok(created) => {
                        // **One of the daemon's two correlation lines**, and `info` rather
                        // than `debug` on purpose: a join key only a reader who already knew
                        // to raise the level would have is not a join key. Everything the
                        // daemon says afterwards names a handle or a stream id and nothing
                        // else; this is what binds those to a pane. All three come off the
                        // *answer*, never the request — see `log_file` for why the request is
                        // not logged at any level.
                        tracing::info!(
                            client = %client,
                            pane_key = %created.pane_key,
                            handle = %created.handle,
                            incarnation = %created.incarnation,
                            "opened a session"
                        );
                        ResponsePayload::SessionCreate(created)
                    }
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                })
                .await
            }
            RequestPayload::SessionList(SessionList {}) => ResponsePayload::SessionList {
                sessions: sessions.list(),
            },
            RequestPayload::SessionClose(request) => {
                blocking(move || match sessions.close(&request.handle) {
                    Ok(()) => ResponsePayload::SessionClose,
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                })
                .await
            }
            RequestPayload::TerminalRead(request) => match sessions.get(&request.handle) {
                Ok(session) => ResponsePayload::TerminalRead(session.read(
                    request.mode,
                    request.cursor,
                    request.limit,
                )),
                Err(err) => ResponsePayload::Error(err.into_envelope()),
            },
            RequestPayload::TerminalSend(request) => match sessions.get(&request.handle) {
                Ok(session) => match session.send(&request) {
                    Ok(()) => ResponsePayload::TerminalSend,
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                },
                Err(err) => ResponsePayload::Error(err.into_envelope()),
            },
            RequestPayload::TerminalResize(request) => match sessions.get(&request.handle) {
                Ok(session) => match session.resize(request.cols, request.rows) {
                    Ok(()) => ResponsePayload::TerminalResize,
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                },
                Err(err) => ResponsePayload::Error(err.into_envelope()),
            },
            RequestPayload::TerminalWait(request) => match sessions.get(&request.handle) {
                Ok(session) => {
                    let timeout = request.timeout_ms.map(Duration::from_millis);
                    let handle = request.handle.clone();
                    // A wait blocks by definition. It is also the one verb a client may hold
                    // open for minutes, which is why it must not occupy a runtime worker.
                    blocking(move || {
                        ResponsePayload::TerminalWait(nysia_proto::TerminalWaitResult {
                            handle,
                            outcome: session.wait(request.wait_for, timeout),
                        })
                    })
                    .await
                }
                Err(err) => ResponsePayload::Error(err.into_envelope()),
            },
            RequestPayload::StreamAttach(request) => {
                self.attach(&request.handle, caller, request_id, deferred)
            }
            RequestPayload::StreamDetach(request) => {
                // Detaching a stream that has already gone is not an error. Proto is explicit
                // that an id naming nothing live is the routine detach race rather than a
                // fault, and answering "no such stream" would have clients retrying a
                // teardown that has already happened.
                self.streams.detach(&caller.client_id, request.stream_id);
                ResponsePayload::StreamDetach
            }
            // The five v0.2 agent-status verbs. Still no `_` arm, for the reason the
            // placeholder this replaces was written without one: a verb added later must fail
            // the build here rather than be served silently.
            RequestPayload::AgentHook(request) => {
                let pane = match self.ingest_pane(caller, request.pane_hint.as_ref()) {
                    Ok(pane) => pane,
                    Err(refusal) => return ResponsePayload::Error(refusal),
                };
                let status = Arc::clone(&self.status);
                // The write, and the broadcast with it, on the blocking pool: `rusqlite` is a
                // blocking C library and this runs on the agent's critical path once per tool
                // call it makes. The broadcast goes to *other* clients' stream connections, so
                // nothing orders it against this answer.
                blocking(move || match status.record(&pane, &request.event) {
                    Ok(change) => {
                        if let Some(change) = change {
                            status.broadcast(&change);
                        }
                        ResponsePayload::AgentHook
                    }
                    // Reported rather than swallowed: the hook spools what the daemon could
                    // not keep, and a daemon answering "received" would have it throw the
                    // status away instead (§2.3).
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                })
                .await
            }
            RequestPayload::AgentStatusGet(request) => {
                let status = Arc::clone(&self.status);
                blocking(move || match status.status(&request.pane) {
                    Ok(status) => ResponsePayload::AgentStatusGet { status },
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                })
                .await
            }
            RequestPayload::AgentStatusList(AgentStatusList {}) => {
                let status = Arc::clone(&self.status);
                blocking(move || match status.statuses() {
                    Ok(statuses) => ResponsePayload::AgentStatusList { statuses },
                    Err(err) => ResponsePayload::Error(err.into_envelope()),
                })
                .await
            }
            RequestPayload::AgentStatusSubscribe(AgentStatusSubscribe {}) => {
                self.subscribe(caller, request_id, deferred)
            }
            RequestPayload::AgentStatusUnsubscribe(request) => {
                // Releasing an id that has already gone is not an error, for the reason
                // `StreamDetach` gives: it is the routine teardown race, and answering "no
                // such subscription" would have clients retrying something already done.
                if let Some(connection) = self.streams.current(&caller.client_id) {
                    self.status.unsubscribe(connection, request.stream_id);
                }
                self.streams.detach(&caller.client_id, request.stream_id);
                ResponsePayload::AgentStatusUnsubscribe
            }
            // Scaffolding, and **v0.3 wave C1 replaces it** with the three real handlers.
            // The verbs are on the wire from wave A so that the store, the window and this
            // daemon are built against one definition of them; a daemon that is asked one
            // before C1 lands still has to answer something, and `unsupported` is the code
            // for a verb this daemon does not serve. One arm rather than a wildcard, on
            // purpose: the match stays exhaustive, so the next verb added to the wire fails
            // the build here rather than being answered silently by a catch-all.
            RequestPayload::ProjectRegister(_)
            | RequestPayload::ProjectList(_)
            | RequestPayload::ProjectForget(_) => {
                ResponsePayload::Error(nysia_proto::project::unsupported_envelope())
            }
        }
    }

    /// Which pane a hook belongs to, proved rather than accepted (§3.2).
    ///
    /// The ancestry walk is the proof and `pane_hint` is only ever compared against it. A
    /// caller descending from no session this daemon spawned is refused: the alternative is a
    /// daemon that lets any process on the machine write status into any pane, which is what
    /// `crates/nysia/tests/agent_status.rs` runs its hook inside the session to avoid having
    /// to rely on.
    ///
    /// The two refusals say different things on purpose. "The kernel would not name you" is a
    /// platform limitation — a Unix carrying no pid in its peer credentials — and "you descend
    /// from nothing I own" is a caller in the wrong place. Told apart, a red CI leg says which
    /// of the two happened; collapsed into one message it says neither.
    fn ingest_pane(
        &self,
        caller: &Caller,
        hint: Option<&PaneKey>,
    ) -> Result<PaneKey, ErrorEnvelope> {
        let Some(from) = caller.session.as_ref() else {
            return Err(envelope(
                ErrorCode::InvalidRequest,
                if caller.pid.is_some() {
                    "a hook may only report status for the session it runs in, and this caller \
                     descends from no session this daemon owns"
                } else {
                    "this platform would not name the calling process, so the session a hook \
                     runs in cannot be proved"
                },
                "run `nysia hook` from inside a Nysia session — Claude's installed hook \
                 entries already do",
                &[
                    "§3.2: pane identity comes from peer credentials and the pty process \
                     tree, and NYSIA_PANE_KEY is a hint that is never the proof",
                ],
            ));
        };
        let session = self
            .sessions
            .get(&from.handle)
            .map_err(IntoEnvelope::into_envelope)?;
        let pane = session.pane_key().clone();
        if let Some(hinted) = hint.filter(|hinted| *hinted != &pane) {
            // Logged and ignored, never honoured. A disagreement is worth seeing — a stale
            // `NYSIA_PANE_KEY` inherited across a re-spawn looks exactly like this — but the
            // hint is a hint, so the proof simply wins.
            tracing::debug!(
                proved = pane.as_str(),
                hinted = hinted.as_str(),
                "a hook's pane hint disagrees with the session it came from; using the session"
            );
        }
        Ok(pane)
    }

    /// Reserve a stream id for status changes, leaving the subscription to the caller.
    ///
    /// The same shape as [`Self::attach`] and for the same reason: a client can only record
    /// the id by parsing this answer, so nothing may be written to that id until the answer
    /// has gone out. A change frame overtaking it names an id the client never assigned, which
    /// proto's routing rule makes a desync that costs the whole connection.
    fn subscribe(
        &self,
        caller: &Caller,
        request_id: &RequestId,
        deferred: &mut Option<Deferred>,
    ) -> ResponsePayload {
        let Some(attached) = self.streams.attach(&caller.client_id) else {
            return ResponsePayload::Error(
                envelope(
                    ErrorCode::InvalidRequest,
                    "this client has no stream connection to route status changes to",
                    "open a second connection with `role: \"stream\"` and the same `clientId`, \
                     then subscribe",
                    &[
                        "a subscription rides the stream connection terminal output already \
                         uses; there is no second mechanism",
                    ],
                )
                .retryable(true),
            );
        };
        let stream_id = attached.sink.stream_id();
        *deferred = Some(Deferred::Subscription(PendingSubscription {
            connection: attached.connection,
            sink: attached.sink,
            stream_id,
            request: request_id.clone(),
        }));
        ResponsePayload::AgentStatusSubscribe(AgentStatusSubscribed { stream_id })
    }

    /// Reserve a stream id for a session's output, leaving the replay to the caller.
    ///
    /// The id is assigned and registered here; **nothing is written to the stream connection
    /// yet**. The replay comes back in `deferred` so that [`Self::serve_control`] can start it
    /// only once the answer carrying the id has gone out — see the comment there for why the
    /// order is not negotiable.
    fn attach(
        &self,
        handle: &nysia_proto::SessionHandle,
        caller: &Caller,
        request_id: &RequestId,
        deferred: &mut Option<Deferred>,
    ) -> ResponsePayload {
        let session = match self.sessions.get(handle) {
            Ok(session) => session,
            Err(err) => return ResponsePayload::Error(err.into_envelope()),
        };
        let Some(attached) = self.streams.attach(&caller.client_id) else {
            return ResponsePayload::Error(
                envelope(
                    ErrorCode::InvalidRequest,
                    "this client has no stream connection to route output to",
                    "open a second connection with `role: \"stream\"` and the same `clientId`, \
                     then attach",
                    &[
                        "control and stream are separate connections so a terminal firehose \
                         cannot delay a session close",
                    ],
                )
                .retryable(true),
            );
        };
        let stream_id = attached.sink.stream_id();
        // The second correlation line, and the one that makes a stream id mean something. An
        // id is scoped to **one stream connection** and the counter restarts with it, so two
        // panes across a reconnect can carry the same number and time order is all a reader
        // would otherwise have. `client` is what tells those apart, and `handle` is the key
        // that survives everything — the window records the same handle against the same id,
        // so the two files join on it without either needing the other's vocabulary.
        tracing::info!(
            client = %caller.client_id,
            handle = %handle,
            stream = %stream_id,
            "routed a session onto this client's stream connection"
        );
        *deferred = Some(Deferred::Replay(PendingReplay {
            session,
            connection: attached.connection,
            sink: attached.sink,
            stream_id,
            request: request_id.clone(),
        }));
        ResponsePayload::StreamAttach(StreamAttached {
            handle: handle.clone(),
            stream_id,
        })
    }

    /// Carry binary output one way and credit acks the other.
    ///
    /// Takes the connection already bound rather than binding here, because the bind has to
    /// happen before the hello is answered — see [`Self::serve_io`] for the race that closes.
    async fn serve_stream<R, W>(
        self: &Arc<Self>,
        reader: ControlReader<R>,
        writer: ControlWriter<W>,
        client_id: &ClientId,
        bound: BoundStream,
    ) where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let BoundStream {
            key,
            mut outbox,
            superseded,
        } = bound;
        // The two halves run concurrently on purpose: a client that has stopped acking must
        // not also stop the daemon from *reading* its acks, which is the one thing that would
        // turn a temporary stall into a permanent one.
        let mut writer = writer.into_inner();
        let pumping = tokio::spawn(async move {
            while let Some(frame) = outbox.recv().await {
                if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
        });
        // The second wake path is not a nicety. A webview reload leaves this connection
        // superseded with its reader parked, and a parked named-pipe read cannot be cancelled
        // on Windows — so without this the task, its slot in the client count and its socket
        // are held until the client happens to send something, and the client's own reader
        // waits for an EOF that only arrives when this side lets go.
        tokio::select! {
            () = self.read_credit(reader, key) => {}
            () = superseded.notified() => {
                tracing::debug!(
                    client = %client_id,
                    connection = key.get(),
                    "letting go of a stream connection a newer one replaced"
                );
            }
        }
        pumping.abort();
        self.streams.unbind(key);
        // Its subscriptions too. `unbind` closes the sinks, so a broadcast would notice on
        // its next frame — but "notice eventually" is a list that grows for the life of a
        // daemon every time a window reloads.
        self.status.forget_connection(key);
    }

    /// Read credit acks until the client goes away.
    ///
    /// Scoped to `connection`, because a stream id means nothing off its own connection: ids
    /// are counted per connection, so two of them — two clients, or one client across a
    /// reload — both hold [`StreamId::FIRST`], and an unscoped lookup would credit whichever
    /// of them a shared map happened to hold.
    async fn read_credit<R>(&self, reader: ControlReader<R>, connection: ConnectionKey)
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        use tokio::io::AsyncReadExt;

        // Whatever the handshake read past the `hello` line belongs to the frame stream. One
        // `read` can easily deliver both, and dropping the remainder would lose the client's
        // first frame for no reason it could ever diagnose.
        let (leftover, mut reader) = reader.into_parts();
        let mut decoder = FrameDecoder::new();
        decoder.push(&leftover);
        let mut buffer = [0u8; 4096];
        loop {
            let read = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            decoder.push(&buffer[..read]);
            loop {
                match decoder.next_frame() {
                    Ok(Some(frame)) if frame.kind == FrameKind::Credit => {
                        match serde_json::from_slice::<CreditFrame>(&frame.payload) {
                            Ok(credit) => {
                                // Proto's discard-versus-drop rule, applied rather than
                                // described. The registry makes the call — it holds both the
                                // table and the watermark — and this honours both halves of
                                // the answer. Discarding *everything* unroutable would be the
                                // tolerated deviation it was: harmless by the client's good
                                // behaviour rather than by the protocol, and one direction
                                // quietly opting out of a shared rule is how two
                                // implementations start to disagree about what is on the wire.
                                match self.streams.apply_credit(connection, frame.stream, &credit) {
                                    CreditOutcome::Routed | CreditOutcome::DiscardFrame => {}
                                    // The connection, and every session riding it. The
                                    // registry has already said which id and against which
                                    // watermark.
                                    CreditOutcome::DropConnection => return,
                                }
                            }
                            Err(err) => {
                                tracing::debug!(%err, "ignored an unreadable credit frame");
                            }
                        }
                    }
                    Ok(Some(frame)) => {
                        tracing::debug!(
                            kind = frame.kind.as_str(),
                            "ignored a frame a client is not supposed to send"
                        );
                    }
                    Ok(None) => break,
                    Err(err) => {
                        // A frame stream that has lost sync cannot be resynchronised — there
                        // is no delimiter to scan forward to — so the connection is finished.
                        tracing::debug!(%err, "dropping a stream connection that lost framing");
                        return;
                    }
                }
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.lease.remove();
    }
}

/// Work that must not start until the answer naming its stream id has been written.
///
/// Both arms mint an id on the control connection and then write to the *stream* connection,
/// and nothing orders those two sockets. A client can only record an id by parsing the
/// answer, and proto's routing rule makes a frame naming an id at or beyond the next one to
/// assign a desync that costs the whole connection — so the side that can supply the ordering
/// has to, and this is that ordering made a type rather than a comment.
#[derive(Debug)]
enum Deferred {
    /// A session attach owes an opening grant and a replay ring.
    Replay(PendingReplay),
    /// A status subscription owes nothing up front, and must simply not be in the broadcast
    /// list while a change could still overtake its id.
    Subscription(PendingSubscription),
}

impl Deferred {
    /// The request whose answer names the id, so a failed write can forget its receipt.
    fn request(&self) -> &RequestId {
        match self {
            Self::Replay(replay) => &replay.request,
            Self::Subscription(subscription) => &subscription.request,
        }
    }

    /// Begin, now that the client knows the id.
    async fn start(self, status: &AgentStatusService) {
        match self {
            Self::Replay(replay) => replay.start().await,
            Self::Subscription(subscription) => {
                status.subscribe(subscription.connection, subscription.sink);
            }
        }
    }

    /// Give the id back, for an answer that never reached the client.
    fn abandon(self, streams: &StreamRegistry) {
        match self {
            Self::Replay(replay) => replay.abandon(streams),
            Self::Subscription(subscription) => {
                streams.detach_on(subscription.connection, subscription.stream_id);
            }
        }
    }
}

/// A subscription that holds an id the client has not been told about yet.
///
/// It is not in the broadcast list while it is here, which is the whole point: a hook
/// arriving on another connection in this window would otherwise put a change frame ahead of
/// the answer that explains its id.
#[derive(Debug)]
struct PendingSubscription {
    /// The connection the id belongs to, so a rollback undoes the one that was attached even
    /// if a newer connection has taken the client id over since.
    connection: ConnectionKey,
    /// Where the changes will go.
    sink: Arc<StreamSink>,
    /// The id that was reserved.
    stream_id: StreamId,
    /// The request whose answer names it.
    request: RequestId,
}

/// An attach that holds an id but has not written a byte to the stream connection yet.
///
/// It exists so that the two halves of an attach can be *ordered*: the id is reserved while
/// the verb runs, the answer naming it goes out, and only then does the opening grant and the
/// replay ring follow. Enqueued the other way round they race the answer across two sockets
/// that nothing orders, and a client applying proto's discard-versus-drop rule reads the
/// early frames as an id that was never assigned and drops the connection — on the re-attach
/// path that is D-1's flagship promise.
#[derive(Debug)]
struct PendingReplay {
    /// The session whose scrollback is owed to the client.
    session: Arc<OwnedSession>,
    /// The connection the id belongs to, so a rollback undoes the one that was attached even
    /// if a newer connection has taken the client id over since.
    connection: ConnectionKey,
    /// Where the replay goes.
    sink: Arc<StreamSink>,
    /// The id that was reserved.
    stream_id: StreamId,
    /// The request whose answer names that id, so a failed write can forget its receipt.
    request: RequestId,
}

impl PendingReplay {
    /// Send the opening grant and the replay ring, now that the client knows the id.
    ///
    /// On a blocking thread: the replay takes the session's terminal-state lock and can write
    /// the whole scrollback, and a runtime worker held for that stalls every other session
    /// sharing it.
    async fn start(self) {
        let Self { session, sink, .. } = self;
        if let Err(err) = tokio::task::spawn_blocking(move || session.attach(&sink)).await {
            tracing::warn!(%err, "a replay did not run; the pane will fill from live output");
        }
    }

    /// Give the id back, for an attach whose answer never reached the client.
    ///
    /// A client that never learned the id can never ack it, so a sink left open here would
    /// spend its allowance once and stall the session it feeds for as long as the daemon runs.
    fn abandon(self, streams: &StreamRegistry) {
        streams.detach_on(self.connection, self.stream_id);
    }
}

/// Who is asking, for logging and for keying mutation receipts.
///
/// The fingerprint is the kernel's word plus the client's own id. The kernel half is what
/// makes it unforgeable — a peer cannot choose the uid its socket reports — and the client
/// half is what lets one logical client's retry find its own receipt across a reconnect,
/// which a pid alone would not survive.
#[derive(Debug, Clone)]
struct Caller {
    client_id: ClientId,
    fingerprint: String,
    /// The pid the kernel reported, when it reported one.
    ///
    /// Kept only to tell two refusals apart: a platform that will not name the peer at all
    /// and a peer that descends from nothing this daemon owns are different problems, and a
    /// hook refusal that could not say which would send whoever reads it to the wrong file.
    pid: Option<u32>,
    /// The session this caller descends from, when it descends from one (§3.2).
    ///
    /// Identification, never authorisation. It is `None` for the window and for a person at a
    /// shell, and both must work - which is exactly why the account check is the only thing
    /// that decides whether a connection is served at all.
    session: Option<CallerSession>,
}

impl Caller {
    /// Identify a caller from what the kernel said and what the process tree shows.
    ///
    /// The ancestry walk runs once, at the handshake, rather than per request: a caller's
    /// parentage does not change while its connection is open, and walking the process table
    /// on every verb would put a snapshot of every process on the machine in the path of
    /// `terminal read`.
    fn new(
        peer: Option<PeerCredentials>,
        client_id: &ClientId,
        sessions: &SessionRegistry,
    ) -> Self {
        let user = peer.as_ref().map_or("unknown", |peer| peer.user.as_str());
        let session = peer
            .as_ref()
            .and_then(|peer| peer.pid)
            .and_then(|pid| crate::rpc::peer::ancestry(pid, &sessions.leaders()));
        if let Some(found) = &session {
            tracing::debug!(
                %client_id,
                handle = found.handle.as_str(),
                depth = found.depth,
                "the caller descends from a session this daemon owns"
            );
        }
        Self {
            client_id: client_id.clone(),
            fingerprint: format!("{user}/{client_id}"),
            pid: peer.as_ref().and_then(|peer| peer.pid),
            session,
        }
    }
}

/// Stored answers to mutations, so a retry does not do the work twice.
#[derive(Debug, Default)]
struct Receipts {
    stored: HashMap<(String, String), ResponsePayload>,
    order: VecDeque<(String, String)>,
}

impl Receipts {
    fn get(&self, fingerprint: &str, request: &RequestId) -> Option<ResponsePayload> {
        self.stored
            .get(&(fingerprint.to_owned(), request.to_string()))
            .cloned()
    }

    /// Forget one receipt, for work whose answer never reached the caller.
    ///
    /// Only for a mutation whose durable effect is the answer itself. A `session create` that
    /// was not answered still spawned a shell, and a retry that re-ran it would spawn a
    /// second — which is the whole reason receipts exist. An *attach* is the other shape: its
    /// effect is an id the client never learned, so a receipt for one is a promise to replay
    /// an answer routing output to a sink that has already been given up.
    fn forget(&mut self, fingerprint: &str, request: &RequestId) {
        let key = (fingerprint.to_owned(), request.to_string());
        if self.stored.remove(&key).is_some() {
            self.order.retain(|stored| stored != &key);
        }
    }

    fn put(&mut self, fingerprint: &str, request: &RequestId, payload: ResponsePayload) {
        let key = (fingerprint.to_owned(), request.to_string());
        if self.stored.insert(key.clone(), payload).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > RECEIPT_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.stored.remove(&oldest);
            }
        }
    }
}

/// Drain the disk spool into the store, before the daemon serves anybody.
///
/// A drain that fails is logged and not fatal. The rows are the ones a hook could not hand
/// over while this daemon was away, so losing them costs the dots they carried — where
/// refusing to start would cost every session the daemon was about to own.
/// Clears the trim-in-progress flag however the trim ends, a panic included.
///
/// See [`Daemon::spawn_log_trim`]: leaving the flag set stops trimming for the daemon's whole
/// life, so the clearing belongs somewhere unwinding cannot skip.
struct TrimInProgress(Arc<Daemon>);

impl Drop for TrimInProgress {
    fn drop(&mut self) {
        self.0.trimming_log.store(false, Ordering::Release);
    }
}

fn drain_spool(runtime_dir: &std::path::Path, status: &AgentStatusService) {
    let drained = match crate::rpc::spool::drain(runtime_dir) {
        Ok(drained) => drained,
        Err(err) => {
            tracing::warn!(%err, "could not drain the agent-status spool; carrying on without it");
            return;
        }
    };
    if drained.rows.is_empty() && drained.unreadable == 0 {
        return;
    }
    let summary = status.restore(&drained.rows);
    tracing::info!(
        applied = summary.applied,
        superseded = summary.superseded,
        failed = summary.failed,
        unreadable = drained.unreadable,
        "drained the agent-status spool"
    );
}

/// Run `work` on the blocking pool, answering with an internal error if the pool refuses.
async fn blocking<F>(work: F) -> ResponsePayload
where
    F: FnOnce() -> ResponsePayload + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(payload) => payload,
        Err(err) => ResponsePayload::Error(internal(&err.to_string())),
    }
}

/// The envelope for a failure the caller did nothing to cause.
fn internal(detail: &str) -> ErrorEnvelope {
    envelope(
        ErrorCode::Internal,
        format!("the daemon failed while serving this verb: {detail}"),
        "retry the request",
        &["`nysia session list` will show whether the daemon is still serving"],
    )
    .retryable(true)
}

/// Take a lock, treating poisoning as recoverable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nysia_proto::{CreditAck, Frame, SessionCreate, SessionKind, StreamAttach};

    use super::*;
    use crate::rpc::testing::TestShell;

    /// A socket that has gone away: every write fails the way one into a closed pipe does.
    ///
    /// Written out rather than staged by dropping a pipe's far end, because the branches it
    /// is here for are the ones that run when an answer never reached the client — and a test
    /// for those should not also rest on when a buffered duplex decides to report the far end
    /// has gone.
    struct WriteFails;

    impl tokio::io::AsyncWrite for WriteFails {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// One credit ack, framed the way a client puts it on the wire.
    fn credit_ack(stream: StreamId, bytes: u32) -> Vec<u8> {
        let payload = serde_json::to_vec(&CreditFrame::Ack(CreditAck { bytes }))
            .expect("a credit ack serialises");
        nysia_proto::encode(&Frame::new(FrameKind::Credit, stream, payload))
            .expect("a credit ack encodes")
    }

    #[test]
    fn receipts_replay_the_first_answer_and_forget_the_oldest() {
        let mut receipts = Receipts::default();
        let first = RequestId::generate();
        receipts.put("me", &first, ResponsePayload::SessionClose);
        assert!(receipts.get("me", &first).is_some());
        // A different caller must not read somebody else's receipt: the fingerprint is half
        // the key precisely so one client cannot replay another's mutation.
        assert!(receipts.get("someone-else", &first).is_none());

        for _ in 0..RECEIPT_CAPACITY {
            receipts.put("me", &RequestId::generate(), ResponsePayload::SessionClose);
        }
        assert!(
            receipts.get("me", &first).is_none(),
            "the oldest receipt should have been forgotten"
        );
    }

    #[tokio::test]
    async fn binding_writes_a_lease_that_describes_the_daemon_and_removing_it_is_the_clean_path() {
        let endpoint = crate::rpc::endpoint::scratch("lease");
        let (daemon, listener) = Daemon::bind(DaemonConfig::new(endpoint.clone())).expect("binds");
        let lease = PidRecordFile::at(endpoint.pid_record_path());
        let record = lease.read().expect("reads").expect("is there");
        assert!(record.describes(daemon.identity()));

        daemon.shutdown();
        assert!(
            lease.read().expect("reads").is_none(),
            "a clean shutdown takes the lease with it"
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_daemon_trims_the_log_beside_its_own_endpoint() {
        // The wiring, not the rotation — `log_file`'s own tests cover what a trim does. What
        // this holds is that a bound daemon knows which file it is filling, so the tick in
        // `serve` is aimed at something.
        let endpoint = crate::rpc::endpoint::scratch("logtrim");
        let (daemon, listener) = Daemon::bind(DaemonConfig::new(endpoint.clone())).expect("binds");

        let log = endpoint.log_path();
        assert_eq!(
            daemon.trim_log().expect("a trim"),
            Trimmed::Untouched,
            "a daemon with no log to trim reported doing something"
        );

        std::fs::File::create(&log)
            .expect("a log")
            .set_len(log_file::MAX_LOG_BYTES + 1)
            .expect("a size over the cap");
        assert!(matches!(
            daemon.trim_log().expect("a trim"),
            Trimmed::Rotated { .. }
        ));
        assert_eq!(
            std::fs::metadata(&log).expect("the live log").len(),
            0,
            "the daemon rotated a file that was not its own log"
        );
        assert!(log_file::rotation_path(&log, 1).exists());

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_panicking_trim_does_not_stop_the_daemon_trimming_for_ever() {
        // The failure the guard exists for, and the reason it is a guard rather than a store
        // at the end of the closure: leaving the flag set means `spawn_log_trim` returns early
        // every tick from then on, so the log grows unbounded again — silently, and for the
        // daemon's whole life, which under D-1 is days. That is a worse outcome than the
        // double rotation the flag prevents.
        let endpoint = crate::rpc::endpoint::scratch("trimpanic");
        let (daemon, listener) = Daemon::bind(DaemonConfig::new(endpoint.clone())).expect("binds");
        daemon.trimming_log.store(true, Ordering::Release);

        // The hook is silenced for the duration: this panic is the input, not a failure, and
        // a backtrace in the middle of a passing run reads like one.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = TrimInProgress(Arc::clone(&daemon));
            panic!("a trim went wrong in a way `io::Error` does not cover");
        }));
        std::panic::set_hook(hook);

        assert!(
            unwound.is_err(),
            "the panic was supposed to escape the closure"
        );
        assert!(
            !daemon.trimming_log.load(Ordering::Acquire),
            "the flag survived a panicking trim, so this daemon would never trim again"
        );

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_daemon_that_has_served_nobody_does_not_retire_out_from_under_its_spawner() {
        let endpoint = crate::rpc::endpoint::scratch("retire");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: Some(Duration::ZERO),
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");

        assert!(
            !daemon.should_retire(),
            "a freshly spawned daemon must wait for the client that is about to dial in"
        );
        daemon.served_anyone.store(true, Ordering::Release);
        assert!(
            !daemon.should_retire(),
            "the first tick only arms the timer"
        );
        assert!(
            daemon.should_retire(),
            "and the second finds the grace spent"
        );

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_daemon_holding_a_client_or_a_request_never_retires() {
        // The whole of D-1 in one condition. An idle timer that could fire while somebody is
        // connected would make "the daemon outlives the app" a promise it does not keep.
        let endpoint = crate::rpc::endpoint::scratch("busy");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: Some(Duration::ZERO),
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        daemon.served_anyone.store(true, Ordering::Release);

        daemon.clients.store(1, Ordering::Release);
        assert!(!daemon.should_retire());
        assert!(!daemon.should_retire());

        daemon.clients.store(0, Ordering::Release);
        daemon.in_flight.store(1, Ordering::Release);
        assert!(!daemon.should_retire());
        assert!(!daemon.should_retire());

        daemon.in_flight.store(0, Ordering::Release);
        assert!(!daemon.should_retire(), "the timer had to be re-armed");
        assert!(daemon.should_retire());

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_daemon_told_never_to_retire_does_not() {
        let endpoint = crate::rpc::endpoint::scratch("forever");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        daemon.served_anyone.store(true, Ordering::Release);
        for _ in 0..4 {
            assert!(!daemon.should_retire());
        }
        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn a_receipt_can_be_forgotten_when_its_answer_never_left() {
        // Only for work whose whole effect is the answer. An attach that was never written
        // leaves an id the client cannot know and a sink that has been given up, so replaying
        // that answer to a retry routes output into nothing and calls it success. Every other
        // mutation has a durable effect that survives the failed write — a `session create`
        // spawned a shell either way — and forgetting one of those would have the retry do
        // the work twice, which is the thing receipts exist to prevent.
        let mut receipts = Receipts::default();
        let attach = RequestId::generate();
        let created = RequestId::generate();
        receipts.put("me", &attach, ResponsePayload::StreamDetach);
        receipts.put("me", &created, ResponsePayload::SessionClose);

        receipts.forget("me", &attach);
        assert!(receipts.get("me", &attach).is_none());
        assert!(
            receipts.get("me", &created).is_some(),
            "forgetting one receipt must not disturb another"
        );
        // Idempotent: a write can fail once, and a second forget is not an error.
        receipts.forget("me", &attach);
        assert_eq!(receipts.order.len(), receipts.stored.len());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stream_connection_is_attachable_before_its_hello_is_answered() {
        // The reload race, from the side that can close it. A client may attach the instant
        // its connect returns, and its connect returns when the hello answer arrives — so if
        // the bind came after that answer there is a window where the client believes it has
        // a stream connection and the daemon still routes its id to the one being replaced.
        // The attach is not refused, so nothing tells the client to retry; it lands on a sink
        // the supersede then closes, and the pane is silent while the window says ready.
        //
        // A pipe narrower than one line is what makes this an assertion rather than a wager:
        // the daemon is held *inside* the write of its own answer, which is exactly the state
        // a racing client observes, and it is held there for as long as this test looks.
        let endpoint = crate::rpc::endpoint::scratch("prebind");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");
        let mine = PeerCredentials {
            pid: Some(std::process::id()),
            user: "me".to_owned(),
            daemon_user: "me".to_owned(),
        };

        let (theirs, ours) = tokio::io::duplex(8);
        let (ours_reader, ours_writer) = tokio::io::split(ours);
        let (_their_reader, their_writer) = tokio::io::split(theirs);
        let serving = tokio::spawn({
            let daemon = Arc::clone(&daemon);
            async move {
                daemon
                    .serve_io(
                        ControlReader::new(ours_reader),
                        ControlWriter::new(ours_writer),
                        Some(mine),
                    )
                    .await;
            }
        });

        // Say hello in the stream role and then read nothing at all, which is the worst a
        // real client's scheduler can do to it.
        ControlWriter::new(their_writer)
            .write_frame(&HelloRequest::new(
                PROTOCOL_VERSION,
                ClientRole::Stream,
                client_id.clone(),
            ))
            .await
            .expect("the hello goes out");

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && daemon.streams().bound() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            daemon.streams().bound(),
            1,
            "a stream connection must be bound before its hello is answered; bound after, a \
             client that attaches the moment its connect returns lands on the connection this \
             one supersedes"
        );
        assert!(
            daemon.streams().attach(&client_id).is_some(),
            "which is to say: an attach arriving while the answer is still being written must \
             find this connection, not the one it replaced"
        );

        serving.abort();
        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_refused_hello_binds_nothing_and_leaves_the_stream_it_named_alone() {
        // The other half of "bound before the answer, and after the vet". A bind *supersedes*
        // whatever held that client id — closing its sinks, which can never be acked again,
        // and waking its reader — so binding before the account check hands an unauthorised
        // peer a way to take somebody else's live stream down by naming their id in a hello
        // that is about to be refused. The refusal still goes out; the damage is already done.
        //
        // Driven through the serve loop rather than against `reject_reason`, because what is
        // being asserted is not which answer the vet picks but that nothing reaches the
        // registry before it picks one.
        let endpoint = crate::rpc::endpoint::scratch("refused");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");

        // The client that is already here, with a session's output routed to it.
        let mine = daemon.streams().bind(&client_id);
        let live = daemon.streams().attach(&client_id).expect("attaches").sink;
        let bound_before = daemon.streams().bound();

        // A peer from another account, in the stream role, naming that client's id.
        let theirs = PeerCredentials {
            pid: Some(std::process::id()),
            user: "them".to_owned(),
            daemon_user: "me".to_owned(),
        };
        let (peer, ours) = tokio::io::duplex(4096);
        let (ours_reader, ours_writer) = tokio::io::split(ours);
        let (_peer_reader, peer_writer) = tokio::io::split(peer);
        ControlWriter::new(peer_writer)
            .write_frame(&HelloRequest::new(
                PROTOCOL_VERSION,
                ClientRole::Stream,
                client_id.clone(),
            ))
            .await
            .expect("the hello goes out");

        // Returns as soon as the refusal is written; there is nothing to serve after one.
        daemon
            .serve_io(
                ControlReader::new(ours_reader),
                ControlWriter::new(ours_writer),
                Some(theirs),
            )
            .await;

        assert_eq!(
            daemon.streams().bound(),
            bound_before,
            "a hello that is refused must bind nothing: the bind is what supersedes, so a \
             peer that failed the account check reaching the registry at all is the failure"
        );
        assert!(
            !live.is_closed(),
            "the refused peer named somebody else's live stream, and a supersede closes \
             sinks that can never be acked again — the session behind this one would stall \
             for as long as the daemon runs"
        );
        assert_eq!(
            daemon
                .streams()
                .attach(&client_id)
                .map(|opened| opened.connection),
            Some(mine.key),
            "and the client id must still route to the connection that holds it, not to the \
             one that was just turned away"
        );

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_stream_connection_whose_acceptance_never_left_is_not_left_bound() {
        // The bind happens before the answer, so an answer that never goes out leaves the
        // client id pointing at a socket the client does not know it holds. It will never ack
        // on that connection — it was never told it was accepted — so a sink opened against
        // it spends its opening allowance and then stalls the session it feeds for good.
        let endpoint = crate::rpc::endpoint::scratch("unbind");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");
        let mine = PeerCredentials {
            pid: Some(std::process::id()),
            user: "me".to_owned(),
            daemon_user: "me".to_owned(),
        };

        let (peer, ours) = tokio::io::duplex(4096);
        let (ours_reader, _ours_writer) = tokio::io::split(ours);
        let (_peer_reader, peer_writer) = tokio::io::split(peer);
        ControlWriter::new(peer_writer)
            .write_frame(&HelloRequest::new(
                PROTOCOL_VERSION,
                ClientRole::Stream,
                client_id.clone(),
            ))
            .await
            .expect("the hello goes out");

        daemon
            .serve_io(
                ControlReader::new(ours_reader),
                ControlWriter::new(WriteFails),
                Some(mine),
            )
            .await;

        assert_eq!(
            daemon.streams().bound(),
            1,
            "the hello was accepted and the connection was bound; without that this asserts \
             nothing about the rollback"
        );
        assert_eq!(
            daemon.streams().connections(),
            0,
            "and the bind was given back when the acceptance could not be written"
        );
        assert!(
            daemon.streams().attach(&client_id).is_none(),
            "a client id left pointing at a socket nobody is reading serves the next attach \
             a sink that can never be acked"
        );

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn an_attach_whose_answer_never_left_gives_the_id_back_and_forgets_its_receipt() {
        // An attach's whole effect is the id in its answer. When that answer does not reach
        // the client, both halves of the effect have to go: the sink, which the client can
        // never ack because it never learned the id, and the receipt, which would otherwise
        // replay that id to a retry after the sink behind it had been given up — an attach
        // into nothing, reported as a success.
        let endpoint = crate::rpc::endpoint::scratch("rollback");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");
        let mine = PeerCredentials {
            pid: Some(std::process::id()),
            user: "me".to_owned(),
            daemon_user: "me".to_owned(),
        };
        let session = daemon
            .sessions()
            .create(&SessionCreate {
                kind: SessionKind::Shell,
                pane_key: None,
                profile: TestShell::pick().profile,
                cwd: None,
                env_overrides: BTreeMap::new(),
                cols: 80,
                rows: 24,
            })
            .expect("a shell session starts");
        // The stream connection the attach routes to. Held for the length of the test: an
        // attach with nowhere to write is refused, and a refusal would take this test through
        // a branch that has no rollback to make.
        let _stream = daemon.streams().bind(&client_id);
        let caller = Caller::new(Some(mine), &client_id, daemon.sessions());

        let request = RequestEnvelope::new(RequestPayload::StreamAttach(StreamAttach {
            handle: session.handle.clone(),
        }));
        let attach = request.request_id.clone();
        let (peer, ours) = tokio::io::duplex(4096);
        let (ours_reader, _ours_writer) = tokio::io::split(ours);
        let (_peer_reader, peer_writer) = tokio::io::split(peer);
        ControlWriter::new(peer_writer)
            .write_frame(&request)
            .await
            .expect("the attach goes out");

        daemon
            .serve_control(
                ControlReader::new(ours_reader),
                ControlWriter::new(WriteFails),
                &caller,
            )
            .await;

        assert_eq!(
            daemon.streams().next_stream_id(&client_id),
            Some(StreamId(StreamId::FIRST.get() + 1)),
            "an id was assigned, so the rollback below is the one after a real attach rather \
             than an assertion about an attach that never happened"
        );
        assert_eq!(
            daemon.streams().len(),
            0,
            "the sink goes with the answer that never arrived; kept, it spends its opening \
             allowance against a client that can never ack an id it was never told"
        );
        assert!(
            lock(&daemon.receipts)
                .get(&caller.fingerprint, &attach)
                .is_none(),
            "and the receipt goes with it, or a retry is handed the id of a sink that has \
             already been given up and told it succeeded"
        );

        daemon
            .sessions()
            .close(&session.handle)
            .expect("the session closes");
        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn credit_for_a_stream_that_has_been_detached_is_discarded_and_the_reader_goes_on() {
        // Proto's discard half, in the client-to-daemon direction. Control and stream are
        // separate sockets with nothing ordering them, so a detach always races the acks
        // already in flight for the id it retired — and dropping the connection over one
        // would take every other session riding it down for an ordinary detach.
        let endpoint = crate::rpc::endpoint::scratch("discard");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");

        let bound = daemon.streams().bind(&client_id);
        let retired = daemon.streams().attach(&client_id).expect("attaches").sink;
        let retired_id = retired.stream_id();
        assert!(daemon.streams().detach(&client_id, retired_id));
        let live = daemon.streams().attach(&client_id).expect("attaches").sink;
        let live_id = live.stream_id();
        // Spend some allowance first, so "the reader went on" is something this test can see
        // rather than something it infers from the absence of a teardown.
        live.send(&Frame::new(FrameKind::Output, live_id, vec![b'x'; 4096]));
        let spent = live.credit();

        let (mut peer, ours) = tokio::io::duplex(64 * 1024);
        let (ours_reader, ours_writer) = tokio::io::split(ours);
        let serving = tokio::spawn({
            let daemon = Arc::clone(&daemon);
            let client_id = client_id.clone();
            async move {
                daemon
                    .serve_stream(
                        ControlReader::new(ours_reader),
                        ControlWriter::new(ours_writer),
                        &client_id,
                        bound,
                    )
                    .await;
            }
        });

        peer.write_all(&credit_ack(retired_id, 4096))
            .await
            .expect("the stale ack goes out");
        peer.write_all(&credit_ack(live_id, 4096))
            .await
            .expect("the live ack goes out");
        peer.flush().await.expect("both acks are on the wire");

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && live.credit() <= spent {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            live.credit() > spent,
            "the ack queued behind the discarded one never landed; a daemon that drops the \
             connection over the tail of a detach race takes every other session on it down"
        );
        assert_eq!(
            daemon.streams().connections(),
            1,
            "a frame it merely discards must cost the connection nothing"
        );
        assert!(
            !serving.is_finished(),
            "and must leave the reader reading, which is the difference between discarding \
             one frame and discarding the connection"
        );

        serving.abort();
        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn credit_for_a_stream_id_that_was_never_assigned_drops_the_connection() {
        // Proto's other half, which the daemon used to opt out of: an id at or beyond the
        // next one to assign was never handed out here, so no sequence of events puts a
        // well-behaved client in this position. Discarding it instead is tolerable only for
        // as long as the client behaves — the protocol bounds nothing in this direction —
        // and a rule that holds in one direction is one the two ends can disagree about.
        let endpoint = crate::rpc::endpoint::scratch("desync");
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");

        let bound = daemon.streams().bind(&client_id);
        // One real attach first, so the watermark has moved and the id below is refused for
        // being past it rather than for being the first id on an untouched connection.
        let attached = daemon.streams().attach(&client_id).expect("attaches").sink;
        let never = daemon
            .streams()
            .next_stream_id(&client_id)
            .expect("the connection is bound");

        let (mut peer, ours) = tokio::io::duplex(64 * 1024);
        let (ours_reader, ours_writer) = tokio::io::split(ours);
        // Exactly the next id to assign. The *client* has one reason to wait on this one id —
        // the daemon may write a frame for an attach whose answer the client has not finished
        // parsing — and the daemon has none: it assigns the id before the answer naming it
        // goes out, so nothing can put a client here first.
        peer.write_all(&credit_ack(never, 1))
            .await
            .expect("the desynchronised ack goes out");
        peer.flush().await.expect("it is on the wire");

        let served = tokio::time::timeout(
            Duration::from_secs(10),
            daemon.serve_stream(
                ControlReader::new(ours_reader),
                ControlWriter::new(ours_writer),
                &client_id,
                bound,
            ),
        )
        .await;

        assert!(
            served.is_ok(),
            "credit naming an id this connection never assigned must drop it; discarded \
             instead, the daemon goes on reading from a peer it no longer agrees with"
        );
        assert_eq!(
            daemon.streams().connections(),
            0,
            "and dropping the connection is what it says: the socket and every sink on it"
        );
        assert!(
            attached.is_closed(),
            "including the streams that were riding it, which can never be acked again"
        );
        // Still open, so nothing but the rule ended this: a test where the client had hung up
        // would pass with the rule deleted.
        drop(peer);

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn every_hello_that_is_not_acceptable_is_refused_with_the_right_retryability() {
        let endpoint = crate::rpc::endpoint::scratch("hello");
        let (daemon, listener) = Daemon::bind(DaemonConfig::new(endpoint.clone())).expect("binds");
        let client_id: ClientId = "nysia-test".parse().expect("a well-formed client id");

        let mine = PeerCredentials {
            pid: Some(std::process::id()),
            user: "me".to_owned(),
            daemon_user: "me".to_owned(),
        };
        let theirs = PeerCredentials {
            user: "them".to_owned(),
            ..mine.clone()
        };

        let good = HelloRequest::new(PROTOCOL_VERSION, ClientRole::Control, client_id.clone());
        assert!(daemon.reject_reason(&good, Some(&mine)).is_none());

        // An account that is not this daemon's, and a peer the kernel would not name, are the
        // same refusal: neither is proof, and only proof gets in.
        for peer in [Some(&theirs), None] {
            let reason = daemon
                .reject_reason(&good, peer)
                .expect("an unproven peer is refused");
            assert!(matches!(reason, RejectReason::Unauthorized { .. }));
            assert!(!reason.retryable(), "retrying reproduces it exactly");
        }

        let ancient = HelloRequest::new(
            nysia_proto::ProtocolVersion(PROTOCOL_VERSION.get() + 1),
            ClientRole::Control,
            client_id,
        );
        let reason = daemon
            .reject_reason(&ancient, Some(&mine))
            .expect("a version outside the range is refused");
        match reason {
            RejectReason::UnsupportedVersion { daemon, attachable } => {
                // Both halves, so the client can say which of them has to change.
                assert_eq!(daemon, PROTOCOL_VERSION);
                assert_eq!(attachable, ProtocolRange::attachable());
            }
            other => panic!("expected an unsupported version, got {other:?}"),
        }

        // Shutting down is the one retryable reason: the next connection reaches a fresh
        // daemon, where the others would reproduce exactly.
        daemon.shutting_down.store(true, Ordering::Release);
        let reason = daemon
            .reject_reason(&good, Some(&mine))
            .expect("a retiring daemon takes no new clients");
        assert!(matches!(reason, RejectReason::ShuttingDown));
        assert!(reason.retryable());

        daemon.shutdown();
        drop(listener);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }
}
