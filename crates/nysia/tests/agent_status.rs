//! The v0.2 acceptance test: **a hook fires inside a session and the pane's dot changes**.
//!
//! # It is expected to fail, and that is the point
//!
//! Nothing behind this exists yet. `nysia hook` is a CLI stub that reports itself
//! unimplemented, `nysia agent status` is not a verb at all, and there is no ingest, no
//! store and no status RPC for either of them to reach. So the test is `#[ignore]`d with a
//! reason naming the wave that lands them — `docs/plans/v0.2-delivery-plan.md` **wave C, W4**
//! — and `cargo test --workspace` stays green with it in the tree.
//!
//! It is written now, in wave A, because of the single worst finding of v0.1: the daemon and
//! the Tauri client were each green and did not interoperate, and only binding a real daemon
//! to the real client found it. Three later PRs can each be green against their own unit
//! tests and still not add up to a working dot. This is the thing they are measured against,
//! and writing it after them would be writing it to fit them.
//!
//! **What it proves when it passes**, and none of which any unit test can:
//!
//! 1. `nysia hook` reads a payload on stdin and reaches the daemon over the socket.
//! 2. It prints `{}` **first**, so it can never block or influence the agent (§5.2).
//! 3. The daemon maps `Stop` to `done` (§2.1), attributes it to the right pane, and keeps
//!    it — a fresh client process, connecting after the hook's process is gone, reads it.
//! 4. The CLI can say so in JSON, which is what the window and the phone read too.
//!
//! # Two things it does differently from the sentence in the plan, deliberately
//!
//! **The session is a shell, not an agent.** `nysia session create` has no `--kind agent`
//! today, and a real agent session would need a `claude` on PATH and credentials that no CI
//! runner has — which would make this a test that skips on both legs, and a test that skips
//! is a test that is not run. Nothing is lost: the status row is keyed by [`PaneKey`] and
//! §2.2 puts no session kind in it, so the path this drives is the same one an agent drives.
//! What a real agent adds is hook *installation*, which is W2's, has its own gate, and is not
//! what this test is for.
//!
//! **The hook runs inside the session**, through `terminal send`, rather than being spawned
//! by this file. That is not convenience, it is the only version of the test that can ever
//! pass against a correct daemon. §3.2: pane identity is proven from socket peer credentials
//! and PTY process-tree ancestry, and `NYSIA_PANE_KEY` is a hint for speed and never the
//! proof. A `nysia hook` this file spawned is a child of the test harness, in no session's
//! process tree, and a daemon that accepted it would be one that lets any process on the
//! machine write status into any pane. Driving it from inside the session is what makes the
//! ancestry real, and it is the reason the payload goes through a file: three shells, three
//! quoting dialects, and none of them have to survive a JSON document on their command line.
//!
//! Modelled on `survival.rs`, which is what made v0.1 real, down to its bounded waits — a
//! test that can hang is a test that will, and a hung integration test takes the runner with
//! it. Its harness is copied rather than shared: `survival.rs` is another wave's file and
//! extracting a common module would be an edit outside this task's owned paths.
//!
//! # Read this before you trust a red — the pwsh line has never run
//!
//! [`hook_command`] picks one of three shells, and **the `pwsh` branch has been executed by
//! nobody.** It was written on a machine without PowerShell 7, so every run of this test so
//! far took the `cmd` branch — and `pwsh` is the branch **both CI legs take** the moment the
//! `#[ignore]` comes off, because that is the profile `session create` resolves when it is
//! installed. Only that one line is unproven: `survival.rs` already drives `--profile pwsh`
//! on both runners, so the profile, the spawn and the prompt wait are all exercised; what is
//! not is `Get-Content -Raw '<payload>' | & '<nysia>' hook …`, the one spelling of feeding
//! the payload in that PowerShell needs because it has no `<` operator.
//!
//! So, for whoever removes the `#[ignore]`: **run this with `--ignored` on a machine that
//! has `pwsh` before reading a red as a feature bug.**
//!
//! ```text
//! cargo test -p nysia --test agent_status -- --ignored --nocapture
//! ```
//!
//! The failure message prints the pane's screen, which is where a harness bug shows itself:
//! a quoting or redirection fault appears there as a PowerShell parser error against the
//! command line, where a missing feature appears as `nysia hook` answering for itself. If it
//! is the harness, the fix is in this file and not in yours.
//!
//! [`PaneKey`]: nysia_proto::PaneKey

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
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest to wait for the daemon to start answering.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// The hook payload, exactly as Claude writes one to stdin.
///
/// Snake_case, and carrying more than the daemon needs — `session_id`, `transcript_path`,
/// `cwd` — because that is what actually arrives and a payload trimmed to what this test
/// asserts on would not prove the extra fields are tolerated.
const STOP_PAYLOAD: &str = r#"{"session_id":"acceptance","transcript_path":"/dev/null","cwd":".","hook_event_name":"Stop","is_interrupt":false}"#;

