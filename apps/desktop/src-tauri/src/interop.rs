//! The window's client against a real daemon, over a real socket.
//!
//! ## Why this file exists
//!
//! Both halves of this protocol were implemented against fakes, both suites were green, and
//! the two did not interoperate at all. The client returned credit as a `Grant` and the
//! daemon honours only an `Ack`, so nothing ever replenished and a flood stopped dead at the
//! per-stream ceiling — with the daemon's pump blocked behind it, which froze `terminal
//! read` for that session for the CLI and for hooks too. On Unix the window composed a
//! socket path the daemon does not bind, so it could never find one at all.
//!
//! Neither gate could see any of it. A fake models the preconditions its author already
//! understood, which is exactly the set of things that were not wrong. **The only test that
//! can catch a protocol disagreement is one where both implementations are present**, and
//! that is the whole claim of this module: a `nysia_core::rpc::Daemon` bound on its own
//! endpoint, this crate's [`crate::state::Client`] dialling it, and no stand-in between them
//! but the webview — which is a [`FrameSink`] here, because there is no webview in a test.
//!
//! The fake daemon in `apps/web` stays for the fast cases. It is not a substitute for this.
//!
//! ## Why the harness looks like this
//!
//! The daemon's accept loop is async; this client is synchronous on purpose, because every
//! Tauri command reaches it through `spawn_blocking` (traps register #2). So the runtime
//! lives on a thread of the harness's own and the test bodies are plain `#[test]`. Written
//! as `#[tokio::test]` instead, the first blocking `connect` on the current-thread runtime
//! parks the executor the daemon needs in order to answer it, and the test hangs for its
//! whole deadline with nothing in the log.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nysia_core::rpc::{Daemon, DaemonConfig, Endpoint, EnvSource};
use nysia_proto::credit::CreditWindow;
use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::frame::{FrameDecoder, FrameKind};
use nysia_proto::identity::{SessionHandle, SessionKind};
use nysia_proto::session::{SessionCreate, ShellProfile};
use nysia_proto::stream::StreamId;
use nysia_proto::terminal::{TerminalRead, TerminalSend, TerminalWait, WaitFor};
use nysia_proto::version::PROTOCOL_VERSION;

use crate::channel::FrameSink;
use crate::state::Client;

/// Every wait here is bounded; a test that can hang is a test that will.
const DEADLINE: Duration = Duration::from_secs(30);

/// The same deadline, as the wire spells one.
const DEADLINE_MS: u64 = 30_000;

/// A daemon of this test's own, serving on an endpoint nothing else uses.
///
/// Isolated rather than convenient. Binding the endpoint `Endpoint::from_env` resolves would
/// collide with whatever daemon the developer has running and with the other leg of CI, so
/// the runtime directory is overridden — which `EnvSource::isolated` then carries into the
/// Windows pipe name as well, because a pipe lives in a machine-global namespace and a
/// directory alone would not separate two of them.
struct Harness {
    daemon: Arc<Daemon>,
    endpoint: Endpoint,
    runtime: Option<tokio::runtime::Runtime>,
    runtime_dir: PathBuf,
}

impl Harness {
    fn start(tag: &str) -> Self {
        let endpoint = scratch(tag);
        let runtime_dir = endpoint.runtime_dir().to_path_buf();

        // Multi-threaded on purpose: the test thread blocks in socket reads that only the
        // daemon can answer, so the daemon must be running on threads the test is not
        // holding. A current-thread runtime deadlocks here for exactly that reason.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the daemon");

        // Inside the runtime, not beside it: binding a Windows named pipe registers it with
        // the reactor, and `Listener::bind` panics outright with "there is no reactor
        // running" when called from a plain thread.
        //
        // `idle_retire_after: None` so a test's own pauses — and they are long, a shell has
        // to start — cannot look like an idle daemon and retire it mid-test.
        let (daemon, listener) = runtime
            .block_on(async {
                Daemon::bind(DaemonConfig {
                    idle_retire_after: None,
                    ..DaemonConfig::new(endpoint.clone())
                })
            })
            .expect("the daemon binds its own endpoint");

        runtime.spawn({
            let daemon = Arc::clone(&daemon);
            let listener = listener;
            // The real accept loop, not a stand-in. Everything this module claims rests on
            // that.
            async move {
                let _ = daemon.serve(listener).await;
            }
        });

        Self {
            daemon,
            endpoint,
            runtime: Some(runtime),
            runtime_dir,
        }
    }

