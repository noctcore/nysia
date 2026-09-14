//! Interop: a real [`Daemon`] on a real socket, driven by a real [`Client`].
//!
//! # Why this module exists at all
//!
//! The daemon and the Tauri client shipped green on both runners and did not interoperate.
//! Neither tree had a test that made the two halves talk, so both gates were measuring one
//! half against its own assumptions: the credit exchange was inverted, the two ends counted
//! credit in different units, stream ids were numbered daemon-globally where proto scopes
//! them per connection, the replay raced the response that names it, and a second stream
//! connection under one client id took the first one's sinks down.
//!
//! Every one of those is invisible to a test that stops at the socket. So these tests do not
//! stop at the socket: they bind a daemon, dial it with [`Client`], open a second connection
//! in [`ClientRole::Stream`], and drive the binary frame protocol the way a renderer does —
//! decoding frames, classifying ids by proto's own rule, and acking **payload bytes after
//! rendering**, which is what §7.3 asks a consumer to do.
//!
//! # What each test pins down
//!
//! | Test | The claim |
//! |---|---|
//! | [`a_flood_keeps_flowing_past_the_per_stream_ceiling`] | credit round-trips, in units both ends agree on |
//! | [`an_attach_answers_before_a_byte_of_replay_is_enqueued`] | the response cannot lose to the replay it explains |
//! | [`a_second_stream_connection_does_not_take_the_first_one_s_streams_down`] | a reload does not deafen the window |
//! | [`a_client_grant_cannot_talk_the_daemon_out_of_its_own_window`] | the producer's window holds against a consumer's grant |
//! | [`a_replay_is_closed_by_one_boundary_with_live_output_strictly_after_it`] | a client can tell a replayed query from a live one |
//!
//! Each fails against the code as it was and passes against the code as it is.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nysia_proto::stream::UnattachedFrame;
use nysia_proto::{
    ClientId, ClientRole, CreditAck, CreditFrame, CreditWindow, Frame, FrameDecoder, FrameKind,
    SessionCreate, SessionCreated, SessionHandle, SessionKind, StreamId, TerminalRead,
    TerminalSend, TerminalWait, WaitFor,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::rpc::client::Client;
use crate::rpc::endpoint::Endpoint;
use crate::rpc::server::{Daemon, DaemonConfig};
use crate::rpc::testing::{TOKEN, TestShell};
use crate::rpc::transport::{ConnectionReader, ConnectionWriter};

/// No interop test may run longer than this. A test that can hang is a test that will.
const DEADLINE: Duration = Duration::from_secs(90);

/// How long an answer that must not wait on anything is given.
///
/// Generous for a local socket and still an order of magnitude short of the replay-blocked
/// case it has to tell apart.
const PROMPT_ANSWER: Duration = Duration::from_secs(10);

/// The longest a test may hold a session's terminal state before letting go by itself.
///
/// Three times [`PROMPT_ANSWER`], so a daemon that waits on the lock is still unambiguously
/// late — and finite, because a holder that waits to be released keeps the lock for however
/// long the test is stuck, and a test that fails by wedging the runtime reports nothing at
/// all. It burns the CI job's timeout instead, on the platform where that is most expensive.
const HOLD_AT_MOST: Duration = Duration::from_secs(30);

/// A window small enough that the accounting bug shows up in a test's worth of output.
///
/// The arithmetic, because it is the whole reason these numbers are not the defaults: a
/// daemon charging the nine-byte header drains `per_stream_initial / 9` frames' worth of
/// allowance no matter how diligently the client acks. At the shipped defaults that is
/// ~58,000 frames of 48 KiB — 2.7 GB, which no test will ever push. At 2 KiB and 256-byte
/// chunks it is ~227 frames, or about 58 KiB of output, which one shell loop produces in a
/// second. The bug is identical; only the patience needed to see it changes.
const TIGHT: CreditWindow = CreditWindow {
    per_stream_initial: 2 * 1024,
    per_stream_max: 4 * 1024,
    total_initial: 2 * 1024,
    total_max: 4 * 1024,
    pending_cap: 2 * 1024,
    ack_batch: 512,
    chunk: 256,
};

/// A daemon listening on a scratch endpoint, torn down on drop.
struct Harness {
    daemon: Arc<Daemon>,
    endpoint: Endpoint,
    serving: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// Bind, start serving, and hand back the endpoint to dial.
    fn start(tag: &str, window: CreditWindow) -> Self {
        let endpoint = crate::rpc::endpoint::scratch(tag);
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            // Never retire: these tests hold a control connection open across waits long
            // enough that an idle timer would be a second, invisible failure mode.
            idle_retire_after: None,
            credit_window: window,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("a scratch daemon binds");
        let serving = tokio::spawn({
            let daemon = Arc::clone(&daemon);
            async move {
                let _ = daemon.serve(listener).await;
            }
        });
        Self {
            daemon,
            endpoint,
            serving,
        }
    }

    /// A control connection, handshaken.
    async fn control(&self, client_id: &ClientId) -> Client {
        Client::connect(&self.endpoint, client_id, ClientRole::Control)
            .await
            .expect("a control connection is accepted")
    }

    /// A stream connection, handshaken and ready to decode frames.
    async fn stream(&self, client_id: &ClientId) -> StreamPeer {
        let client = Client::connect(&self.endpoint, client_id, ClientRole::Stream)
            .await
            .expect("a stream connection is accepted");
        StreamPeer::new(client)
    }

    /// Stop serving and take the scratch directory with it.
    fn stop(self) {
        self.daemon.shutdown();
        self.serving.abort();
        let _ = std::fs::remove_dir_all(self.endpoint.runtime_dir());
    }
}

/// The client half of a stream connection, behaving the way §7.3 asks a consumer to.
///
/// It renders (into a buffer, which is all a test needs of a renderer), acks **payload
/// bytes** once [`CreditWindow::ack_batch`] of them have been rendered, and applies proto's
/// own routing rule to any id it has no entry for. That last part is what makes this a
/// protocol test rather than a byte counter: a frame naming an id at or beyond the next one
/// this connection would be assigned is, by proto's definition, a desync — so it fails the
/// test here instead of silently dropping a connection in front of a user.
struct StreamPeer {
    reader: ConnectionReader,
    writer: ConnectionWriter,
    decoder: FrameDecoder,
    /// Ids this connection has been told about, by an attach response.
    live: HashSet<u32>,
    /// One past the highest id this connection has been given: proto's watermark.
    next_to_assign: StreamId,
    /// The window the daemon announced in its opening grant, per stream.
    window: HashMap<u32, CreditWindow>,
    /// Bytes rendered but not yet acked, per stream.
    unacked: HashMap<u32, u32>,
    /// Everything rendered, per stream.
    rendered: HashMap<u32, Vec<u8>>,
    /// How many payload bytes have been rendered in total, across every stream.
    total_rendered: u64,
    /// How many replay boundaries each stream has been sent.
    ///
    /// A count rather than a flag, because "exactly once per attach" is the claim and a flag
    /// would be satisfied by a daemon that marked every chunk.
    boundaries: HashMap<u32, u32>,
    /// How much had been rendered for a stream when its first boundary arrived.
    ///
    /// The seam, kept as an offset so a test can ask what was *replayed* separately from what
    /// arrived live afterwards. A marker in the wrong place would still satisfy a test that
    /// only counted them.
    boundary_at: HashMap<u32, usize>,
    /// Whether rendering sends an ack back.
    ///
    /// A consumer that acks is the normal case and the only one a flood can flow through.
    /// Turning it off is how a test watches the allowance *hold*: with nothing coming back,
    /// whatever still arrives arrived on credit the daemon granted, and the total is then a
    /// direct measurement of the window in force rather than of the round trip.
    acks: bool,
}

impl StreamPeer {
    fn new(client: Client) -> Self {
        let (leftover, reader, writer) = client.into_stream_parts();
        let mut decoder = FrameDecoder::new();
        // Whatever the handshake read past the `hello` line is already frame bytes. Dropping
        // it would lose the opening grant for no reason a client could ever diagnose.
        decoder.push(&leftover);
        Self {
            reader,
            writer,
            decoder,
            live: HashSet::new(),
            next_to_assign: StreamId::FIRST,
            window: HashMap::new(),
            unacked: HashMap::new(),
            rendered: HashMap::new(),
            total_rendered: 0,
            boundaries: HashMap::new(),
            boundary_at: HashMap::new(),
            acks: true,
        }
    }

    /// Stop acking, so the daemon gets no credit back from here on.
    fn stop_acking(&mut self) {
        self.acks = false;
    }

    /// Record an id the daemon has just handed this connection, moving the watermark.
    fn assign(&mut self, stream_id: StreamId) {
        self.live.insert(stream_id.get());
        if let Some(next) = stream_id.next()
            && next > self.next_to_assign
        {
            self.next_to_assign = next;
        }
    }

    /// Read once and handle every frame that completes, acking what got rendered.
    ///
    /// `Ok(false)` means the daemon closed the connection.
    async fn pump(&mut self, patience: Duration) -> std::io::Result<bool> {
        let mut buffer = [0u8; 8192];
        let read = match tokio::time::timeout(patience, self.reader.read(&mut buffer)).await {
            Ok(Ok(0)) => return Ok(false),
            Ok(Ok(read)) => read,
            Ok(Err(err)) => return Err(err),
            // Nothing arrived in time. Not an error: the caller is looping against its own
            // deadline and decides what a quiet socket means.
            Err(_) => return Ok(true),
        };
        self.decoder.push(&buffer[..read]);
        let mut acks: Vec<(StreamId, u32)> = Vec::new();
        loop {
            let frame = match self.decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(err) => panic!("the daemon wrote a frame this client cannot decode: {err}"),
            };
            if !self.live.contains(&frame.stream.get()) {
                // Proto's rule, applied rather than described. `DropConnection` is the case
                // the ordering fix exists to make impossible.
                assert_eq!(
                    frame.stream.classify_unattached(self.next_to_assign),
                    UnattachedFrame::DiscardFrame,
                    "the daemon sent a frame for stream {} before telling this connection the \
                     id existed; the next id this connection expects to be assigned is {}",
                    frame.stream,
                    self.next_to_assign
                );
                continue;
            }
            if let Some(ack) = self.render(&frame) {
                acks.push((frame.stream, ack));
            }
        }
        for (stream, bytes) in acks {
            self.ack(stream, bytes).await?;
        }
        Ok(true)
    }

    /// Take one frame for a live stream, returning an ack that is now due.
    fn render(&mut self, frame: &Frame) -> Option<u32> {
        let id = frame.stream.get();
        match frame.kind {
            FrameKind::Credit => {
                match serde_json::from_slice::<CreditFrame>(&frame.payload) {
                    Ok(CreditFrame::Grant(grant)) => {
                        self.window.insert(id, grant.window);
                    }
                    // The daemon is the producer; it never acks.
                    Ok(CreditFrame::Ack(ack)) => panic!("the daemon acked its own stream: {ack:?}"),
                    Err(err) => panic!("the daemon wrote an unreadable credit frame: {err}"),
                }
                None
            }
            FrameKind::Output => {
                // "Rendered" is the whole point of the window: this stands in for xterm's
                // `write()` callback, and the ack below is what that callback triggers.
                self.rendered
                    .entry(id)
                    .or_default()
                    .extend_from_slice(&frame.payload);
                self.total_rendered += frame.payload.len() as u64;
                let batch = self
                    .window
                    .get(&id)
                    .map_or(CreditWindow::DEFAULT.ack_batch, |window| window.ack_batch);
                let owed = self.unacked.entry(id).or_default();
                *owed = owed.saturating_add(
                    u32::try_from(frame.payload.len()).expect("a frame payload fits a u32"),
                );
                if self.acks && *owed >= batch {
                    return Some(std::mem::take(owed));
                }
                None
            }
            FrameKind::ReplayEnd => {
                *self.boundaries.entry(id).or_default() += 1;
                let so_far = self.rendered.get(&id).map_or(0, Vec::len);
                self.boundary_at.entry(id).or_insert(so_far);
                None
            }
            // An exit or a bell is not rendered text and is never acked.
            _ => None,
        }
    }

    /// How many replay boundaries this stream has been sent.
    fn boundary_count(&self, stream: StreamId) -> u32 {
        self.boundaries.get(&stream.get()).copied().unwrap_or(0)
    }

    /// What arrived before the first boundary — the replay proper.
    fn replayed(&self, stream: StreamId) -> String {
        let seam = self.boundary_at.get(&stream.get()).copied().unwrap_or(0);
        self.rendered
            .get(&stream.get())
            .map(|bytes| String::from_utf8_lossy(&bytes[..seam.min(bytes.len())]).into_owned())
            .unwrap_or_default()
    }

    /// What arrived after it — everything the child has written since.
    ///
    /// Empty while no boundary has arrived, which is the honest reading: with no seam there
    /// is nothing that is provably live.
    fn after_boundary(&self, stream: StreamId) -> String {
        let Some(&seam) = self.boundary_at.get(&stream.get()) else {
            return String::new();
        };
        self.rendered
            .get(&stream.get())
            .map(|bytes| String::from_utf8_lossy(&bytes[seam.min(bytes.len())..]).into_owned())
            .unwrap_or_default()
    }

    /// Tell the daemon how many payload bytes have been rendered.
    ///
    /// Payload bytes, because that is all a renderer ever sees. An ack counted in encoded
    /// bytes would return more than was spent; one counted here while the daemon charged the
    /// header returns less, and the difference is the leak that strands a long session.
    async fn ack(&mut self, stream: StreamId, bytes: u32) -> std::io::Result<()> {
        let payload = serde_json::to_vec(&CreditFrame::Ack(CreditAck { bytes }))
            .expect("a credit ack serialises");
        let encoded = nysia_proto::encode(&Frame::new(FrameKind::Credit, stream, payload))
            .expect("a credit ack encodes");
        self.writer.write_all(&encoded).await?;
        self.writer.flush().await
    }

    /// Flush whatever is rendered but still unacked, whatever the batch says.
    async fn ack_everything(&mut self) -> std::io::Result<()> {
        let owed: Vec<(u32, u32)> = self
            .unacked
            .iter()
            .filter(|(_, bytes)| **bytes > 0)
            .map(|(id, bytes)| (*id, *bytes))
            .collect();
        for (id, bytes) in owed {
            self.unacked.insert(id, 0);
            self.ack(StreamId(id), bytes).await?;
        }
        Ok(())
    }

    /// Everything rendered for one stream, as text.
    fn text(&self, stream: StreamId) -> String {
        self.rendered
            .get(&stream.get())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    }

    /// Pump until `predicate` holds over what has been rendered, or give up.
    async fn until(&mut self, predicate: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            if predicate(self) {
                return true;
            }
            if !self
                .pump(Duration::from_millis(250))
                .await
                .expect("reading a stream connection")
            {
                break;
            }
        }
        predicate(self)
    }
}

