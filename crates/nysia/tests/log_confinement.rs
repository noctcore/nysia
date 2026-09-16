//! The confinement, as the shipped binary applies it: **one variable decides, and it is off**.
//!
//! `crates/nysia/src/log.rs` holds the rule against a filter the tests build themselves, and
//! `the_runtime_branch_confines_unless_the_way_out_is_named` holds the branch that chooses
//! between the two by running it in a process of its own. Neither of those says anything about
//! the binary that ships. `main` is what wires `log::filter` into the subscriber, and a change
//! that dropped that one call would leave every check in that module green while every daemon
//! anybody runs wrote terminal bytes and caller paths into a file.
//!
//! So this drives the real thing. It starts `nysia --daemon`, points its diagnostics at the
//! file the real spawn path points them at, has it open a session, and then reads that file.
//! Two daemons, one difference between them: `NYSIA_LOG_UNCONFINED`. With it set, a confined
//! crate's own logging is in the file. With it removed, the same `NYSIA_LOG` in the same
//! binary puts none of it there.
//!
//! # Which leg exercises what
//!
//! [`what_leaks`] is the whole of the asymmetry and says why it exists: `portable_pty` reaches
//! the rule by a different route on Unix than on Windows, so a single string would have been a
//! real leak on one leg and vacuously absent on the other. Everything else here is the same on
//! both.
//!
//! # What this cannot show
//!
//! That the *window* does the same. It is a Tauri binary and opening one in CI is not a test;
//! `nysia-desktop`'s own `log` module re-execs its test binary at a child that calls the real
//! `install`, which is the closest that side can get.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nysia_core::rpc::{ENDPOINT_VAR, Endpoint, EnvSource, RUNTIME_DIR_VAR, log_file};

/// The binary cargo just built for this test target — the one that ships.
const NYSIA: &str = env!("CARGO_BIN_EXE_nysia");

/// The longest any single CLI invocation may take.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest to wait for the daemon to start answering.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// How long each leg watches the log for the line the other leg found.
///
/// Both legs spend it. The control returns as soon as the line appears and the confined leg
/// waits the whole of it, so the confined leg's `!contains` is never the faster read of the
/// two — which is the way that assertion would otherwise pass for the wrong reason.
const SETTLE: Duration = Duration::from_secs(10);

/// `NYSIA_LOG` for both legs.
///
/// `info` so the daemon's own lines are in the file whatever else happens — a `!contains` on
/// an empty log is not evidence — and the module-qualified directive because that is the
/// spelling the screen exists for. Refused on the confined leg, honoured on the control.
const ASKED: &str = "info,portable_pty::cmdbuilder=trace";

/// A line the daemon writes about itself, so an empty file cannot pass for a confined one.
const DAEMON_OWN_LINE: &str = "nysiad listening";

/// What this leg's daemon writes out of a confined crate once the confinement is off.
///
/// Different on each platform because `portable_pty` reaches the rule by a different route on
/// each, and one string would have been a real leak on one leg and vacuously absent on the
/// other.
///
/// **Unix.** `CommandBuilder::as_command` resolves `$SHELL` on every spawn, and `SHELL` is an
/// `envOverrides` entry a `session create` carries, because it is not one of
/// `nysia_core::pty::SCRUBBED_VARS`. Pointed at a file that is not executable, the crate
/// writes back the path the caller handed it, at `warn` — a level the shipped default already
/// permits. The evidence is that path, which is `marker` and which no other test could
/// produce.
///
/// **Windows.** There is nothing to plant, so the evidence is not a marker. `CommandBuilder::new`
/// calls `get_base_env`, which reads the machine and user environment out of the registry and
/// writes every name and value at `trace`; the line below is the one that carries them. It is
/// the same rule broken — `nysia_core::rpc::log_file`'s *no environment* — with the machine's
/// own environment block in place of a path this test chose.
fn what_leaks(marker: &str) -> String {
    if cfg!(windows) {
        "adding SYS env:".to_owned()
    } else {
        marker.to_owned()
    }
}

/// One CLI invocation's result.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A directory whose name no other test in this repo could produce.
///
/// Tagged and suffixed with the pid the way the unit tests' own scratch directories are, so
/// two runs at the same time cannot read each other's marker.
fn a_marked_directory(tag: &str) -> (String, PathBuf) {
    let marker = format!("ZZZ-LOGRULE-{tag}-{}", std::process::id());
    let dir = std::env::temp_dir().join(&marker);
    let _ = std::fs::create_dir_all(&dir);
    (marker, dir)
}

