//! The v0.3 acceptance test: **a registered folder is still there, under the same id,
//! after the daemon restarts**.
//!
//! # It is written to fail, and that is the point
//!
//! Nothing behind it exists yet. `nysia project` is not a verb, there is no projects table,
//! and `crates/nysia-core/src/rpc/server.rs` answers the three project verbs with
//! `unsupported` because wave A put them on the wire and nothing serves them. So this is
//! `#[ignore]`d with a reason naming **v0.3 wave C1** — `docs/plans/v0.3-delivery-plan.md`
//! §4 — and `cargo test --workspace` stays green with it in the tree. **It is expected to
//! fail until C1 lands.** When C1 removes the `#[ignore]`, nothing else here should move:
//! a test edited to fit the code it grades is a test that grades nothing.
//!
//! It is written now, in wave A, for the reason `survival.rs` and `agent_status.rs` were.
//! Three later PRs can each be green against their own unit tests and still not add up to a
//! project that survives a restart, and a test written after them would be written to fit
//! them.
//!
//! **What it proves when it passes**, and none of which any unit test can:
//!
//! 1. `nysia project register <path>` reaches the daemon and it accepts a git repository.
//! 2. `nysia project list --json` reports it — a bare array on stdout, one object per
//!    project, which is the shape `session list --json` already uses.
//! 3. The id it reports is the one `ProjectId::from_canonical_path` derives from the
//!    folder's canonical path, so the identity rule is the daemon's rule and not just
//!    `nysia-proto`'s. Persistence alone would not show this: a daemon that minted a random
//!    id and stored it would survive a restart perfectly and reorder the sidebar on the
//!    first machine that registered the same folder twice.
//! 4. A **second daemon process**, started on the same runtime directory after the first
//!    was killed, reports the same project under the same id. That is the whole of §3.3:
//!    a project that was registered must still be there after a restart.
//!
//! # Read this before you trust a red
//!
//! A red here is either "the feature is absent", which is what it says today, or "the
//! harness is broken", and only one of those is worth keeping. The failure message carries
//! all four runs — both `project` verbs, on both daemons — so the two are tellable apart
//! without rerunning anything:
//!
//! - **The feature is absent** looks like clap refusing the argv: `unrecognized subcommand
//!   'project'`, exit 2, nothing on stdout. That is the expected red.
//! - **The harness is broken** looks like the daemon never answering, `git` not resolving,
//!   or the second daemon failing to come up on the runtime directory the first one left
//!   behind. Those are this file's bugs and the fix is in this file.
//!
//! The steps before the restart are captured rather than asserted, so the restart happens
//! on every run — including today's. A `.ok()` on the register step would mean the half of
//! the harness that matters most had never executed until the day the feature landed.
//!
//! **Which `git` this run drove is announced** on the process's real stderr, green runs
//! included. libtest captures `print!`/`eprintln!` per test thread and shows it only when a
//! test fails, so it says nothing on exactly the runs somebody needs to read. Wave A's
//! warning to v0.2's wave C was about a shell nobody could tell had been chosen; this
//! harness chooses a `git` instead, and the same rule applies:
//!
//! ```text
//! projects: driving git at C:\Program Files\Git\cmd\git.exe (git version 2.51.0.windows.1)
//! ```
//!
//! ```text
//! cargo test -p nysia --test projects -- --ignored --nocapture
//! ```

use std::ffi::OsStr;
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

