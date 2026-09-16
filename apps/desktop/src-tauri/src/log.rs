//! Where the window's diagnostics go, and what they are allowed to say.
//!
//! # The file
//!
//! `<runtime dir>/<stem>.window.log`, beside the daemon's own, opened and rotated through
//! [`nysia_core::rpc::log_file`] — the same 8 MiB cap and the same three kept copies. Both
//! processes therefore have one retention policy, one owner-only directory and one answer to
//! "how much disk does Nysia use".
//!
//! Under `windows_subsystem = "windows"` a packaged window has **no stderr**: anything
//! `tracing` wrote there went nowhere at all, and the window's half of a failure has been
//! unreadable in every release build so far. The file is what fixes that. Output still goes
//! to stderr as well, because a developer running `pnpm dev` reads the terminal.
//!
//! # Why the webview logs here and not through the daemon
//!
//! This was the open question on #75, and the lean was the other way. The argument that
//! decided it is not cost, it is reachability:
//!
//! > The transport events worth reading are the ones that happen when the daemon is what
//! > is broken.
//!
//! A failed `daemon_connect`, a protocol mismatch, a socket that dropped mid-verb, the
//! reconnect loop backing off — a logger that posted those over the socket would be silent
//! for exactly the failures anybody opens a log to investigate, and would add a control-round
//! trip to the path that is already failing. Writing to the window's own file has neither
//! property. The cost is two files instead of one; they sit in the same directory, under the
//! same policy, and correlate on `PaneKey` / `SessionHandle` / `Incarnation` / `StreamId`,
//! which the protocol already carries on both sides.
//!
//! Under D-1 the window holds no state the daemon should own. A log file is not session
//! state — nothing reads it back, and deleting it loses nothing but history — but it is the
//! first thing this process writes into the runtime directory, so it is named here rather
//! than left for a reviewer to wonder about.
//!
//! # What it never writes
//!
//! [`nysia_core::rpc::log_file`] states the rule for every Nysia log: no PTY output, no
//! keystrokes, no `tool_input`, no environment or working directory, no request or response
//! bodies. On this side that is structural rather than remembered —
//! [`crate::commands`] logs a verb's *name* and never its payload, and the one entry point
//! the webview can reach takes a name from a closed list and two numbers.
//!
//! The rule binds the crates this process links as well as the code it writes, so `NYSIA_LOG`
//! is put through [`log_file::screen_directives`] and [`log_file::CONFINED_TARGETS`] is folded
//! in behind what survives — the same two halves the daemon applies, and for the same reason
//! neither is enough alone. Under D-1 and D-7 the terminal state and the PTYs both live in the
//! daemon, so this process runs no VT and opens no pty, and nothing here is expected to reach
//! any of those targets. Both halves are applied anyway, because "this binary happens not to
//! call it today" is the kind of premise that stops being true without anybody noticing, and
//! the whole point of the rule is that it does not depend on remembering.
//!
//! `portable_pty` is the entry that makes that argument concrete rather than cautious. It is
//! already linked here, through `nysia-core`, and the line it writes about a spawn that failed
//! carries the caller's working directory at `ERROR` — a level the shipped default keeps. A
//! window that grew one path into a pty would have started leaking on the day it did, with no
//! `NYSIA_LOG` involved and nothing to prompt anybody to go and add the entry.
//!
//! The daemon's `log` module is where the confinement is explained. It is not where this copy
//! is proved: a lock on one door is not a policy, and a proof of one door is not a proof of
//! two. The tests at the bottom of this file ask this process's own `confine` the question,
//! and they fail if this copy stops screening or stops folding.
//!
//! # Where a refused directive is announced
//!
//! **Into the file, written directly, before the subscriber exists.** Not through `tracing`,
//! which is the thing that cannot be trusted to carry it: the filter has just refused a
//! directive, and any `NYSIA_LOG` that names targets rather than a level leaves nothing
//! matching this module's own — so `NYSIA_LOG=nysia_core=debug,vte::ansi=trace` used to be
//! answered by a refusal note the refusing filter then dropped, and
//! `NYSIA_LOG_UNCONFINED=1 NYSIA_LOG=nysia_core=debug` by a window that never said what its
//! log had become. The announcement went through the one filter it is about.
//!
//! The daemon has neither problem, because its note is an `eprintln!` that no filter sees and
//! its stderr *is* its log. This side needed the same property and the file handle is what
//! gives it: [`install`] holds it before `.init()` takes ownership, so the lines go in through
//! the same tee the subscriber is about to use — the file, and the stderr `pnpm dev` shows —
//! and they land ahead of everything else in it, including the window's own "logging here".
//! That is what `unconfined_note`'s *said into the log it is about, before anything else is
//! written there* asks for, and what this module did not do. The wording is `log_file`'s, so
//! the two processes cannot come to say it differently.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nysia_core::rpc::{Endpoint, log_file};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