    /// A client of this daemon, connected and with its output connection open.
    fn window(&self, sink: impl FrameSink) -> Client {
        let client = Client::at(self.endpoint.clone());
        client.connect().expect("the window connects");
        client
            .attach_channel(sink)
            .expect("the stream connection opens");
        client
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.daemon.shutdown();
        // Without a deadline this blocks until every spawned task ends, and `serve` only
        // ends on its own terms.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(5));
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// An endpoint under a short scratch directory.
///
/// Short for the reason `nysia-core`'s own helper is: macOS caps a Unix socket path at 103
/// bytes and its `TMPDIR` is already about half of that, so anything descriptive here
/// overruns the cap and every test in the module fails to resolve an endpoint at all.
pub(crate) fn scratch(tag: &str) -> Endpoint {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = format!("nysd{:x}{unique:x}", std::process::id());
    let base = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let runtime_dir = base.join(name);
    let _ = std::fs::remove_dir_all(&runtime_dir);

    let source = EnvSource {
        runtime_dir_override: Some(runtime_dir),
        endpoint_override: None,
        home: None,
        xdg_runtime_dir: None,
        account: format!("nysia-test-{tag}"),
        isolated: true,
    };
    Endpoint::resolve(PROTOCOL_VERSION, source).expect("a scratch endpoint resolves")
}

/// The webview, as far as the client can tell.
///
/// Decodes each coalesced window the dispatcher delivers and reports what it "rendered",
/// which is what returns credit. It acks **output frames only**, because that is precisely
/// what `TerminalRouter` in `apps/web` acks — a sink that acked everything would be a kinder
/// peer than the real one and would hide a stall the real one produces.
///
/// It acks immediately rather than after a delay. A real xterm takes a few milliseconds; the
/// protocol property under test is that the ack replenishes at all, not how fast.
#[derive(Clone)]
struct Webview {
    client: Arc<Mutex<Option<Client>>>,
    decoder: Arc<Mutex<FrameDecoder>>,
    /// Payload bytes of output delivered, per stream.
    seen: Arc<Mutex<BTreeMap<StreamId, u64>>>,
    /// Payload bytes of output delivered, across every stream.
    total: Arc<AtomicU64>,
}

impl Webview {
    fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            decoder: Arc::new(Mutex::new(FrameDecoder::new())),
            seen: Arc::new(Mutex::new(BTreeMap::new())),
            total: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Hand it the client whose credit it returns.
    ///
    /// After construction, because the client cannot be built until the sink exists — the
    /// same order the window has, where the `Channel` comes from the invoke that opens the
    /// connection.
    fn acks_for(&self, client: &Client) {
        if let Ok(mut held) = self.client.lock() {
            *held = Some(client.clone());
        }
    }

    fn delivered(&self, stream: StreamId) -> u64 {
        self.seen
            .lock()
            .ok()
            .and_then(|seen| seen.get(&stream).copied())
            .unwrap_or_default()
    }

    fn delivered_total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
}

impl FrameSink for Webview {
    fn deliver(&self, bytes: Vec<u8>) -> Result<(), String> {
        let mut decoder = self.decoder.lock().map_err(|_| "poisoned decoder")?;
        decoder.push(&bytes);
        loop {
            let frame = match decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            };
            if frame.kind != FrameKind::Output {
                continue;
            }

            let count = frame.payload.len();
            if let Ok(mut seen) = self.seen.lock() {
                *seen.entry(frame.stream).or_default() += count as u64;
            }
            self.total.fetch_add(count as u64, Ordering::Relaxed);

            // The whole point of the sink. `terminal_ack` is what the webview invokes from
            // inside xterm's `write()` callback, and this is the same call.
            if let Ok(held) = self.client.lock()
                && let Some(client) = held.as_ref()
            {
                let _ = client.rendered(frame.stream, u32::try_from(count).unwrap_or(u32::MAX));
            }
        }
    }
}

/// The shell these tests drive, and the line that makes it write until it is stopped.
///
/// **One decision, returning both.** The profile and the flood line have to agree or the
/// test types one language's loop into another shell, which produces a syntax error and no
/// output at all — and then reads that as a credit stall, because a stall and a command that
/// never ran look identical from the sink. An earlier version of this file picked the line
/// by asking whether `pwsh` resolved while leaving the profile at the platform default, so
/// on any machine with PowerShell 7 and a POSIX login shell it did exactly that.
///
/// `pwsh` where §9 says it will be and CI has it, the platform's own shell where it is not,
/// so a machine without PowerShell 7 still runs this.
fn shell() -> (Option<ShellProfile>, &'static str) {
    if nysia_core::pty::resolve("pwsh").is_ok() {
        return (
            Some(ShellProfile::Pwsh),
            r#"while ($true) { Write-Output ("x" * 120) }"#,
        );
    }
    if cfg!(windows) {
        return (
            Some(ShellProfile::Cmd),
            "for /l %i in (1,1,100000000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        );
    }
    (
        None,
        "yes xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    )
}

/// Create a shell session on the daemon and return its handle.
fn open_shell(client: &Client) -> SessionHandle {
    let answer = client
        .request(RequestPayload::SessionCreate(SessionCreate {
            kind: SessionKind::Shell,
            pane_key: None,
            profile: shell().0,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 100,
            rows: 30,
        }))
        .expect("the daemon creates a session");
    match answer {
        ResponsePayload::SessionCreate(created) => created.handle,
        other => panic!(
            "session_create was answered with a {} payload",
            other.verb()
        ),
    }
}

/// Wait until the shell has drawn its prompt and gone quiet, before typing at it.
///
/// **Not a nicety.** A shell that has not finished starting echoes what is typed and then
/// redraws the line when its line editor takes over, so the command appears on screen twice
/// and runs zero times. That is not a flake either: on the macOS runner it lost the flood
/// line every time, and the test read 473 bytes of prompt where it expected a megabyte —
/// looking exactly like the credit stall it exists to catch, which is the worst way for a
/// test to fail.
///
/// An empty screen means the shell has written nothing yet; quiet alone would be satisfied
/// by the silence *before* it starts, so both conditions are needed and in this order.
fn await_prompt(client: &Client, handle: &SessionHandle) {
    let drew_something = eventually(|| !screen(client, handle).trim().is_empty());
    assert!(
        drew_something,
        "the shell never drew a prompt within {DEADLINE:?}, so there is nothing to type at"
    );
    settle(client, handle);
}

/// Wait for the session to stop producing output.
fn settle(client: &Client, handle: &SessionHandle) {
    let _ = client.request(RequestPayload::TerminalWait(TerminalWait {
        handle: handle.clone(),
        wait_for: WaitFor::Idle,
        timeout_ms: Some(DEADLINE_MS),
    }));
}

/// The session's rendered screen, as the CLI and hooks read it.
fn screen(client: &Client, handle: &SessionHandle) -> String {
    match client.request(RequestPayload::TerminalRead(TerminalRead::screen(
        handle.clone(),
    ))) {
        Ok(ResponsePayload::TerminalRead(read)) => read.lines.join(
            "
",
        ),
        _ => String::new(),
    }
}

fn type_line(client: &Client, handle: &SessionHandle, text: &str) {
    client
        .request(RequestPayload::TerminalSend(TerminalSend {
            handle: handle.clone(),
            text: text.to_owned(),
            enter: true,
            interrupt: false,
        }))
        .expect("the daemon accepts input");
}

/// Poll `check` until it holds or the deadline passes. `true` if it held.
fn eventually(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    check()
}

#[test]
fn the_window_reaches_a_daemon_bound_where_it_dials() {
    // **What this proves, exactly.** That a client dialling a `Listening` reaches a daemon
    // bound on it and completes the hello — the socket, the handshake, and the platform's
    // transport, end to end.
    //
    // What it does *not* prove is that the window resolves the right `Listening` in the
    // first place: the harness pins one through `Client::at`, which is the whole point of
    // that constructor and which bypasses `dial` and the resolver entirely. A deliberately
    // wrong resolver leaves every test in this module green. That claim belongs to
    // `state::tests::the_ordinary_constructor_dials_what_the_resolver_answers` and
    // `endpoint::tests::the_endpoint_is_the_one_both_halves_resolve`, which together close
    // the chain `Client::new` -> `dial` -> `endpoint()` -> `nysia-core`.
    let harness = Harness::start("hello");
    let client = harness.window(Webview::new());

    let identity = client.identity().expect("the window holds an identity");
    assert_eq!(
        identity.pid,
        harness.daemon.identity().pid,
        "the window shook hands with a daemon that is not the one under test"
    );
}

#[test]
fn output_reaches_the_webview_on_the_id_the_daemon_assigned() {
    let harness = Harness::start("route");
    let webview = Webview::new();
    let client = harness.window(webview.clone());
    webview.acks_for(&client);

    let handle = open_shell(&client);
    let stream = client
        .attach_session(handle.clone())
        .expect("the daemon routes the session");

    await_prompt(&client, &handle);
    let prompt = webview.delivered(stream);

    // Not any id: the one the daemon answered with. A client that numbered streams itself
    // would pass everything above this line and paint into the wrong pane here.
    //
    // Measured from after the prompt, so what is asserted is output this line caused. The
    // prompt alone would satisfy `> 0` and would have passed on a runner where the typed
    // line was lost entirely.
    type_line(&client, &handle, "echo NYSIA-INTEROP");
    assert!(
        eventually(|| webview.delivered(stream) > prompt),
        "nothing was delivered on stream {stream:?} within {DEADLINE:?}"
    );
    assert_eq!(
        webview.delivered_total(),
        webview.delivered(stream),
        "output arrived on an id the daemon never assigned to this session"
    );
}

#[test]
fn a_flood_keeps_flowing_past_the_per_stream_ceiling() {
    // **The proof that the credit exchange agrees.** The daemon spends its allowance as it
    // writes and stops at `per_stream_initial`; only the client's ack replenishes it. A
    // client that returned credit as a `Grant` — the shape the daemon ignores, because a
    // client does not own the window — got exactly this far and stopped, which is what was
    // measured at roughly 520 KB in three consecutive runs before this test existed.
    //
    // Past the ceiling rather than at it: crossing it at all is the whole claim, and a
    // margin that needed several round trips of replenishment is what separates "the
    // exchange works" from "the opening allowance was generous".
    let harness = Harness::start("flood");
    let webview = Webview::new();
    let client = harness.window(webview.clone());
    webview.acks_for(&client);

    let handle = open_shell(&client);
    let stream = client
        .attach_session(handle.clone())
        .expect("the daemon routes the session");

    await_prompt(&client, &handle);
    type_line(&client, &handle, shell().1);

    let ceiling = u64::from(CreditWindow::DEFAULT.per_stream_initial);
    let target = ceiling * 2;
    let flowed = eventually(|| webview.delivered(stream) >= target);
    let delivered = webview.delivered(stream);
    assert!(
        flowed,
        "the flood stalled at {delivered} bytes, short of {target}; the per-stream ceiling is \
         {ceiling}, so the client's credit is not replenishing the daemon's allowance"
    );
}

/// The flagship path: a webview reload, against the real daemon, end to end.
///
/// It was `#[ignore]`d for two rounds while the daemon half caught up, and it is worth
/// recording what it took, because neither half alone was enough. The daemon now binds a
/// stream connection **before** it answers that connection's hello, so an attach issued the
/// instant a reload's connect returns cannot be served on the connection it supersedes. And
/// this client claims the stream generation before it opens the socket, so the reader the
/// reload abandons cannot reach end-of-file still holding the live generation and take the
/// healthy control connection with it.
///
/// Measured with the second half reverted and a 200 ms gap in its place — the shape a
/// control round trip from the dying webview actually produces — this failed five runs in
/// five, with the pane silent while the window said ready.
#[test]
fn a_reload_reattaches_over_a_second_stream_connection() {
    // What a webview reload is, against the real daemon: the control connection stays, a
    // second stream connection is opened under the same client id, every id learned on the
    // first is void, and the sessions attach again. The daemon has to accept the second
    // connection as the same client, and the ids it hands out have to route to the new sink.
    let harness = Harness::start("reload");
    let first = Webview::new();
    let client = harness.window(first.clone());
    first.acks_for(&client);

    let handle = open_shell(&client);
    let before = client
        .attach_session(handle.clone())
        .expect("attached once");

    // The reload. `connect` short-circuits — the control socket is already held — and only
    // the stream connection is replaced, which is the case the reader's generation exists
    // for.
    let second = Webview::new();
    client
        .attach_channel(second.clone())
        .expect("a second stream connection opens under the same client id");
    second.acks_for(&client);

    let after = client
        .attach_session(handle.clone())
        .expect("the session attaches again on the new connection");
    assert_ne!(
        client.identity(),
        None,
        "the reload tore down a control connection that was working"
    );

    await_prompt(&client, &handle);
    type_line(&client, &handle, "echo NYSIA-RELOADED");
    assert!(
        eventually(|| second.delivered(after) > 0),
        "nothing reached the pane after the reload; it was attached as {after:?} \
         (it held {before:?} before)"
    );
}

/// The isolation this module's honesty rests on.
///
/// If the scratch endpoint ever collided with the real one, these tests would bind over a
/// daemon somebody is using and pass while doing it — on a developer's machine and on both
/// CI legs, where the two runners share a filesystem namespace per platform.
#[test]
fn the_scratch_endpoint_is_not_the_one_a_real_daemon_uses() {
    let mine = scratch("isolation");
    let real = Endpoint::from_env().expect("this environment resolves an endpoint");
    assert_ne!(
        mine.listening(),
        real.listening(),
        "the interop daemon would bind over a daemon somebody is using"
    );

    // And two harnesses in one run must not collide with each other either: the tests run on
    // threads of one binary, and a shared name would have the second `bind` fail.
    assert_ne!(scratch("isolation").listening(), mine.listening());
}