/// The longest to wait for a daemon to start answering. Both of them.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// The folder that gets registered, under the daemon's runtime directory.
///
/// Inside the runtime directory so that [`Nysiad`]'s `Drop` takes the repository with it:
/// a test that leaves a git repository in the temp directory of every CI runner is a test
/// that fills one.
const REPOSITORY: &str = "acceptance-repo";

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
            std::env::temp_dir().join(format!("nysia-projects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        // `unwrap_or_else`, not `expect`: clippy's allow-expect-in-tests covers `#[test]`
        // functions and `#[cfg(test)]` modules, and this is neither.
        std::fs::create_dir_all(&runtime_dir)
            .unwrap_or_else(|err| panic!("could not make a runtime directory: {err}"));

        let child = spawn_daemon(&runtime_dir);
        let daemon = Self { child, runtime_dir };
        daemon.await_ready();
        daemon
    }

    /// Kill this daemon and bring a fresh process up on the same runtime directory.
    ///
    /// **The same directory is the whole test.** The store is `<runtime_dir>/nysia.db`
    /// (`rpc::server::STORE_FILE`), so a second daemon here is the same daemon's state and
    /// a different process — which is what "survives a restart" means. Removing the
    /// directory, as [`Nysiad::start`] does, would be starting over and would pass with no
    /// persistence at all.
    ///
    /// The kill is abrupt on purpose: a daemon that only keeps a project because it was
    /// asked to shut down politely has not persisted it, it has flushed it. The endpoint
    /// left behind is not a problem for the replacement — `rpc::discovery` decides a lease
    /// is stale by trying to reach what it names, never by the pid in it.
    /// It carries its own proof that it is a restart (traps register #12). With the first
    /// daemon killed and reaped and the second not yet started, a client verb has nothing
    /// to answer it — so if this step succeeds, something outlived the kill and the
    /// `await_ready` below would be satisfied by the daemon that was supposed to have gone.
    /// A test that reported persistence it never measured would be worse than no test, and
    /// this half runs today, on a build where the project verbs do not exist.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();

        let orphaned = Run::capture(&self.runtime_dir, &["session", "list", "--no-spawn"]);
        assert_ne!(
            orphaned.code,
            Some(0),
            "the daemon was killed and something is still answering on its endpoint, so \
             what follows would not be a restart: {} {}",
            orphaned.stdout.trim(),
            orphaned.stderr.trim()
        );

        self.child = spawn_daemon(&self.runtime_dir);
        self.await_ready();
    }

    /// Poll until the daemon answers a verb, or give up.
    ///
    /// `session list` rather than a project verb: this has to answer "is a daemon up" on a
    /// build where the project verbs are refused, and a probe that cannot tell "not ready"
    /// from "not implemented" would spin for forty-five seconds before every assertion.
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