/// Start a shell session through the daemon, the way any client would.
async fn start_shell(client: &mut Client) -> SessionCreated {
    client
        .session_create(SessionCreate {
            kind: SessionKind::Shell,
            pane_key: None,
            profile: TestShell::pick().profile,
            cwd: None,
            env_overrides: std::collections::BTreeMap::new(),
            cols: 80,
            rows: 24,
        })
        .await
        .expect("a shell session starts")
}

/// Wait until the shell has drawn its prompt and gone quiet.
///
/// Typing before this reliably loses rather than occasionally: a shell that has not finished
/// starting echoes what is typed and then redraws the line when its line editor takes over,
/// so the command appears twice and runs zero times.
async fn await_prompt(client: &mut Client, handle: &SessionHandle) {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        let read = client
            .terminal_read(TerminalRead::screen(handle.clone()))
            .await
            .expect("reading the screen");
        if !read.lines.join("\n").trim().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    settle(client, handle).await;
}

/// Wait for the session to stop producing output.
async fn settle(client: &mut Client, handle: &SessionHandle) {
    let _ = client
        .terminal_wait(TerminalWait {
            handle: handle.clone(),
            wait_for: WaitFor::Idle,
            timeout_ms: Some(DEADLINE.as_millis() as u64),
        })
        .await;
}