/// What the window logs when `NYSIA_LOG` asks for nothing this filter can use.
///
/// The daemon's default, and the same reasoning: a window that refused to start over a
/// malformed `NYSIA_LOG` would have turned a typo into an outage. It is also where a value
/// whose every directive was refused lands — an `EnvFilter` built from an empty string enables
/// nothing at all, so falling through to one would answer `NYSIA_LOG=vte::ansi=trace` with a
/// window that logs nothing.
const DEFAULT_DIRECTIVES: &str = "info";

/// How often the window measures its own log against the cap.
///
/// The daemon's interval, for the same reasons — see `nysia_core::rpc::server`. The window
/// writes far less than the daemon does, so in practice this tick finds nothing to do for the
/// life of the process; it is here because "far less" is not "bounded".
const TRIM_INTERVAL: Duration = Duration::from_secs(30);

/// Start `tracing`, writing to the window's log file and to stderr.
///
/// Returns the file it opened, or `None` when the window is logging to stderr alone.
///
/// **Never fails.** An endpoint that will not resolve and a log that will not open are both
/// answered by logging to stderr and saying so, because a window that refused to start over a
/// diagnostics file would have turned a nuisance into an outage — and the first thing anybody
/// would want in order to debug *that* is the log it declined to open.
pub fn install() -> Option<PathBuf> {
    let asked = std::env::var(log_file::LOG_ENV).unwrap_or_default();
    let lifted = log_file::confinement_lifted();
    // Screened once and handed on, rather than screened again inside [`confine`]. The two
    // calls cannot disagree — it is a pure function of the same string — but a reader had to
    // establish that before they could be sure the line being announced was the line being
    // dropped.
    let screened = log_file::screen_directives(&asked);
    let filter = if lifted {
        unconfined(&asked)
    } else {
        confine(&screened)
    };

    let opened = Endpoint::from_env()
        .map_err(|err| format!("the runtime directory could not be named: {err}"))
        .and_then(|endpoint| {
            let path = endpoint.window_log_path();
            log_file::open_for_append(&path)
                .map(|file| (path, file))
                .map_err(|err| format!("the log could not be opened: {err}"))
        });

    match opened {
        Ok((path, file)) => {
            // Through the writer rather than through `tracing`, and before `.init()` takes
            // ownership of it. See the module docs: the filter about to be installed is the
            // one that just refused a directive, and a target-scoped `NYSIA_LOG` leaves it
            // matching nothing in this module — so an announcement made through it is an
            // announcement the reader never gets. This also puts the lines ahead of the
            // "logging here" below, which is the ordering `unconfined_note` asks for.
            let writer = sink(file);
            say(&mut writer.make_writer(), lifted, &screened);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(writer)
                .init();
            spawn_trim(path.clone());
            tracing::info!(path = %path.display(), "the window is logging here");
            Some(path)
        }
        Err(why) => {
            // Stderr is the whole of this window's diagnostics now, so that is where the
            // notes go — still before `.init()`, for the same reason.
            say(&mut std::io::stderr(), lifted, &screened);
            // The same filter, confinement included. A window that fell back to stderr is
            // still a window whose terminal crates must not print bytes — and on a dev machine
            // that stderr is a scrollback somebody may paste.
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .init();
            // On a packaged Windows build this warning goes nowhere, which is the whole
            // problem it is reporting. It is emitted anyway: `pnpm dev` shows it, and that is
            // where somebody is in a position to fix the cause.
            tracing::warn!(%why, "the window has no log file; diagnostics go to stderr only");
            None
        }
    }
}