/// Start `nysia --daemon` on `runtime_dir`.
///
/// `--no-idle-retire` so the daemon cannot decide it is unwanted during a pause on a slow
/// runner — which, in a test whose middle step is killing one on purpose, would be a
/// failure nobody could tell from the one being measured.
fn spawn_daemon(runtime_dir: &Path) -> Child {
    Command::new(NYSIA)
        .args(["--daemon", "--no-idle-retire"])
        .env("NYSIA_RUNTIME_DIR", runtime_dir)
        .env("NYSIA_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|err| panic!("could not start the daemon: {err}"))
}

/// The `git` this run drives, announced on the process's real stderr.
///
/// **Not `eprintln!`**, and the difference is the whole point. libtest captures the
/// `print!` family per test thread and shows it only when a test *fails*, so an `eprintln!`
/// here would say nothing on exactly the runs somebody needs to read. A direct write to
/// [`std::io::stderr`] does not go through that capture.
///
/// Resolved through `nysia_core::pty::resolve` rather than by handing `"git"` to
/// [`Command`], because on Windows `CreateProcess` launches real executables and not the
/// shims a package manager writes (traps register #8). `git` is a real `.exe` on both
/// platforms, so this is belt-and-braces — but a harness that resolves programs one way
/// while the daemon resolves them another is a harness that finds a different git.
fn announce_git() -> nysia_core::pty::ResolvedProgram {
    use std::io::Write;

    let git = nysia_core::pty::resolve("git").unwrap_or_else(|err| {
        panic!("this test needs `git` on PATH and could not resolve one: {err}")
    });
    let version = git_output(&git, Path::new("."), &["--version"]);
    let _ = writeln!(
        std::io::stderr(),
        "projects: driving git at {} ({})",
        git.program.display(),
        version.trim()
    );
    git
}

/// Run `git <args>` in `cwd`, returning its stdout and panicking if it failed.
///
/// A `git` that will not run is a harness failure, not a finding about Nysia, so it says so
/// in those words rather than surfacing three steps later as an empty project list.
fn git_output(git: &nysia_core::pty::ResolvedProgram, cwd: &Path, args: &[&str]) -> String {
    let leading: Vec<&OsStr> = git.leading_args.iter().map(OsStr::new).collect();
    let output = Command::new(&git.program)
        .args(&leading)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("harness: could not run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "harness: git {args:?} failed ({:?})\n{}{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A git repository with one commit on `main`, under `parent`.
///
/// `-b main` pins the initial branch rather than taking whatever `init.defaultBranch` the
/// runner is configured with, and the empty commit gives `HEAD` something to point at. An
/// unborn branch is a real state a repository can be in and it is not the one this test is
/// about, so it is removed rather than reasoned around.
fn make_repository(parent: &Path, git: &nysia_core::pty::ResolvedProgram) -> PathBuf {
    let repo = parent.join(REPOSITORY);
    std::fs::create_dir_all(&repo)
        .unwrap_or_else(|err| panic!("harness: could not make the repository directory: {err}"));
    git_output(git, &repo, &["init", "-q", "-b", "main"]);
    git_output(
        git,
        &repo,
        &[
            // Passed with `-c` rather than written into the repository's config, so the
            // runner's own identity is neither needed nor touched.
            "-c",
            "user.email=acceptance@nysia.invalid",
            "-c",
            "user.name=Nysia Acceptance",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "initial",
        ],
    );
    repo
}

/// Every `id` in a `project list --json` answer, or `None` for every way there is not one.
///
/// Total rather than asserting as it goes, so the assertions at the end of the test carry
/// the whole story. Today this returns `None` because the CLI refuses the argv, and the
/// failure message has to say that rather than panicking inside a JSON parser.
///
/// The shape it expects is the one `session list --json` already prints: a bare array on
/// stdout, one object per project, each with a string `id`. Pinning one spelling is the
/// point; a test that accepted two would measure neither.
fn listed_ids(run: &Run) -> Option<Vec<String>> {
    let projects: serde_json::Value = serde_json::from_str(run.stdout.trim()).ok()?;
    projects
        .as_array()?
        .iter()
        .map(|project| {
            project
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// One run, rendered for a failure message.
fn told(label: &str, run: &Run) -> String {
    format!(
        "\n{label}\n  exit: {:?}\n  stdout: {:?}\n  stderr: {:?}",
        run.code,
        run.stdout.trim(),
        run.stderr.trim()
    )
}

#[test]
#[ignore = "expected to fail until v0.3 wave C1 (docs/plans/v0.3-delivery-plan.md §4) \
            serves the project verbs, adds the projects table and ships the `nysia project` \
            CLI; there is no verb, no store table and no subcommand to pass against today"]
fn a_registered_project_is_still_there_with_its_id_after_the_daemon_restarts() {
    let git = announce_git();
    let mut daemon = Nysiad::start("restart");
    let repo = make_repository(&daemon.runtime_dir, &git);

    // The id the wire type derives, computed here from the same canonical path the daemon
    // will see. This is the half that persistence alone does not prove: a daemon that
    // minted a random id and stored it would survive the restart and still be wrong.
    let canonical = std::fs::canonicalize(&repo).expect("the repository can be canonicalised");
    let expected = nysia_proto::ProjectId::from_canonical_path(&canonical)
        .expect("a temp directory is a Unicode path");
    let path = repo.display().to_string();

    // Captured, never `.ok()`d: the restart below has to happen on every run, including the
    // runs where registration is refused because the verb does not exist yet.
    let registered = daemon.run(&["project", "register", &path, "--json", "--no-spawn"]);
    let before = daemon.run(&["project", "list", "--json", "--no-spawn"]);

    daemon.restart();

    let after = daemon.run(&["project", "list", "--json", "--no-spawn"]);

    let story = format!(
        "{}{}{}",
        told("project register, on the first daemon:", &registered),
        told("project list, on the first daemon:", &before),
        told("project list, on the second daemon:", &after)
    );

    // Registered, listed, and listed under the id its canonical path derives.
    assert_eq!(
        listed_ids(&before),
        Some(vec![expected.to_string()]),
        "`nysia project list --json` should report exactly the repository that was just \
         registered, under the id `ProjectId::from_canonical_path` derives for it \
         ({expected}).{story}"
    );

    // §3.3, which is the point: a second daemon process, on the same runtime directory,
    // reports the same project under the same id.
    assert_eq!(
        listed_ids(&after),
        listed_ids(&before),
        "the project should have survived the daemon restart unchanged; a project the \
         sidebar forgets, or comes back with a new id for, is not a registered project.\
         {story}"
    );
}