/// Type the lines that make the shell *compute* [`TOKEN`].
async fn compute_token(client: &mut Client, handle: &SessionHandle) {
    for line in TestShell::pick().lines {
        client
            .terminal_send(TerminalSend::line(handle.clone(), line))
            .await
            .expect("writing a line");
        // Settle between lines rather than wait for the token: only the *last* line produces
        // it, and `cmd` needs the assignment to have run before it parses the line using it.
        settle(client, handle).await;
    }
}

fn client_id(name: &str) -> ClientId {
    name.parse().expect("a well-formed client id")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flood_keeps_flowing_past_the_per_stream_ceiling() {
    // Decision B. The consumer acks what it rendered, in payload bytes; the producer must
    // charge the same units or the allowance leaks the frame header on every frame and the
    // session stalls for good — with the pump stopped, which freezes `terminal read` for the
    // CLI and the hooks too, not just the window.
    let harness = Harness::start("iflood", TIGHT);
    let me = client_id("nysia-interop-flood");
    let mut control = harness.control(&me).await;
    let mut stream = harness.stream(&me).await;

    let created = start_shell(&mut control).await;
    await_prompt(&mut control, &created.handle).await;

    let attached = control
        .stream_attach(created.handle.clone())
        .await
        .expect("attaches");
    stream.assign(attached.stream_id);

    let shell = TestShell::pick();
    control
        .terminal_send(TerminalSend::line(created.handle.clone(), shell.flood))
        .await
        .expect("starting the flood");
    // The token goes in behind the flood, so seeing it is proof the whole flood got through
    // rather than proof that some of it did. Nothing typed contains the token.
    for line in shell.lines {
        control
            .terminal_send(TerminalSend::line(created.handle.clone(), line))
            .await
            .expect("writing a line");
    }

    let id = attached.stream_id;
    let arrived = stream.until(|peer| peer.text(id).contains(TOKEN)).await;
    assert!(
        arrived,
        "the flood stalled after {} rendered bytes and never reached the token; the opening \
         allowance is {} bytes, so a window that replenishes cannot stop here",
        stream.total_rendered, TIGHT.per_stream_initial
    );
    assert!(
        stream.total_rendered > u64::from(TIGHT.per_stream_initial),
        "nothing was proven: {} bytes is inside the opening allowance, so no credit ever had \
         to come back",
        stream.total_rendered
    );

    control
        .session_close(created.handle)
        .await
        .expect("closes the session");
    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attach_answers_before_a_byte_of_replay_is_enqueued() {
    // Decision D, made deterministic. The replay reads the session's terminal state, so a
    // test that holds that lock holds the replay: a daemon that replays before it answers
    // cannot answer at all, and a daemon that answers first is untouched by the lock. The
    // race becomes an assertion, in both directions, with no sleeps and no repetition.
    let harness = Harness::start("iattach", CreditWindow::DEFAULT);
    let me = client_id("nysia-interop-attach");
    let mut control = harness.control(&me).await;
    let mut stream = harness.stream(&me).await;

    let created = start_shell(&mut control).await;
    await_prompt(&mut control, &created.handle).await;
    // Scrollback worth replaying: this is the re-attach D-1 is about, not a fresh pane.
    compute_token(&mut control, &created.handle).await;

    let session = harness
        .daemon
        .sessions()
        .get(&created.handle)
        .expect("the session is there");
    let vt = Arc::clone(session.terminal_state());
    let (taken, taken_rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = vt.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        taken.send(()).ok();
        // Self-releasing, rather than waiting to be told. A holder that waits holds the lock
        // for as long as the test is stuck, and the daemon blocked behind it holds a runtime
        // worker — so a failure stops reporting and starts wedging, which on Windows means
        // burning the whole CI job's timeout and saying nothing about what broke.
        release_rx.recv_timeout(HOLD_AT_MOST).ok();
    });
    // Both blocking waits go off the runtime. The workers they would occupy are the ones
    // serving the daemon this test is talking to, and one of them drives the timer below.
    tokio::task::spawn_blocking(move || taken_rx.recv_timeout(DEADLINE))
        .await
        .expect("the holder thread ran")
        .expect("the terminal state lock was taken");

    let asked = Instant::now();
    let answered =
        tokio::time::timeout(PROMPT_ANSWER, control.stream_attach(created.handle.clone())).await;
    let waited = asked.elapsed();
    release.send(()).ok();
    let _ = tokio::task::spawn_blocking(move || holder.join()).await;

    let attached = answered
        .expect(
            "the attach response waited on the replay ring; nothing orders the control and \
             stream sockets, so a replay enqueued first can beat the answer that names its id",
        )
        .expect("attaches");
    // Not merely inside the timeout. An answer that arrives at nine seconds arrived *because*
    // the lock was released, which is the same false pass by a slower route — the claim is
    // that the replay is not on the answer's path at all, so the answer owes nothing to a
    // lock this test still holds.
    assert!(
        waited < PROMPT_ANSWER / 4,
        "the attach took {waited:?}, which is long enough that it was waiting on the replay \
         rather than on a socket round trip"
    );
    stream.assign(attached.stream_id);

    // And the replay does arrive, once it can — on the id the answer named, which is the
    // only id this connection will accept a frame for.
    let id = attached.stream_id;
    assert_eq!(
        id,
        StreamId::FIRST,
        "the first attach on a fresh connection"
    );
    let replayed = stream.until(|peer| peer.text(id).contains(TOKEN)).await;
    assert!(
        replayed,
        "the scrollback never replayed; a re-attach to a live session is D-1's flagship path"
    );

    control
        .session_close(created.handle)
        .await
        .expect("closes the session");
    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_stream_connection_does_not_take_the_first_one_s_streams_down() {
    // Decisions C and E. A webview reload opens a second stream socket under the same client
    // id. Ids are per connection, so the new one counts from the start; and the old one's
    // teardown — which on Windows cannot be interrupted and so arrives whenever it arrives —
    // must close only what it opened.
    let harness = Harness::start("ireload", CreditWindow::DEFAULT);
    let me = client_id("nysia-interop-reload");
    let mut control = harness.control(&me).await;

    let first = harness.stream(&me).await;
    // Not a wait: an invariant. A connection is bound before its hello is answered, so by the
    // time `stream` has returned the daemon has already counted it — which is the contract the
    // real client relies on when it attaches the instant its connect returns, and a contract
    // a test that merely waited would hide rather than check.
    assert_eq!(harness.daemon.streams().bound(), 1);
    let created = start_shell(&mut control).await;
    await_prompt(&mut control, &created.handle).await;
    let before = control
        .stream_attach(created.handle.clone())
        .await
        .expect("attaches");
    assert_eq!(
        before.stream_id,
        StreamId::FIRST,
        "the first attach on the first connection"
    );

    // The reload.
    let mut second = harness.stream(&me).await;
    assert_eq!(harness.daemon.streams().bound(), 2);
    let after = control
        .stream_attach(created.handle.clone())
        .await
        .expect("attaches on the new connection");
    assert_eq!(
        after.stream_id,
        StreamId::FIRST,
        "stream ids are scoped to one stream connection, so a fresh one counts from the \
         start; a daemon-global counter makes the client's discard-versus-drop watermark \
         unknowable and hands out an id the new connection has no way to have expected"
    );
    second.assign(after.stream_id);

    // The superseded reader finally wakes and lets go. Everything it takes with it must be
    // its own.
    drop(first);

    compute_token(&mut control, &created.handle).await;
    let id = after.stream_id;
    let flowing = second.until(|peer| peer.text(id).contains(TOKEN)).await;
    assert!(
        flowing,
        "output stopped reaching the surviving connection after the superseded one went; a \
         window in this state shows a dead pane while still believing it is attached"
    );
    second.ack_everything().await.expect("acking");
    // By now the superseded connection's teardown has certainly run — the output above came
    // through after it — so this is the state it left behind, not a state it has yet to reach.
    assert_eq!(
        harness.daemon.streams().connections(),
        1,
        "the superseded connection's teardown took the connection that replaced it with it; an \
         unbind must close only what its own connection owns"
    );
    assert_eq!(harness.daemon.streams().len(), 1, "and its stream with it");

    // And the connection is still usable for a session it has not seen before, rather than
    // refused with "this client has no stream connection to route output to".
    let another = start_shell(&mut control).await;
    let fresh = control
        .stream_attach(another.handle.clone())
        .await
        .expect("a fresh attach must still find the surviving stream connection");
    assert_eq!(
        fresh.stream_id,
        StreamId(2),
        "the surviving connection keeps counting; a detached or retired id is never recycled"
    );

    control
        .session_close(created.handle)
        .await
        .expect("closes the session");
    control
        .session_close(another.handle)
        .await
        .expect("closes the session");
    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_grant_cannot_talk_the_daemon_out_of_its_own_window() {
    // Decision A, from the wire. The daemon owns the window and issues grants; a consumer
    // that could grant itself credit would be turning the backpressure off from outside.
    let harness = Harness::start("igrant", TIGHT);
    let me = client_id("nysia-interop-grant");
    let mut control = harness.control(&me).await;
    let mut stream = harness.stream(&me).await;

    let created = start_shell(&mut control).await;
    await_prompt(&mut control, &created.handle).await;
    let attached = control
        .stream_attach(created.handle.clone())
        .await
        .expect("attaches");
    stream.assign(attached.stream_id);

    // Read the opening grant, so the window below is the daemon's own number rather than a
    // constant this test holds a second copy of.
    let id = attached.stream_id;
    assert!(
        stream
            .until(|peer| peer.window.contains_key(&id.get()))
            .await,
        "the daemon opens a stream by announcing the window"
    );
    assert_eq!(
        stream.window.get(&id.get()).copied(),
        Some(TIGHT),
        "the grant carries the window in force, so the client holds no copy of the constants"
    );

    let sink = harness
        .daemon
        .streams()
        .attach(&client_id("nysia-interop-onlooker"));
    assert!(
        sink.is_none(),
        "a client with no stream connection has nothing to attach to"
    );

    // Stop acking. From here the daemon gets nothing back, so whatever still arrives
    // arrived on credit the daemon itself granted — which is what makes the total below a
    // measurement of the window rather than of a round trip.
    stream.stop_acking();

    // A grant from the consumer, for far more than the window allows. The daemon must not
    // act on it: it is the producer, it owns the window, and a consumer that could credit
    // itself could turn the backpressure off from outside.
    let payload = serde_json::to_vec(&CreditFrame::Grant(nysia_proto::CreditGrant {
        bytes: u32::MAX,
        window: CreditWindow::DEFAULT,
    }))
    .expect("serialises");
    let encoded =
        nysia_proto::encode(&Frame::new(FrameKind::Credit, id, payload)).expect("encodes");
    stream.writer.write_all(&encoded).await.expect("writes");
    stream.writer.flush().await.expect("flushes");

    // Now flood, and read without ever acking. A daemon that honoured that grant has had
    // `u32::MAX` handed to it and stops at `per_stream_max`; one that ignores it stops at the
    // opening allowance, which is the only credit it ever issued.
    let before = stream.total_rendered;
    control
        .terminal_send(TerminalSend::line(
            created.handle.clone(),
            TestShell::pick().flood,
        ))
        .await
        .expect("starting the flood");

    let quiet = Duration::from_secs(3);
    let mut last_change = Instant::now();
    let mut seen = stream.total_rendered;
    while last_change.elapsed() < quiet {
        if !stream
            .pump(Duration::from_millis(250))
            .await
            .expect("reading")
        {
            break;
        }
        if stream.total_rendered != seen {
            seen = stream.total_rendered;
            last_change = Instant::now();
        }
    }

    let on_credit = stream.total_rendered - before;
    let allowed = u64::from(TIGHT.per_stream_initial + TIGHT.chunk);
    assert!(
        on_credit > 0,
        "nothing arrived at all, so this measured no window"
    );
    assert!(
        on_credit <= allowed,
        "{on_credit} bytes arrived against an opening allowance of {} — the daemon took the          consumer's grant, which is the backpressure being switched off from outside",
        TIGHT.per_stream_initial
    );

    // And the connection is intact: an ignored grant is not a fatal frame.
    let alive = control
        .terminal_read(TerminalRead::screen(created.handle.clone()))
        .await;
    assert!(alive.is_ok(), "an ignored grant must not cost the session");
    assert_eq!(harness.daemon.streams().len(), 1, "nor the stream it named");

    control
        .session_close(created.handle)
        .await
        .expect("closes the session");
    harness.stop();
}

/// The daemon half of the re-attach input defect: the replay says where it stops.
///
/// **What this can and cannot prove.** It proves the wire contract — one boundary per attach,
/// after the last replayed byte, before anything live. It cannot prove the fix end to end,
/// because the input that corrupted the shell was never written by any code in this
/// repository: it was xterm answering a `ESC[6n` it found in the replayed scrollback, and
/// there is no xterm in a Rust test. That half is proved in
/// `apps/web/src/transport/surface/surface.test.ts` against a terminal stub that answers
/// queries the way a real one does, and end to end by the relaunch script in `scripts/e2e`.
///
/// Against the code as it was, the daemon sends no boundary at all and the first assertion
/// below fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replay_is_closed_by_one_boundary_with_live_output_strictly_after_it() {
    let harness = Harness::start("ibound", CreditWindow::DEFAULT);
    let first = client_id("nysia-interop-boundary-a");
    let mut control = harness.control(&first).await;
    let mut stream = harness.stream(&first).await;

    let created = start_shell(&mut control).await;
    await_prompt(&mut control, &created.handle).await;
    // Scrollback worth replaying. A fresh pane has nothing to mark the end of, and a test
    // that attached to one would pass against a daemon that never replays.
    //
    // **The first window has to attach and drain, not merely type.** A session nothing is
    // watching coalesces its output and holds it: the pump has no sink to flush into, so the
    // bytes stay pending and are delivered to the *first* sink that appears — after its
    // replay and after its boundary, as live output. Skipping this step made the second
    // window's "replay" a bare prompt with the whole session arriving live behind it, which
    // reads exactly like a boundary in the wrong place and failed on both runners while
    // passing locally, where `cmd` writes little enough for the screen to hold the token
    // anyway. Draining here is what makes the token genuinely *past* by the time the second
    // window arrives, which is what a relaunch actually is.
    let watching = control
        .stream_attach(created.handle.clone())
        .await
        .expect("the first window routes the session it is watching");
    stream.assign(watching.stream_id);
    compute_token(&mut control, &created.handle).await;
    let watched = stream
        .until(|peer| peer.text(watching.stream_id).contains(TOKEN))
        .await;
    assert!(
        watched,
        "the first window never saw the token it typed, so there is nothing to relaunch into"
    );
    // The precondition the seam assertions below rest on, checked separately so a failure
    // says which half broke. The replay is a ring of the raw bytes the pump fed it, so a
    // daemon whose screen holds the token has the token to replay; without this, a stale
    // screen would fail as "the boundary landed in the wrong place", which it would not have.
    let screen = control
        .terminal_read(TerminalRead::screen(created.handle.clone()))
        .await
        .expect("reading the screen");
    assert!(
        screen
            .lines
            .join(
                "
"
            )
            .contains(TOKEN),
        "the daemon's own screen does not hold {TOKEN}, so there is nothing for a re-attach          to replay; it holds {:?}",
        screen.lines
    );

    drop(stream);
    drop(control);

    // The relaunch, as far as the daemon is concerned: a window it has never seen, attaching
    // to a session that outlived the last one.
    let second = client_id("nysia-interop-boundary-b");
    let mut control = harness.control(&second).await;
    let mut stream = harness.stream(&second).await;
    let attached = control
        .stream_attach(created.handle.clone())
        .await
        .expect("the second window attaches to the session it found");
    let id = attached.stream_id;
    stream.assign(id);

    let marked = stream.until(|peer| peer.boundary_count(id) > 0).await;
    assert!(
        marked,
        "the attach replayed {:?} and never said where the replay stopped; a client cannot \
         tell a replayed query from a live one without that frame, and its answers reach the \
         child as keystrokes",
        stream.text(id)
    );

    // The scrollback is on the replay side of the seam. A boundary that arrived first would
    // be a marker in the wrong place, which is worse than no marker: it opens a client's
    // input gate while the bytes that provoke the answers are still to come.
    let replayed = stream.until(|peer| peer.replayed(id).contains(TOKEN)).await;
    assert!(
        replayed,
        "the boundary landed before the scrollback it was supposed to close; the replay side \
         holds {:?} and the live side {:?}",
        stream.replayed(id),
        stream.after_boundary(id)
    );

    // And what the session writes *next* is on the other side of it. This is the half that
    // makes the seam meaningful rather than decorative — and it proves the session is still
    // a shell rather than a recording, which is the rest of D-1's sentence.
    compute_token(&mut control, &created.handle).await;
    let answered = stream
        .until(|peer| peer.after_boundary(id).contains(TOKEN))
        .await;
    assert!(
        answered,
        "the re-attached session's own output did not arrive after the boundary; the live \
         side holds {:?}",
        stream.after_boundary(id)
    );

    // Exactly once. A daemon that marked every chunk would satisfy every assertion above and
    // would tell a client the replay had ended while it was still replaying.
    assert_eq!(
        stream.boundary_count(id),
        1,
        "the attach sent {} boundaries; the contract is one per attach",
        stream.boundary_count(id)
    );

    control
        .session_close(created.handle)
        .await
        .expect("closes the session");
    harness.stop();
}
