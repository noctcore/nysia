//! The D-1 acceptance test: **kill the client and the session survives**.
//!
//! This is the deliverable the rest of the daemon exists to make true, and the plan's
//! acceptance sentence in full: *kill the UI and the shell survives; reattach and the
//! scrollback replays.* It runs CLI-only, with no GUI anywhere near it.
//!
//! What it does, and why each step is shaped the way it is:
//!
//! 1. Starts `nysia --daemon` as a child this test owns, on its own endpoint. Owning it is
//!    what makes the teardown deterministic; isolating the endpoint is what lets the test run
//!    beside a developer's real daemon — and on Windows the pipe namespace is machine-global,
//!    so `NYSIA_RUNTIME_DIR` has to reach the pipe *name*, which it does.
//! 2. Creates a session, and makes the shell **compute** a token. Computed, never typed: a
//!    test that asserts on a string which also appears in the line as typed is satisfied by
//!    kernel echo, with the shell having run nothing at all.
//! 3. **Disconnects every client.** Each `nysia` verb is its own process, so by the time the
//!    command returns, its socket is closed and the daemon has zero clients. Nothing is asked
//!    of the daemon between then and step 4.
//! 4. Starts a *fresh* client process and asserts the screen still shows the token, and that
//!    `--stream` from the start of the log still shows it too. The first is "the session
//!    survived"; the second is "the scrollback replays".
//!
//! Every wait is bounded. A test that can hang is a test that will, and a hung integration
//! test takes the CI runner with it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The binary cargo just built for this test target.
const NYSIA: &str = env!("CARGO_BIN_EXE_nysia");

/// The longest any single step may take.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest to wait for a command's output *after* it has exited.
///
/// Its own, much shorter budget, because the only thing that makes this elapse is a child of
/// the command still holding the pipe open — which is a specific bug with a specific cause,
/// and one this test has already caught once. See [`Run::capture`].
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest to wait for the daemon to start answering.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// The token the shell computes. It appears in no line that is typed.
const TOKEN: &str = "NYSIA-42";

/// One CLI invocation's result.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Run `nysia <args>` against `runtime_dir` and wait for it, bounded.
    ///
    /// Every client verb is given `--no-spawn`: the daemon under test is the one this file
    /// started, and a verb that quietly started a second one would make the test prove
    /// nothing about the first.
    fn capture(runtime_dir: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(NYSIA)
            .args(args)
            .env("NYSIA_RUNTIME_DIR", runtime_dir)
            .env("NYSIA_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("could not run {NYSIA} {args:?}: {err}"));

        // The two pipes are drained on their own threads and delivered through channels, so
        // the wait below is bounded in both directions. Joining them instead would hang for
        // exactly the case this guards: a daemon that inherited the pipe and never lets go.
        let stdout = drain(child.stdout.take());
        let stderr = child.stderr.take().map(|pipe| drain(Some(pipe)));

        let code = wait_for(&mut child, STEP_TIMEOUT).unwrap_or_else(|| {
            let _ = child.kill();
            panic!(
                "`nysia {}` did not finish within {STEP_TIMEOUT:?}",
                args.join(" ")
            )
        });

        let stdout = stdout.recv_timeout(DRAIN_TIMEOUT).unwrap_or_else(|_| {
            panic!(
                "`nysia {}` exited but its stdout never closed; something it spawned is \
                 holding the pipe open",
                args.join(" ")
            )
        });
        let stderr = stderr
            .and_then(|rx| rx.recv_timeout(DRAIN_TIMEOUT).ok())
            .unwrap_or_default();
        Self {
            code,
            stdout,
            stderr,
        }
    }

    /// The run, asserting it succeeded.
    fn ok(self, what: &str) -> Self {
        assert_eq!(
            self.code,
            Some(0),
            "{what} failed ({:?}): {}{}",
            self.code,
            self.stdout,
            self.stderr
        );
        self
    }
}

/// Read a pipe on its own thread, delivering the whole of it through a channel.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            let _ = tx.send(text);
        });
    } else {
        let _ = tx.send(String::new());
    }
    rx
}

/// Wait for `child`, giving up after `timeout`.
fn wait_for(child: &mut Child, timeout: Duration) -> Option<Option<i32>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

/// The daemon under test, killed when the test ends however it ends.
struct Nysiad {
    child: Child,
    runtime_dir: PathBuf,
}

