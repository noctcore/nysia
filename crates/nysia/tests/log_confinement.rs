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

/// A target only the daemon's **subscriber** writes, so an empty file cannot pass for a
/// confined one.
///
/// `nysiad listening` was this control and proved less than it looked.
/// `crates/nysia/src/daemon.rs` prints those words on **stdout**, and [`Nysiad::start`] points
/// stdout at this same file — so a daemon whose subscriber wrote nothing at all satisfied it,
/// and only the lifted leg, where the leak is itself a subscriber line, was showing that the
/// pipeline under test had run at all. `nysia_core::rpc::server` raises the same words through
/// `tracing::info!` a line later, and a **target** is something only the formatter puts in a
/// line: no `println!` in this binary writes one.
///
/// The target on its own rather than the target and the message it introduces, because
/// `tracing_subscriber::fmt` colours its output whether or not the handle is a terminal, and
/// what sits between the two in the file is an ANSI escape rather than `: `.
const A_TARGET_ONLY_THE_SUBSCRIBER_WRITES: &str = "nysia_core::rpc::server";

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
struct Marked {
    /// The directory's own name, which is what gets planted in a session's values.
    marker: String,
    /// The directory itself.
    dir: PathBuf,
}

/// Removed here rather than by a line at the foot of the test body, because that line does
/// not run when an assertion panics — so the run that most wants looking at was also the one
/// that left `ZZZ-LOGRULE-*` directories behind in the temp directory, and a guard that
/// litters when it fails is a guard people stop running.
///
/// This closes the panic path and promises no more than that. The removal is still best
/// effort: [`Nysiad::drop`] kills the daemon rather than waiting for it, and a pty's own
/// child can outlive that kill still holding this directory as its working directory, which
/// on Windows is enough to make `remove_dir_all` fail.
impl Drop for Marked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn a_marked_directory(tag: &str) -> Marked {
    let marker = format!("ZZZ-LOGRULE-{tag}-{}", std::process::id());
    let dir = std::env::temp_dir().join(&marker);
    let _ = std::fs::create_dir_all(&dir);
    Marked { marker, dir }
}

/// What an assertion may say about a log it must not print.
///
/// Every assertion below reads a file this test told a daemon to fill, and on Windows the
/// control leg has deliberately put `get_base_env`'s whole HKLM and HKCU environment block in
/// it — `Path`, `JAVA_HOME`, every `NVM_*`, the machine's user name — because that block *is*
/// the evidence that leg exists to find. The file itself is owner-only
/// (`log_file::open_for_append`), but a failing assertion's message is the thing a person
/// pastes into an issue, and pasting it would carry the environment straight past those
/// permissions. That is traps register #13 in the test whose subject is keeping such content
/// out of a file.
///
/// So: how many of the log's lines carry `needle`, out of how many, and the first of those
/// lines **cut at the end of the match**. Which line, at what level, from which target is
/// everything these assertions decide; what follows the match is, on Windows, one environment
/// variable's value, and none of it is diagnosis.
///
/// `crates/nysia/src/log.rs` holds the same helper for the same reason. The two cannot share
/// one: this is a separate test binary, and the other lives in a `#[cfg(test)]` module that
/// nothing outside its crate can name.
fn where_it_is(written: &str, needle: &str) -> String {
    let lines = written.lines().count();
    let carrying: Vec<&str> = written
        .lines()
        .filter(|line| line.contains(needle))
        .collect();
    let Some(first) = carrying.first() else {
        return format!("`{needle}` is in none of the log's {lines} lines");
    };
    let upto = first
        .find(needle)
        .map_or(*first, |at| &first[..at + needle.len()]);
    format!(
        "`{needle}` is in {} of the log's {lines} lines, the first of them reading `{upto}` up \
         to the match",
        carrying.len()
    )
}