/// Say what this log is now allowed to contain, into the log itself.
///
/// Every write is discarded on failure. A window that died because its diagnostics file went
/// away mid-sentence would have turned a nuisance into the outage [`install`] exists not to
/// cause, and the daemon's `note` answers its own stderr the same way.
fn say(out: &mut impl Write, lifted: bool, screened: &log_file::Screened<'_>) {
    if lifted {
        let _ = writeln!(out, "{}", log_file::unconfined_note());
        return;
    }
    for refused in &screened.refused {
        let _ = writeln!(out, "{}", log_file::refusal_note(refused));
    }
}

/// Hold the linked crates down, whatever `NYSIA_LOG` asked for.
///
/// Two halves, the daemon's two. The screen refuses a directive that could out-specify an
/// entry — `NYSIA_LOG=vte::ansi=trace` names a module where the entry names a crate, and
/// `EnvFilter` resolves a callsite against the longest target that matches it. The fold is
/// applied *after* the user's filter, so raising the level to debug something does not switch
/// the rule off either (CLAUDE.md §6).
///
/// Takes what [`log_file::screen_directives`] already answered, because [`install`] screens
/// once and announces the refusals out of the same answer it filters by.
///
/// A directive that does not parse is skipped rather than panicking a window at startup, and
/// `every_confined_directive_parses` in `nysia`'s `log` module is what stops one shipping —
/// though a parse is only half of it, because a directive can also parse and match nothing.
/// `a_failed_spawns_path_does_not_reach_a_confined_log`, beside it, is the half that plants a
/// real spawn. Neither of those can speak for this copy, which is what the tests below are for.
fn confine(screened: &log_file::Screened<'_>) -> EnvFilter {
    log_file::CONFINED_TARGETS
        .iter()
        .filter_map(|directive| directive.parse().ok())
        .fold(
            parsed(&screened.honoured.join(",")),
            EnvFilter::add_directive,
        )
}

/// `asked` honoured in full, with [`log_file::CONFINED_TARGETS`] **not** applied.
///
/// Reached only when [`log_file::UNCONFINED_ENV`] is set, and named so that this is what a
/// reviewer finds when they ask how the confinement can be off (CLAUDE.md §6). The window is
/// not where anybody would go looking for `vte` output — it runs no VT — but the variable is
/// one variable for the whole of Nysia, and a window that quietly ignored it while the daemon
/// honoured it would be a second rule nobody wrote down. [`install`] says what it means into
/// the log, written to the file directly, ahead of everything else that file will hold.
fn unconfined(asked: &str) -> EnvFilter {
    parsed(asked)
}

/// `asked` as an `EnvFilter`, or [`DEFAULT_DIRECTIVES`] when it is empty or does not parse.
///
/// The empty case is answered here rather than left to `EnvFilter`, which reads an empty string
/// as a filter with no directives at all — one that enables nothing, not one that falls back.
fn parsed(asked: &str) -> EnvFilter {
    if asked.trim().is_empty() {
        return EnvFilter::new(DEFAULT_DIRECTIVES);
    }
    EnvFilter::try_new(asked).unwrap_or_else(|_| EnvFilter::new(DEFAULT_DIRECTIVES))
}

/// Both sinks at once: the log file, and the stderr a developer is watching.
///
/// `Arc<File>` is a `MakeWriter` because `&File` is `io::Write`, and `.and` layers the two, so
/// an event reaches both rather than one or the other depending on the build.
///
/// Its own function so a test can build the writer [`install`] builds without calling
/// `install`, and so `install` can write into it before `.init()` takes ownership.
/// `.init()` installs a *global* subscriber and can happen once in a process, so `install`
/// is reachable from one test and one process only — which is what `what_the_window_logged`
/// re-execs this test binary for. A composition nothing exercises is exactly where a silent
/// "the file stayed empty" would live.
fn sink(file: std::fs::File) -> impl for<'a> tracing_subscriber::fmt::MakeWriter<'a> + 'static {
    Arc::new(file).and(std::io::stderr)
}