/// What §5.2 says the hook prints before it does anything else.
const EMPTY_DECISION: &str = "{}";

/// One CLI invocation's result.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Run `nysia <args>` against `runtime_dir` and wait for it, bounded.
    ///
    /// `--no-spawn` on every client verb: the daemon under test is the one this file
    /// started, and a verb that quietly started a second one would prove nothing about it.
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

        // Both pipes are drained on their own threads and delivered through channels, so the
        // wait below is bounded in both directions. Joining them instead would hang for
        // exactly the case this guards: a daemon that inherited the pipe and never let go.
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
            std::env::temp_dir().join(format!("nysia-agent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        // `unwrap_or_else`, not `expect`: clippy's allow-expect-in-tests covers `#[test]`
        // functions and `#[cfg(test)]` modules, and this is neither.
        std::fs::create_dir_all(&runtime_dir)
            .unwrap_or_else(|err| panic!("could not make a runtime directory: {err}"));

        // `--no-idle-retire` so the daemon cannot decide it is unwanted during a pause on a
        // slow runner.
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

/// The shell to drive, and the line that makes it run `nysia hook` with `payload` on stdin.
///
/// `pwsh` where it is installed — what §9 names and what both CI runners have — and the
/// platform's own shell where it is not. Three dialects because there is no redirection
/// spelling all three share: PowerShell has no `<` operator at all (it is reserved and
/// unimplemented), so it pipes the file in instead.
///
/// The payload never appears on the command line. A JSON document through three quoting
/// dialects is a test that fails for reasons that have nothing to do with status.
///
/// **The `pwsh` line below has never been executed** — see the module docs. It is the line
/// both CI legs will take, and it is the only part of this harness that no run has covered.
fn hook_command(payload: &Path) -> (Vec<&'static str>, String) {
    let payload = payload.display();
    if nysia_core::pty::resolve("pwsh").is_ok() {
        // Untried. `Get-Content -Raw` because PowerShell has no `<` operator — it is
        // reserved and unimplemented — so the file cannot be redirected in the way the other
        // two shells do it.
        return (
            vec!["--profile", "pwsh"],
            format!("Get-Content -Raw '{payload}' | & '{NYSIA}' hook --event Stop --no-spawn"),
        );
    }
    if cfg!(windows) {
        return (
            vec!["--profile", "cmd"],
            format!("\"{NYSIA}\" hook --event Stop --no-spawn < \"{payload}\""),
        );
    }
    (
        Vec::new(),
        format!("'{NYSIA}' hook --event Stop --no-spawn < '{payload}'"),
    )
}

/// Wait until the shell has drawn its prompt and gone quiet.
///
/// Typing before this does not sometimes lose, it reliably loses: a shell that has not
/// finished starting echoes what is typed at it and then redraws the line when its line
/// editor takes over, so the command appears twice and runs zero times.
fn await_prompt(daemon: &Nysiad, handle: &str) {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let run = daemon.run(&["terminal", "read", handle, "--no-spawn"]);
        if !run.stdout.trim().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shell never painted anything: {}{}",
            run.stdout,
            run.stderr
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    settle(daemon, handle);
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

/// One field of a `--json` answer, or a panic naming what was printed instead.
fn field(run: &Run, what: &str, key: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(run.stdout.trim())
        .unwrap_or_else(|err| panic!("`{what}` did not print JSON: {err}\n{}", run.stdout));
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("`{what}` should answer with {key}, got {value}"))
        .to_owned()
}

/// The `state` `nysia agent status --json` reports for `pane`, or `None` for every way it
/// could fail to report one — including not being a verb yet.
///
/// Total rather than asserting as it goes, so the one assertion in the test carries the
/// whole story: today this returns `None` because the CLI refuses the argv, and the failure
/// message has to say that rather than panicking inside a JSON parser.
///
/// The shape it expects is the shape `session list --json` already uses: a bare array on
/// stdout, one entry per pane the daemon holds a status for, each a `AgentStatus` — so
/// `lead.pane` names the pane and `lead.state` is its dot. Pinning one spelling is the
/// point; a test that accepted two would measure neither.
fn reported_state(run: &Run, pane: &str) -> Option<String> {
    let statuses: serde_json::Value = serde_json::from_str(run.stdout.trim()).ok()?;
    statuses
        .as_array()?
        .iter()
        .find(|status| status["lead"]["pane"].as_str() == Some(pane))?["lead"]["state"]
        .as_str()
        .map(str::to_owned)
}

#[test]
#[ignore = "fails until v0.2 wave C (W4): `nysia hook`, the daemon ingest and the status RPC"]
fn a_stop_hook_fired_inside_a_session_turns_that_panes_dot_done() {
    let daemon = Nysiad::start("w1");

    // The payload goes to a file rather than through the shell's quoting.
    let payload = daemon.runtime_dir.join("stop-hook.json");
    std::fs::write(&payload, STOP_PAYLOAD)
        .unwrap_or_else(|err| panic!("could not write the hook payload: {err}"));
    let (profile, hook_line) = hook_command(&payload);

    let mut create = vec!["session", "create", "--json", "--no-spawn"];
    create.extend_from_slice(&profile);
    let created = daemon.run(&create).ok("session create");
    let handle = field(&created, "session create --json", "handle");
    // The pane key, not the handle: §2.2 makes the pane the row's identity, and it is what
    // survives the session that reported the status.
    let pane = field(&created, "session create --json", "paneKey");
    await_prompt(&daemon, &handle);

    // The hook runs *in the pane*, so the daemon can prove which pane it belongs to from the
    // process tree (§3.2) rather than from anything the caller said.
    daemon
        .run(&[
            "terminal",
            "send",
            &handle,
            "--text",
            &hook_line,
            "--enter",
            "--no-spawn",
        ])
        .ok("terminal send");
    settle(&daemon, &handle);
    let screen = daemon
        .run(&["terminal", "read", &handle, "--no-spawn"])
        .ok("terminal read")
        .stdout;

    // A *fresh* client process. The hook's own process is gone by now, and so is the client
    // that sent the line: what is being read is what the daemon kept.
    let status = daemon.run(&["agent", "status", "--json", "--no-spawn"]);

    // The one assertion the whole file exists for. Its message carries the pane's screen as
    // well as the answer, because until wave C lands the interesting half of the failure is
    // what the hook printed, not what the status verb did not.
    assert_eq!(
        reported_state(&status, &pane).as_deref(),
        Some("done"),
        "`nysia agent status --json` should report `done` for {pane}.\n\
         exit: {:?}\nstdout: {:?}\nstderr: {:?}\nthe pane's screen after the hook ran:\n{screen}",
        status.code,
        status.stdout,
        status.stderr
    );

    // §5.2: the hook is on the agent's critical path, so it answers before it does anything
    // else and can never block or influence the agent. The pane's own screen is the only
    // place that is observable, which is another reason the hook runs inside the session.
    assert!(
        screen.contains(EMPTY_DECISION),
        "the hook must print {EMPTY_DECISION} before anything else; the pane showed:\n{screen}"
    );

    // Close before the daemon is killed: on Unix a killed daemon leaves its shell orphaned,
    // and a test that leaks a process is a test that fails the *next* run.
    daemon
        .run(&["session", "close", &handle, "--no-spawn"])
        .ok("session close");
}