/// The note the confined daemon owes about [`ASKED`], built from [`ASKED`] itself.
///
/// Through the same screen the daemon ran rather than a second copy of the directive written
/// out here, which could drift out of step with the value being set and leave this asserting
/// a line nothing writes. The panic is a control of its own: an [`ASKED`] the screen refuses
/// nothing of is an [`ASKED`] that has stopped exercising the screen at all, and the confined
/// leg below would then be measuring the fold and calling it the screen.
fn the_note_the_screen_owes() -> String {
    let screened = log_file::screen_directives(ASKED);
    let refused = screened.refused.first().unwrap_or_else(|| {
        panic!(
            "the screen refuses nothing in {}={ASKED}",
            log_file::LOG_ENV
        )
    });
    log_file::refusal_note(refused)
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
    // The control, first and in the same body, because the claim the confined leg below leads
    // up to is a `!contains` — which a wrong target name, a session that never started, or a
    // `portable_pty` that stopped logging would each satisfy on its own.
    //
    // One difference between the two daemons: `NYSIA_LOG_UNCONFINED`. Same binary, same
    // `NYSIA_LOG`, same profile, same planted values, same file read the same way.
    //
    // No assertion here prints the log. See [`where_it_is`] for why not, and note that the
    // reason is this test's own subject: the control leg's file carries, on Windows, exactly
    // the environment block the confined leg exists to keep out of one.
    let plant = a_marked_directory("lifted");
    let leaked = what_leaks(&plant.marker);

    let lifted = Nysiad::start("lifted", true);
    open_a_session(&lifted, &plant.dir);
    let written = lifted.log_until(&leaked);
    assert!(
        written.contains(A_TARGET_ONLY_THE_SUBSCRIBER_WRITES),
        "the control daemon's subscriber wrote nothing, so this file is not its log: {}",
        where_it_is(&written, A_TARGET_ONLY_THE_SUBSCRIBER_WRITES)
    );
    assert!(
        written.contains(&leaked),
        "nothing reached the log even with {} named, so the confined leg below would hold for \
         the wrong reason — see `what_leaks` for what this leg was expecting and why: {}",
        log_file::UNCONFINED_ENV,
        where_it_is(&written, &leaked)
    );
    drop(lifted);

    // And with the way out removed, the same value in the same binary puts none of it there.
    let plant = a_marked_directory("held");
    let leaked = what_leaks(&plant.marker);

    let held = Nysiad::start("held", false);
    open_a_session(&held, &plant.dir);
    let written = held.log_until(&leaked);

    // Two controls, and neither is the other in a different spelling.
    //
    // The first is the refusal the confined daemon owes whoever set `ASKED`, which nothing on
    // this side held it to — `nysia-desktop`'s `log` module asserts the window's equivalent
    // and this file asserted neither. `starts_with` rather than `contains` because
    // `unconfined_note`'s own words are *said into the log it is about, before anything else
    // is written there*, and that is a claim about position. It is also the control that owes
    // nothing to `tracing`: `filter` writes this to stderr before a subscriber exists, so it
    // says the file is this daemon's and that the screen ran on `ASKED`, whatever the
    // subscriber went on to do.
    //
    // The second is what the subscriber itself put there, which is the half the first cannot
    // reach: a daemon that announced the refusal and then wrote through a subscriber pointed
    // somewhere else would satisfy it and leave the `!contains` below vacuous.
    let announced = the_note_the_screen_owes();
    assert!(
        written.starts_with(&announced),
        "the confined daemon did not open its log by saying which directive it would not \
         honour: {}",
        where_it_is(&written, &announced)
    );
    assert!(
        written.contains(A_TARGET_ONLY_THE_SUBSCRIBER_WRITES),
        "the confined daemon's subscriber wrote nothing, so the assertion below would hold \
         against a file only `filter` and stdout had touched: {}",
        where_it_is(&written, A_TARGET_ONLY_THE_SUBSCRIBER_WRITES)
    );
    assert!(
        !written.contains(&leaked),
        "NYSIA_LOG={ASKED} reached a confined crate in the shipped binary: {}",
        where_it_is(&written, &leaked)
    );
    drop(held);
}