/// Keep the window's own log inside the cap for as long as the window runs.
///
/// A plain thread rather than a Tauri task. It is a `stat` every half minute and an
/// occasional copy; giving it an async runtime would buy nothing, and `tokio::spawn` inside
/// this process is the trap that turns a panic into `abort()` (traps register #2).
///
/// The window owns this handle outright rather than inheriting it, so the copy-and-truncate
/// in `log_file` is not strictly forced here the way it is for the daemon. It is used anyway:
/// one rotation with one set of semantics is what makes the two files readable the same way.
fn spawn_trim(path: PathBuf) {
    std::thread::Builder::new()
        .name("nysia-log-trim".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(TRIM_INTERVAL);
                match log_file::trim(&path) {
                    Ok(log_file::Trimmed::Rotated { bytes }) => tracing::info!(
                        moved_to = %log_file::rotation_path(&path, 1).display(),
                        bytes,
                        "the window log passed its cap; the previous contents were rotated aside"
                    ),
                    Ok(log_file::Trimmed::Untouched) => {}
                    Err(err) => {
                        tracing::warn!(%err, "could not rotate the window log");
                    }
                }
            }
        })
        // A window that could not spawn its trim thread still starts. The log grows, which is
        // worse than the cap and far better than no window.
        .map(drop)
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "the window log will not be trimmed while this window runs");
        });
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A sentinel no terminal would produce and no test could mistake for anything else.
    ///
    /// The daemon's, deliberately. The two modules are asking the same question about the same
    /// list, and somebody who greps for one of these tests should land on both.
    const SENTINEL: &str = "ZZZ-DBG-SENTINEL-4f3e9a";

    /// Everything the window's log file holds after `body` ran under `filter`.
    ///
    /// A thread-local subscriber, never a global one: `install` calls `.init()`, which can
    /// happen once per process, and the case that matters most here is the one *without* the
    /// screen.
    ///
    /// The file alone rather than [`sink`], which would also write to stderr. What is being
    /// asked is what the filter let through; `sink`'s other half is already held by
    /// `an_event_reaches_the_file_and_not_only_stderr`, and keeping it out of this means a test
    /// that deliberately leaks a sentinel does not print it into the run.
    fn through(filter: EnvFilter, body: impl FnOnce()) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nth = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("nysia-window-filter-{}-{nth}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("nysiad.window.log");

        let file = log_file::open_for_append(&path).expect("a log");
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Arc::new(file))
            .finish();
        tracing::subscriber::with_default(subscriber, body);

        let written = std::fs::read_to_string(&path).expect("the log");
        let _ = std::fs::remove_dir_all(&dir);
        written
    }

    /// This module's filter as it was before the screen: `NYSIA_LOG` whole, the list folded in.
    ///
    /// The control, and the shape the bug had here too. Every test below asserts the positive
    /// through this **in the same body** before it asserts the negative, so that filtering the
    /// run to one test name cannot leave a vacuous pass behind (traps register #12).
    fn as_it_was_before_the_screen(asked: &str) -> EnvFilter {
        log_file::CONFINED_TARGETS
            .iter()
            .filter_map(|directive| directive.parse().ok())
            .fold(EnvFilter::new(asked), EnvFilter::add_directive)
    }

    /// The whole of what [`install`] does to a `NYSIA_LOG` value, from the value.
    ///
    /// [`confine`] takes what [`log_file::screen_directives`] already answered, because
    /// `install` screens once and announces the refusals out of that same answer. Every test
    /// below writes the string rather than the split, so the screen is done here.
    fn confined(asked: &str) -> EnvFilter {
        confine(&log_file::screen_directives(asked))
    }

    /// One line on each target [`log_file::CONFINED_TARGETS`] holds down, at its real level.
    ///
    /// Raised rather than provoked, and that is the honest limit of what this file can do:
    /// under D-1 and D-7 this process runs no VT and opens no pty, so there is nothing here to
    /// make `vte` or `portable_pty` write of their own accord. `nysia`'s `log` module has the
    /// other end — real bytes through the real parser, and a real spawn that cannot succeed.
    /// What this holds is that *this copy* of the rule answers the same events the same way.
    fn as_the_terminal_crates_would(sentinel: &str) {
        tracing::debug!(target: "vte::ansi", "[unhandled osc_dispatch]: [{sentinel}]");
        tracing::trace!(target: "alacritty_terminal::term", "Setting title to '{sentinel}'");
        tracing::error!(target: "portable_pty::win::pseudocon", "CreateProcessW `{sentinel}` failed");
        tracing::warn!(target: "portable_pty::cmdbuilder", "$SHELL -> {sentinel} not executable");
    }

    #[test]
    fn the_windows_filter_refuses_a_module_qualified_nysia_log() {
        // CLAUDE.md §6 on this door. `EnvFilter` resolves a callsite against its longest
        // matching target and `CONFINED_TARGETS` names crates, so a directive that names a
        // module out-specified every entry — in this process exactly as in the daemon, because
        // it is the same list applied the same way.
        //
        // `vte:=trace` is the case a `::`-boundary screen would let through: four characters
        // against three, and `"vte::ansi".starts_with("vte:")`.
        for asked in [
            "vte::ansi=trace",
            "vte:=trace",
            "alacritty_terminal::term=trace",
            "portable_pty::win::pseudocon=trace",
            "portable_pty::cmdbuilder=trace",
            "info,vte::ansi=trace",
        ] {
            let leaked = through(as_it_was_before_the_screen(asked), || {
                as_the_terminal_crates_would(SENTINEL);
            });
            assert!(
                leaked.contains(SENTINEL),
                "NYSIA_LOG={asked} reached nothing even unscreened, so the assertion below \
                 would hold for the wrong reason: {leaked}"
            );

            let written = through(confined(asked), || as_the_terminal_crates_would(SENTINEL));
            assert!(
                !written.contains(SENTINEL),
                "NYSIA_LOG={asked} out-specified the window's confinement: {written}"
            );
        }
    }

    #[test]
    fn the_windows_filter_refuses_a_span_directive() {
        // The other hole, on this door. `EnvFilter::enabled` answers out of the span scope
        // before it consults a single target directive, so one of these enables every target
        // at its level however the list is spelled.
        //
        // The second spelling names a target of this crate's own and a span of this crate's
        // own, which is what makes it the dangerous shape: it looks like a directive about the
        // window and hands over everything the linked crates would have printed. Its target has
        // to match the span's, which is this module's path, so if that moves the control says
        // so rather than passing quietly.
        for asked in ["[serving]=trace", "nysia_desktop[serving]=trace"] {
            let leaked = through(as_it_was_before_the_screen(asked), || {
                tracing::info_span!("serving").in_scope(|| as_the_terminal_crates_would(SENTINEL));
            });
            assert!(
                leaked.contains(SENTINEL),
                "NYSIA_LOG={asked} enabled nothing even unscreened, so the assertion below \
                 would hold for the wrong reason: {leaked}"
            );

            let written = through(confined(asked), || {
                tracing::info_span!("serving").in_scope(|| as_the_terminal_crates_would(SENTINEL));
            });
            assert!(
                !written.contains(SENTINEL),
                "NYSIA_LOG={asked} reached every target through its span scope: {written}"
            );
        }
    }

    #[test]
    fn the_window_folds_the_list_in_when_nothing_named_those_crates() {
        // The half the screen cannot do, and the one this copy is likeliest to lose quietly.
        // Nothing in `NYSIA_LOG=trace` names a confined crate, so the screen refuses nothing
        // and the entire answer is the fold. This is what fails if this door stops folding
        // `CONFINED_TARGETS` in — the "lock on one door" the module docs are about, as a check
        // rather than as a sentence.
        let leaked = through(EnvFilter::new("trace"), || {
            as_the_terminal_crates_would(SENTINEL);
        });
        assert!(
            leaked.contains(SENTINEL),
            "the unguarded filter dropped these too, so the assertion below proves nothing: \
             {leaked}"
        );

        let written = through(confined("trace"), || {
            as_the_terminal_crates_would(SENTINEL);
            tracing::trace!(target: "nysia_desktop::commands", verb = "stream_attach", "served");
        });
        assert!(
            !written.contains(SENTINEL),
            "the window folded nothing in and the linked crates printed: {written}"
        );
        assert!(
            written.contains("stream_attach"),
            "the window's own lines went down with them: {written}"
        );
    }

    #[test]
    fn the_windows_way_out_is_the_same_one_variable() {
        // `NYSIA_LOG_UNCONFINED` is one variable for the whole of Nysia, and a window that
        // quietly ignored it while the daemon honoured it would be a second rule nobody wrote
        // down. Both halves in one body, as above: the way out lets something out, and the same
        // string does not get out without it.
        let asked = "vte::ansi=trace";

        let lifted = through(unconfined(asked), || as_the_terminal_crates_would(SENTINEL));
        assert!(
            lifted.contains(SENTINEL),
            "the window's way out does not let anything out: {lifted}"
        );

        let held = through(confined(asked), || as_the_terminal_crates_would(SENTINEL));
        assert!(
            !held.contains(SENTINEL),
            "the same string was honoured without anybody naming the way out: {held}"
        );
    }

    #[test]
    fn refusing_every_directive_still_leaves_the_windows_own_lines() {
        // A screen that answered `NYSIA_LOG=vte::ansi=trace` with a window that logged nothing
        // would have traded one bug for a worse one, and this module's whole reason for
        // existing is that a silent window is unreadable in a release build.
        let written = through(confined("vte::ansi=trace"), || {
            tracing::info!(target: "nysia_desktop::commands", verb = "stream_attach", "served");
            tracing::debug!(target: "nysia_desktop::commands", "below the default");
        });

        assert!(
            written.contains("stream_attach"),
            "refusing the only directive left the window with no log at all: {written}"
        );
        assert!(
            !written.contains("below the default"),
            "the fallback is `info`, not `debug`: {written}"
        );
    }

    /// Printed by a child that got all the way through, so "passed" and "never ran" differ.
    const CHILD_OK: &str = "NYSIA-LOGRULE-WINDOW-OK";

    /// What the child prefixes the path [`install`] returned with, so the parent can read it.
    const NAMED_ITS_LOG: &str = "NYSIA-LOGRULE-WINDOW-LOG ";

    /// A child test's own libtest name, which is its module path minus the crate.
    fn child_test_name(leaf: &str) -> String {
        let path = module_path!();
        let without_crate = path.split_once("::").map_or(path, |(_, rest)| rest);
        format!("{without_crate}::{leaf}")
    }

    /// Run the child window in a fresh copy of this test binary, and hand back its log file.
    ///
    /// By re-exec, not by `std::env::set_var`. Two reasons, and either alone would be enough.
    /// [`install`] reads `NYSIA_LOG`, `NYSIA_LOG_UNCONFINED` and `NYSIA_RUNTIME_DIR` from the
    /// process, and `cargo test` runs every test in this binary on threads of one process, so
    /// mutating any of them is a data race the 2024 edition made `unsafe` to write. And
    /// `install` ends in `.init()`, which installs a *global* subscriber and can happen once
    /// in a process — so the function this whole module is about is callable exactly once,
    /// and a process per call is the only way to ask it more than one question.
    /// `nysia_core::agent::claude::launch` arranges a `PATH` the same way.
    ///
    /// The way out is set or **removed** explicitly rather than inherited: a developer with
    /// `NYSIA_LOG_UNCONFINED` exported would otherwise turn the confined leg into a second
    /// copy of the control, and it would pass.
    fn what_the_window_logged(asked: &str, lifted: bool, tag: &str) -> String {
        let runtime_dir =
            std::env::temp_dir().join(format!("nysia-window-branch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        std::fs::create_dir_all(&runtime_dir).expect("a scratch runtime directory");

        let exe = std::env::current_exe().expect("the test binary");
        let mut command = Command::new(exe);
        command
            .args([
                "--exact",
                &child_test_name("a_child_window_that_installs_the_real_filter"),
                "--ignored",
                "--nocapture",
            ])
            .env(nysia_core::rpc::RUNTIME_DIR_VAR, &runtime_dir)
            .env_remove(nysia_core::rpc::ENDPOINT_VAR)
            .env(log_file::LOG_ENV, asked);
        if lifted {
            command.env(log_file::UNCONFINED_ENV, "1");
        } else {
            command.env_remove(log_file::UNCONFINED_ENV);
        }

        let output = command.output().expect("the child window runs");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "NYSIA_LOG={asked}: the child window failed:\n{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(CHILD_OK),
            "NYSIA_LOG={asked}: the child window was filtered out rather than run, so nothing \
             read back from it would mean anything:\n{stdout}"
        );

        // The file, never the child's stderr. `sink` writes to both, and what is being asked
        // is what reached the file that gets attached to an issue — reading the tee's other
        // half would also print a deliberately leaked sentinel into this run.
        let path = stdout
            .lines()
            .find_map(|line| line.strip_prefix(NAMED_ITS_LOG))
            .unwrap_or_else(|| panic!("the child window never named its log:\n{stdout}"));
        let written = std::fs::read_to_string(path).expect("the window's log");
        let _ = std::fs::remove_dir_all(&runtime_dir);
        written
    }

    #[test]
    fn the_windows_refusal_lands_ahead_of_everything_else_in_the_file() {
        // `unconfined_note`'s own words are *said into the log it is about, before anything
        // else is written there*, and that was not true of this side: the notes were emitted
        // after `.init()`, so `the window is logging here` reached the file first in every
        // case. Writing them to the handle before the subscriber takes it fixes the ordering
        // as well as the filtering, and this is the half a `contains` would not notice.
        //
        // A bare level, unlike the test above, because the line this has to come before is one
        // of this crate's own — under a target-scoped value that line is not enabled at all
        // and there would be nothing to be ahead of.
        let written = what_the_window_logged("info,vte::ansi=trace", false, "ordering");
        let note = log_file::refusal_note("vte::ansi=trace");
        let here = "the window is logging here";

        let note_at = written
            .find(&note)
            .unwrap_or_else(|| panic!("the refusal is not in the file at all: {written}"));
        let here_at = written.find(here).unwrap_or_else(|| {
            panic!("the window never said where it was logging, so there is nothing to order against: {written}")
        });
        assert!(
            note_at < here_at,
            "the window wrote into the file before saying what the file may contain: {written}"
        );
    }

    #[test]
    #[ignore = "driven by its outer tests, which set the environment on this child alone"]
    fn a_child_window_that_installs_the_real_filter() {
        let path = install().expect("the window opened its log");
        as_the_terminal_crates_would(SENTINEL);
        println!("{NAMED_ITS_LOG}{}", path.display());
        println!("{CHILD_OK}");
    }

    #[test]
    fn an_event_reaches_the_file_and_not_only_stderr() {
        // The one thing about this module that could fail silently. `install` cannot be
        // called from a test — `.init()` is global and once per process — so the writer it
        // builds is built here instead, through the same function, and asked whether an event
        // actually landed. A packaged Windows build has no stderr at all, so "it appeared in
        // the terminal" is not evidence of anything.
        let dir = std::env::temp_dir().join(format!("nysia-window-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("nysiad.window.log");

        let file = log_file::open_for_append(&path).expect("a log");
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink(file))
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(verb = "stream_attach", stream = 4, "served");
        });

        let written = std::fs::read_to_string(&path).expect("the log");
        assert!(
            written.contains("stream_attach") && written.contains("stream=4"),
            "the event did not reach the window's log file: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_window_log_is_rotated_by_the_same_policy_as_the_daemons() {
        // Not a second cap with the same numbers by coincidence — literally `log_file`'s, so
        // the two files cannot drift into different retention.
        let dir = std::env::temp_dir().join(format!("nysia-window-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("nysiad.window.log");

        std::fs::File::create(&path)
            .expect("a log")
            .set_len(log_file::MAX_LOG_BYTES + 1)
            .expect("a size over the cap");
        let mut opened = log_file::open_for_append(&path).expect("a log");
        opened.write_all(b"this window\n").expect("a write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("the log"),
            "this window\n"
        );
        assert!(log_file::rotation_path(&path, 1).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