impl Nysiad {
    /// Start a daemon on its own endpoint and wait until it answers.
    fn start(tag: &str) -> Self {
        let runtime_dir =
            std::env::temp_dir().join(format!("nysia-survival-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        // `unwrap_or_else`, not `expect`: clippy's allow-expect-in-tests covers `#[test]`
        // functions and `#[cfg(test)]` modules, and this is neither.
        std::fs::create_dir_all(&runtime_dir)
            .unwrap_or_else(|err| panic!("could not make a runtime directory: {err}"));

        // `--no-idle-retire` so the daemon cannot decide it is unwanted during a pause on a
        // slow runner. Idle retire has its own tests; this one is about survival.
        let child = Command::new(NYSIA)
            .args(["--daemon", "--no-idle-retire"])
            .env("NYSIA_RUNTIME_DIR", &runtime_dir)
            .env("NYSIA_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|err| panic!("could not start the daemon: {err}"));

        let daemon = Self { child, runtime_dir };
        daemon.await_ready();
        daemon
    }

    /// Poll until the daemon answers a verb, or give up.
    fn await_ready(&self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let run = Run::capture(&self.runtime_dir, &["session", "list", "--no-spawn"]);
            if run.code == Some(0) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon never began answering within {READY_TIMEOUT:?}: {}",
                run.stderr
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Run a client verb against this daemon.
    fn run(&self, args: &[&str]) -> Run {
        Run::capture(&self.runtime_dir, args)
    }
}

impl Drop for Nysiad {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// The shell to drive, and the lines that make it compute [`TOKEN`].
///
/// `pwsh` where it is installed — which is what §9 names and what both CI runners have — and
/// the platform's own shell where it is not, so a developer machine without PowerShell 7
/// still runs the test rather than skipping the one that matters most.
fn shell() -> (Vec<&'static str>, Vec<&'static str>) {
    if nysia_core::pty::resolve("pwsh").is_ok() {
        return (
            vec!["--profile", "pwsh"],
            vec![r#"Write-Output ("NYSIA" + "-" + (6*7))"#],
        );
    }
    if cfg!(windows) {
        // `cmd` expands `%NYS%` when it parses the line, so the assignment has to be its own
        // command — which also keeps `42` out of everything that is typed.
        return (
            vec!["--profile", "cmd"],
            vec!["set /a NYS=6*7", "echo NYSIA-%NYS%"],
        );
    }
    (Vec::new(), vec![r#"echo "NYSIA-$((6*7))""#])
}

/// Wait for the shell to stop producing output, through the daemon's own `wait --for idle`.
fn settle(daemon: &Nysiad, handle: &str) {
    daemon.run(&[
        "terminal",
        "wait",
        handle,
        "--for",
        "idle",
        "--timeout-ms",
        "15000",
        "--no-spawn",
    ]);
}

/// Read the screen until it shows `TOKEN`, using a fresh client process every time.
fn read_until_token(daemon: &Nysiad, handle: &str) -> String {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let run = daemon.run(&["terminal", "read", handle, "--no-spawn"]);
        if run.stdout.contains(TOKEN) || Instant::now() >= deadline {
            return run.stdout;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The handle out of a `--json` create.
fn handle_from(run: &Run) -> String {
    let value: serde_json::Value = serde_json::from_str(run.stdout.trim()).unwrap_or_else(|err| {
        panic!(
            "`session create --json` did not print JSON: {err}\n{}",
            run.stdout
        )
    });
    value
        .get("handle")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("a create answers with a handle, got {value}"))
        .to_owned()
}

#[test]
fn a_session_survives_losing_every_client_and_its_scrollback_replays() {
    let daemon = Nysiad::start("d1");
    let (profile, lines) = shell();

    let mut create = vec!["session", "create", "--json", "--no-spawn"];
    create.extend_from_slice(&profile);
    let created = daemon.run(&create).ok("session create");
    let handle = handle_from(&created);

    // Every verb below is its own process. By the time each returns, its connection to the
    // daemon is closed — which is the disconnect this test is about, happening over and over.
    for line in lines {
        daemon
            .run(&[
                "terminal",
                "send",
                &handle,
                "--text",
                line,
                "--enter",
                "--no-spawn",
            ])
            .ok("terminal send");
        // Settle between lines rather than wait for the token: only the *last* line produces
        // it, so waiting for it after the first would burn the whole step budget on a
        // condition that cannot be true yet. `cmd` also needs the assignment to have run
        // before it parses the line that expands it.
        settle(&daemon, &handle);
    }
    let before = read_until_token(&daemon, &handle);
    assert!(
        before.contains(TOKEN),
        "the shell should have computed {TOKEN}, got {before:?}"
    );

    // Nothing is connected now: the last client process has exited. Wait long enough that a
    // daemon which tore sessions down when its last client left would have done so.
    std::thread::sleep(Duration::from_secs(2));
    let sessions = daemon
        .run(&["session", "list", "--json", "--no-spawn"])
        .ok("session list");
    let rows: serde_json::Value =
        serde_json::from_str(sessions.stdout.trim()).expect("`session list --json` prints JSON");
    assert_eq!(
        rows.as_array().map(Vec::len),
        Some(1),
        "the session should have outlived every client, got {rows}"
    );

    // A *fresh* client. This is the reattach half.
    let screen = daemon
        .run(&["terminal", "read", &handle, "--screen", "--no-spawn"])
        .ok("terminal read --screen");
    assert!(
        screen.stdout.contains(TOKEN),
        "a fresh client's `terminal read --screen` should still show {TOKEN}, got {:?}",
        screen.stdout
    );

    // And the scrollback, not merely the visible grid.
    let scrollback = daemon
        .run(&[
            "terminal",
            "read",
            &handle,
            "--stream",
            "--cursor",
            "0",
            "--no-spawn",
        ])
        .ok("terminal read --stream");
    assert!(
        scrollback.stdout.contains(TOKEN),
        "the scrollback should have replayed intact, got {:?}",
        scrollback.stdout
    );

    // The session is still *live*, not merely readable: it takes input and the child exits
    // with the code it was told to.
    daemon
        .run(&[
            "terminal",
            "send",
            &handle,
            "--text",
            "exit 7",
            "--enter",
            "--no-spawn",
        ])
        .ok("terminal send");
    let waited = daemon
        .run(&[
            "terminal",
            "wait",
            &handle,
            "--for",
            "exit",
            "--timeout-ms",
            "60000",
            "--json",
            "--no-spawn",
        ])
        .ok("terminal wait");
    let outcome: serde_json::Value =
        serde_json::from_str(waited.stdout.trim()).expect("`terminal wait --json` prints JSON");
    // The outcome is flattened in, and carries the child's status inside it: `outcome` says
    // the wait ended in an exit, `status` says what that exit was.
    assert_eq!(outcome["outcome"], "exited", "got {outcome}");
    assert_eq!(outcome["status"]["code"], 7, "got {outcome}");

    // Close before the daemon is killed: on Unix a killed daemon leaves its shell orphaned,
    // and a test that leaks a process is a test that fails the *next* run.
    daemon
        .run(&["session", "close", &handle, "--no-spawn"])
        .ok("session close");
}

#[test]
fn a_verb_starts_a_daemon_when_none_is_listening_and_still_returns() {
    // Two things at once, and the second is a regression test with a scar.
    //
    // Spawn-if-absent has to work — a verb that needs a daemon should not make the caller
    // start one. And the command has to *return*: on Windows a redirected spawn inherits every
    // inheritable handle, not only the three that were named, so a daemon started from inside
    // `H=$(nysia session create)` once held that pipe open for its whole life and the shell
    // waited forever for output it already had. `Run::capture` reads through a channel with
    // its own deadline precisely so that failure is reported rather than hung on.
    let runtime_dir = std::env::temp_dir().join(format!("nysia-spawn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("a runtime directory");

    let mut create = vec!["session", "create", "--json"];
    let (profile, _) = shell();
    create.extend_from_slice(&profile);
    let created = Run::capture(&runtime_dir, &create).ok("session create with no daemon running");
    let handle = handle_from(&created);

    let listed = Run::capture(&runtime_dir, &["session", "list", "--json"]).ok("session list");
    assert!(listed.stdout.contains(&handle), "got {:?}", listed.stdout);

    // A second verb must find the daemon that is already there rather than starting another.
    Run::capture(&runtime_dir, &["session", "close", &handle]).ok("session close");

    // The daemon this test started is not a child it can kill by handle, so it is found the
    // way any other tool would find it: the lease beside the endpoint. Leaving it to retire on
    // its own would keep the log file open for minutes, and a `remove_dir_all` against an open
    // file fails silently on Windows — which is how a later run inherits a directory it
    // thought was fresh.
    stop_daemon(&runtime_dir);
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

/// Kill the daemon described by the lease in `runtime_dir`, if one is there.
fn stop_daemon(runtime_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(runtime_dir.join("nysiad-v1.pid.json")) else {
        return;
    };
    let Ok(record) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let Some(pid) = record.get("pid").and_then(serde_json::Value::as_u64) else {
        return;
    };
    let pid = pid.to_string();
    let killed = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-TERM", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    let _ = killed;
    // Give the kernel a moment to release the log file before the directory is removed.
    std::thread::sleep(Duration::from_millis(300));
}

#[test]
fn a_client_told_not_to_spawn_says_how_to_start_a_daemon() {
    let runtime_dir = std::env::temp_dir().join(format!("nysia-nospawn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("a runtime directory");

    let run = Run::capture(&runtime_dir, &["session", "list", "--no-spawn", "--json"]);
    assert_eq!(run.code, Some(1), "it should fail, not succeed emptily");
    assert!(
        run.stdout.trim().is_empty(),
        "the result stream must stay empty on failure, got {:?}",
        run.stdout
    );

    // §6.2: every error carries next steps, and the JSON form carries them machine-readably.
    let envelope: serde_json::Value =
        serde_json::from_str(run.stderr.trim()).unwrap_or_else(|err| {
            panic!(
                "--json should print an error envelope: {err}\n{}",
                run.stderr
            )
        });
    let steps = envelope["nextSteps"]
        .as_array()
        .unwrap_or_else(|| panic!("an error envelope carries next steps, got {envelope}"));
    assert!(!steps.is_empty());
    assert_eq!(
        envelope["nextCommandArgs"][0], "nysia",
        "and the argv of the command that fixes it, got {envelope}"
    );

    let _ = std::fs::remove_dir_all(&runtime_dir);
}