/// The shell profile this machine can actually start.
///
/// `pwsh` where §9 says it is — both CI runners have it — `cmd` on a Windows box without it,
/// and the platform's default on Unix. The session has to *start*: a create that was refused
/// before a pty was opened would leave `portable_pty` never called and both legs quiet.
fn profile() -> Vec<&'static str> {
    if nysia_core::pty::resolve("pwsh").is_ok() {
        return vec!["--profile", "pwsh"];
    }
    if cfg!(windows) {
        return vec!["--profile", "cmd"];
    }
    Vec::new()
}

/// Wait for `child`, giving up after `timeout`.
fn wait_for(child: &mut Child, timeout: Duration) -> Option<Option<i32>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => return None,
        }
    }
}

/// The daemon under test, killed when the test ends however it ends.
struct Nysiad {
    child: Child,
    runtime_dir: PathBuf,
    log: PathBuf,
}

impl Nysiad {
    /// Start a daemon on its own endpoint, with its diagnostics in its own log file.
    ///
    /// The file is the one [`Endpoint::log_path`] names, opened the way `spawn_daemon` in
    /// `nysia_core::rpc::discovery` opens it and handed to the child as its stdio — which is
    /// what a daemon a client spawned gets. This test owns the child instead of letting a
    /// client spawn it, because a daemon nobody owns outlives the test, and on Windows one
    /// that is still running holds the binary open against the next `cargo build`.
    ///
    /// `--no-idle-retire` so the daemon cannot decide it is unwanted during a pause on a slow
    /// runner. `lifted` is set or **removed** explicitly rather than inherited: a developer
    /// with `NYSIA_LOG_UNCONFINED` exported would otherwise turn the confined leg into a
    /// second copy of the control, and it would pass.
    fn start(tag: &str, lifted: bool) -> Self {
        let runtime_dir =
            std::env::temp_dir().join(format!("nysia-logrule-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        // `unwrap_or_else`, not `expect`: clippy's allow-expect-in-tests covers `#[test]`
        // functions and `#[cfg(test)]` modules, and this is neither.
        std::fs::create_dir_all(&runtime_dir)
            .unwrap_or_else(|err| panic!("could not make a runtime directory: {err}"));

        // The real formula for the real path, rather than a second copy of it here that could
        // drift into naming a file nothing writes.
        let endpoint = Endpoint::resolve(
            nysia_proto::PROTOCOL_VERSION,
            EnvSource {
                runtime_dir_override: Some(runtime_dir.clone()),
                endpoint_override: None,
                isolated: true,
                ..EnvSource::process()
            },
        )
        .unwrap_or_else(|err| panic!("could not resolve an endpoint: {err}"));
        let log = endpoint.log_path();

        let file = log_file::open_for_append(&log)
            .unwrap_or_else(|err| panic!("could not open the daemon's log: {err}"));
        let second = file
            .try_clone()
            .unwrap_or_else(|err| panic!("could not clone the log handle: {err}"));

        let mut command = Command::new(NYSIA);
        command
            .args(["--daemon", "--no-idle-retire"])
            .env(RUNTIME_DIR_VAR, &runtime_dir)
            .env_remove(ENDPOINT_VAR)
            .env(log_file::LOG_ENV, ASKED)
            .stdin(Stdio::null())
            .stdout(Stdio::from(second))
            .stderr(Stdio::from(file));
        if lifted {
            command.env(log_file::UNCONFINED_ENV, "1");
        } else {
            command.env_remove(log_file::UNCONFINED_ENV);
        }

        let child = command
            .spawn()
            .unwrap_or_else(|err| panic!("could not start the daemon: {err}"));

        let daemon = Self {
            child,
            runtime_dir,
            log,
        };
        daemon.await_ready();
        daemon
    }

    /// Run a client verb against this daemon, bounded.
    ///
    /// `--no-spawn` on every one: the daemon under test is the one this file started, and a
    /// verb that quietly started a second would make the file being read somebody else's.
    /// The client's own `NYSIA_LOG` is `warn` and its own way out is removed, so nothing in
    /// this function is what the assertions are reading.
    fn run(&self, args: &[&str]) -> Run {
        let mut child = Command::new(NYSIA)
            .args(args)
            .env(RUNTIME_DIR_VAR, &self.runtime_dir)
            .env_remove(ENDPOINT_VAR)
            .env(log_file::LOG_ENV, "warn")
            .env_remove(log_file::UNCONFINED_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("could not run {NYSIA} {args:?}: {err}"));

        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let code = wait_for(&mut child, STEP_TIMEOUT).unwrap_or_else(|| {
            let _ = child.kill();
            panic!(
                "`nysia {}` did not finish in {STEP_TIMEOUT:?}",
                args.join(" ")
            )
        });
        Run {
            code,
            stdout: stdout
                .recv_timeout(STEP_TIMEOUT)
                .unwrap_or_else(|_| panic!("`nysia {}` never closed stdout", args.join(" "))),
            stderr: stderr.recv_timeout(STEP_TIMEOUT).unwrap_or_default(),
        }
    }

    /// Poll until the daemon answers a verb, or give up.
    fn await_ready(&self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let run = self.run(&["session", "list", "--no-spawn"]);
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

    /// Everything in this daemon's log once `needle` is in it, or once [`SETTLE`] has passed.
    fn log_until(&self, needle: &str) -> String {
        let deadline = Instant::now() + SETTLE;
        loop {
            let written = std::fs::read_to_string(&self.log).unwrap_or_default();
            if written.contains(needle) || Instant::now() >= deadline {
                return written;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Nysiad {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// Read a pipe on its own thread, delivering the whole of it through a channel.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
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

/// Have `daemon` open a session that makes `portable_pty` write, and say it succeeded.
///
/// `--cwd` and `--env SHELL=…` are both values a `SessionCreate` carries from a caller, which
/// is what makes them the right thing to plant: the rule this whole module is about is that a
/// caller's own paths do not reach the file.
fn open_a_session(daemon: &Nysiad, dir: &Path) -> Run {
    let cwd = dir.display().to_string();
    let shell = format!("SHELL={}", dir.join("not-an-executable").display());
    let mut args = vec!["session", "create", "--json", "--no-spawn"];
    args.extend(profile());
    args.extend(["--cwd", &cwd, "--env", &shell]);

    let run = daemon.run(&args);
    assert_eq!(
        run.code,
        Some(0),
        "the session never started, so neither leg reached `portable_pty` at all: {}{}",
        run.stdout,
        run.stderr
    );
    run
}

#[test]
fn the_shipped_binarys_log_is_confined_unless_the_way_out_is_named() {
    // The control, first and in the same body, because every assertion about the confined
    // daemon below is a `!contains` — which a wrong target name, a session that never
    // started, or a `portable_pty` that stopped logging would each satisfy on its own.
    //
    // One difference between the two daemons: `NYSIA_LOG_UNCONFINED`. Same binary, same
    // `NYSIA_LOG`, same profile, same planted values, same file read the same way.
    let (marker, dir) = a_marked_directory("lifted");
    let leaked = what_leaks(&marker);

    let lifted = Nysiad::start("lifted", true);
    open_a_session(&lifted, &dir);
    let written = lifted.log_until(&leaked);
    assert!(
        written.contains(DAEMON_OWN_LINE),
        "the control daemon wrote nothing about itself, so this file is not its log: {written}"
    );
    assert!(
        written.contains(&leaked),
        "nothing reached the log even with {} named, so the confined leg below would hold for \
         the wrong reason — see `what_leaks` for what this leg was expecting and why: {written}",
        log_file::UNCONFINED_ENV
    );
    drop(lifted);
    let _ = std::fs::remove_dir_all(&dir);

    // And with the way out removed, the same value in the same binary puts none of it there.
    let (marker, dir) = a_marked_directory("held");
    let leaked = what_leaks(&marker);

    let held = Nysiad::start("held", false);
    open_a_session(&held, &dir);
    let written = held.log_until(&leaked);
    assert!(
        written.contains(DAEMON_OWN_LINE),
        "the confined daemon wrote nothing about itself, so the assertion below would hold \
         against an empty file: {written}"
    );
    assert!(
        !written.contains(&leaked),
        "NYSIA_LOG={ASKED} reached a confined crate in the shipped binary: {written}"
    );
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}
