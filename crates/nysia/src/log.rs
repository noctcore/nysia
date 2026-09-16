//! The subscriber filter, and the confinement `NYSIA_LOG` cannot switch off.
//!
//! Two halves, because one of them was not enough on its own.
//! [`nysia_core::rpc::log_file::CONFINED_TARGETS`] is folded in **after** whatever `NYSIA_LOG`
//! asked for, so raising the level to debug something does not also switch off the rule that
//! keeps terminal bytes out of the file. And `NYSIA_LOG` goes through
//! [`log_file::screen_directives`] first, so *naming a module* does not either. Raising the
//! level never undid the rule; naming a module did, and `NYSIA_LOG=vte::ansi=trace` is the
//! spelling somebody reaches for while debugging the very subsystem whose bytes leak.
//!
//! [`log_file::UNCONFINED_ENV`] is the way out and says so in its name. `unconfined` below is
//! the only function that honours it, and `filter` does not take that path unless the variable
//! is set — which is CLAUDE.md §6's *separately named entry point, obvious in review and
//! greppable*.
//!
//! This binary is the one that matters most for all of it: under D-1 and D-7 the daemon owns
//! every PTY and the terminal state, so it is the process in which `vte`, `alacritty_terminal`
//! and `portable_pty` actually run. The window applies the same screen and the same list — see
//! `nysia-desktop`'s `log` module, which holds its own copy with its own tests — for the same
//! reason a lock on one door is not a policy.
//!
//! # Where a refused directive is announced
//!
//! On stderr, which for a spawned daemon is the log file itself and for `nysia` run by hand is
//! the terminal. `filter` is called before any subscriber exists, so there is nowhere else for
//! it to go, and it is where `EnvFilter` already reports a directive it could not parse.
//! Stdout is never touched: that is the verb's result. Under D-11 this binary is also the CLI,
//! so somebody who has exported a refused `NYSIA_LOG` sees the line on every invocation —
//! which is the right way round, because the alternative is a directive that silently does
//! nothing and a person who concludes the log is broken.

use std::io::Write;

use nysia_core::rpc::log_file;
use tracing_subscriber::EnvFilter;

/// What this process logs when `NYSIA_LOG` asks for nothing this filter can use.
///
/// `info` rather than a refusal to start, because a malformed `NYSIA_LOG` is a typo and a
/// daemon that would not come up over one would be worse than a daemon at its default.
///
/// It is also where a value whose every directive was refused lands, and that case is why this
/// is written down at all rather than left inline: an `EnvFilter` built from an empty string
/// enables nothing, so a screen that dropped everything and handed the remainder straight on
/// would answer `NYSIA_LOG=vte::ansi=trace` with a silent daemon.
/// `refusing_every_directive_still_leaves_nysias_own_lines` is that as a test.
const DEFAULT_DIRECTIVES: &str = "info";

/// The filter this process runs: `NYSIA_LOG`, screened and confined.
///
/// The confinement applies to the fallback too — it is not conditional on the user having
/// asked for anything.
///
/// The branch below is the line that switches the confinement off for every shipped run of
/// this binary, so it is held by two checks that put it in a process of its own rather than
/// calling it here — reading the environment is not something a test that shares one with
/// every other test in the binary can do. `the_runtime_branch_confines_unless_the_way_out_is_named`
/// re-execs this test binary at a child that installs exactly the subscriber `main` installs;
/// `crates/nysia/tests/log_confinement.rs` drives the shipped `nysia` binary and reads the
/// log file a daemon actually writes.
///
/// A directive in [`log_file::CONFINED_TARGETS`] that does not parse is skipped rather than
/// panicking here, which would be a crash on a path nobody can fix at runtime;
/// `every_confined_directive_parses` is what stops one reaching a release, because a skipped
/// directive is a confinement silently absent.
pub fn filter() -> EnvFilter {
    let asked = std::env::var(log_file::LOG_ENV).unwrap_or_default();
    if log_file::confinement_lifted() {
        note(&log_file::unconfined_note());
        return unconfined(&asked);
    }
    // Screened once and then handed on, rather than screened again inside [`confine`]. The
    // two calls cannot disagree — it is a pure function of the same string — but a reader
    // had to establish that before they could be sure the line being reported was the line
    // being dropped.
    let screened = log_file::screen_directives(&asked);
    for refused in &screened.refused {
        note(&log_file::refusal_note(refused));
    }
    confine(&screened)
}

/// What survived [`log_file::screen_directives`], with [`log_file::CONFINED_TARGETS`] folded
/// in behind it.
///
/// Both halves, and they are not interchangeable. The screen is what stops a directive
/// out-specifying an entry; the fold is what holds those targets down when nothing was asked
/// about them at all, which is every ordinary run.
///
/// Takes a [`log_file::Screened`] rather than a filter somebody else already built, and that
/// is the shape the fix needed: a directive has to be refused before `EnvFilter` has resolved
/// a callsite against it, and once one is inside an `EnvFilter` there is no way to take it
/// back out. Split out from [`filter`] so the tests can put the same question to a value they
/// wrote themselves, and so the "without this" case can be written down as a test rather than
/// imagined.
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
/// reviewer finds when they ask how the confinement can be off (CLAUDE.md §6). It is the
/// answer to "what is a developer who legitimately needs `vte` output expected to do" — a
/// confinement with no way out at all is its own kind of failure — and it is deliberately not
/// reachable by any spelling of `NYSIA_LOG`, however specific.
///
/// The log it produces can carry terminal output, a window title, an environment block and the
/// working directory of a spawn: everything `log_file`'s module docs say a Nysia log never
/// contains. [`filter`] says so, into that same log, before anything else is written to it.
///
/// Building the filter and saying what it is are separate here for the same reason the screen
/// is separate from the fold: a function that both builds and announces is one somebody can
/// call without the announcement, and this is the one it would be worst to call quietly.
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

/// Say something about the filter, before there is a subscriber to say it through.
///
/// The wording comes from `log_file` so that this process and the window cannot drift into
/// explaining the same refusal differently. See the module docs for why this is stderr and
/// not `tracing`: at the point [`filter`] runs there is no subscriber, and for a spawned
/// daemon stderr is the log file anyway.
///
/// `writeln!` and not `eprintln!`, which panics when the write fails. The one thing this
/// line is for is telling somebody why their `NYSIA_LOG` did nothing, and a process that
/// died over saying it would have answered a refused directive with no daemon at all.
/// `main` writes every one of its own stderr lines the same way, for the same reason.
fn note(what: &str) {
    let _ = writeln!(std::io::stderr(), "{what}");
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, Once};

    use nysia_core::pty::{PtySession, SessionSpec};
    use nysia_core::vt::{TerminalSize, TerminalState, VtConfig};
    use tracing_subscriber::util::SubscriberInitExt;

    use super::*;

    /// A sentinel no terminal would produce and no test could mistake for anything else.
    const SENTINEL: &str = "ZZZ-DBG-SENTINEL-4f3e9a";

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("the capture buffer")).into_owned()
        }
    }

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("the capture buffer")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Everything `tracing` wrote through `filter` while `body` ran.
    ///
    /// A thread-local subscriber, never a global one: the two cases that matter here are "with
    /// the confinement" and "without it", and a global subscriber can only be installed once
    /// per process — which would leave the proof that the guard is load-bearing unwritable.
    fn through(filter: EnvFilter, body: impl FnOnce()) -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, body);
        captured.text()
    }

    /// The filter as it was before the screen: `NYSIA_LOG` parsed whole, the list folded in.
    ///
    /// This is the control, and it is the shape the bug had. Every assertion about the screen
    /// below is a `!contains`, and a `!contains` is satisfied just as well by a typo in a
    /// target name, a missing `log` bridge, or a level that was never going to be enabled.
    /// Each test that uses this asserts the *positive* through it first, **in the same test
    /// body**, so that filtering the run down to one test name cannot leave a vacuous pass
    /// behind — which is traps register #12, and the thing #101's review noted about a control
    /// that lives in a test of its own.
    fn as_it_was_before_the_screen(asked: &str) -> EnvFilter {
        log_file::CONFINED_TARGETS
            .iter()
            .filter_map(|directive| directive.parse().ok())
            .fold(EnvFilter::new(asked), EnvFilter::add_directive)
    }

    /// The whole of what [`filter`] does to a `NYSIA_LOG` value, from the value.
    ///
    /// [`confine`] takes what [`log_file::screen_directives`] already returned, because
    /// `filter` screens once and reports the refusals out of the same answer it filters by.
    /// Every test below writes the string rather than the split, so the screen is done here.
    fn confined(asked: &str) -> EnvFilter {
        confine(&log_file::screen_directives(asked))
    }

    /// One line on each target [`log_file::CONFINED_TARGETS`] holds down, as `tracing` sees it.
    ///
    /// `vte`, `alacritty_terminal` and `portable_pty` all call the `log` crate, which
    /// `tracing-log` converts into events carrying the module path as the target and the level
    /// unchanged — so an event raised here on the same target at the same level is what the
    /// filter is asked about in production. Raising them directly is what lets one filter be
    /// asked about four call sites in one process and on either platform.
    ///
    /// Each level is the one the real call site uses, because the level is half of what an
    /// entry has to get right: `alacritty_terminal` is held to `warn`, so a `trace!` there has
    /// to be dropped while a `warn!` survives.
    ///
    /// The two ends this cannot reach have tests that raise nothing:
    /// `the_vt_hands_an_unhandled_osc_to_log_and_the_confinement_stops_it` feeds real bytes
    /// through the real parser, and `a_failed_spawn_writes_a_caller_offered_path_without_the_confinement`
    /// plants a real spawn.
    fn as_the_terminal_crates_would(sentinel: &str) {
        tracing::debug!(target: "vte::ansi", "[unhandled osc_dispatch]: [{sentinel}]");
        tracing::trace!(target: "alacritty_terminal::term", "Setting title to '{sentinel}'");
        tracing::error!(target: "portable_pty::win::pseudocon", "CreateProcessW `{sentinel}` failed");
        tracing::warn!(target: "portable_pty::cmdbuilder", "$SHELL -> {sentinel} not executable");
    }

    /// Install the `log`-to-`tracing` bridge the daemon runs with, once for this process.
    ///
    /// `vte`, `alacritty_terminal` and `portable_pty` all call the `log` crate, and nothing
    /// they write reaches a `tracing` subscriber until a `LogTracer` is installed. The daemon
    /// gets one from `tracing_subscriber`'s `.init()` in `main`, which is global and can
    /// happen once per process — so a test cannot call the real installer and has to stand the
    /// same bridge up itself. The subscriber installed here writes to `io::sink` and exists
    /// only to carry the bridge; every test puts its own in front of it with `with_default`,
    /// which is thread-local and wins.
    ///
    /// The `expect` is deliberate and is the point of the function. Swallowing the error would
    /// leave the bridge absent, and then every `!contains` assertion below would pass against
    /// a subscriber that was never offered the line — the exact shape of check traps register
    /// #12 is about.
    fn bridge_log_to_tracing() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new("trace"))
                .with_writer(std::io::sink)
                .finish()
                .try_init()
                .expect("a log-to-tracing bridge");
        });
    }

    /// Which `portable_pty` call site this leg's planted spawn actually reaches.
    ///
    /// Said out loud rather than left to a `!contains` that would be true on either leg for
    /// either reason. Windows fails inside `CreateProcessW` and `pseudocon` writes the cwd at
    /// `ERROR`; Unix never gets that far, because `cmdbuilder` resolves `$SHELL` first and
    /// writes it at `WARN`. Two call sites, two platforms, one rule — and both levels clear
    /// the shipped `info` default.
    const LEAKING_TARGET: &str = if cfg!(windows) {
        "portable_pty::win::pseudocon"
    } else {
        "portable_pty::cmdbuilder"
    };

    /// A directory whose name no other test in this repo could produce.
    ///
    /// Tagged and suffixed with the pid the way `log_file`'s own `scratch` is, so two tests
    /// planting spawns at the same time cannot delete each other's.
    fn a_marked_directory(tag: &str) -> (String, PathBuf) {
        let marker = format!("ZZZ-SPAWN-{tag}-{}", std::process::id());
        let dir = std::env::temp_dir().join(&marker);
        let _ = std::fs::create_dir_all(&dir);
        (marker, dir)
    }

    /// Fail a spawn in the two ways that hand `portable_pty` a caller-offered path.
    ///
    /// A real [`PtySession::spawn`], never a `tracing::error!` raised on the crate's target:
    /// the question is whether *the crate* still writes, and an event this module raised would
    /// be just as true of a target nothing in the process ever calls.
    ///
    /// Both values are ones a `SessionCreate` carries — `cwd` directly, and `SHELL` as an
    /// `envOverrides` entry, which survives because `SHELL` is not one of
    /// `nysia_core::pty::SCRUBBED_VARS`. The program is a name no `PATH` holds, so the spawn
    /// cannot succeed and no process is ever started.
    fn plant_a_failed_spawn(dir: &std::path::Path) {
        let spec = SessionSpec::for_program(
            [OsString::from("nysia-no-such-program-4f3e9a")],
            "a spawn that cannot succeed",
        )
        .expect("a spec")
        .with_cwd(dir)
        .with_env("SHELL", dir.join("not-an-executable"));
        assert!(
            PtySession::spawn(spec).is_err(),
            "the plant started a process; it is supposed to fail"
        );
    }

    /// Feed the VT an OSC its parser does not handle, carrying `sentinel`.
    ///
    /// `9001` is not a sequence `vte` recognises, so it reaches the `unhandled` arm and every
    /// byte of every parameter is written to the log.
    fn feed_an_unhandled_osc(sentinel: &str) {
        let mut terminal = TerminalState::new(TerminalSize::new(80, 24), VtConfig::default());
        terminal.feed(format!("\x1b]9001;{sentinel}\x07").as_bytes());
    }

    /// `sentinel` spelled the way `vte` prints it when it does not handle an OSC.
    ///
    /// `vte-0.15.0/src/ansi.rs:1341` writes each byte of each parameter through `{:?}` as a
    /// `char`, so `ZZZ` arrives in the log as `'Z''Z''Z'` and a plain `contains(sentinel)`
    /// would be false however loudly the line had been written. Rendering it the same way here
    /// is what stops the positive half of that test passing for the wrong reason, and what
    /// would notice if the crate ever stopped printing the parameters at all.
    fn as_vte_prints_it(sentinel: &str) -> String {
        sentinel.chars().map(|byte| format!("{byte:?}")).collect()
    }

    #[test]
    fn a_module_qualified_nysia_log_does_not_reach_the_log() {
        // The reproduction, and the whole of why the screen exists. `EnvFilter` resolves a
        // callsite against its *longest* matching target, and `CONFINED_TARGETS` names crates,
        // so a directive naming a module out-specifies the entry holding that crate down.
        //
        // `vte:` and `vte::` are on the list because they are not module paths and name
        // nothing, and they still win: the match is
        // `metadata.target().starts_with(directive_target)` and the tie-break is target
        // length, so four characters beat three. A screen written against `::` boundaries
        // would pass every other case here and let those two through, which is why they are
        // here at all.
        //
        // Each case carries its own control in this same body: the same string through
        // `as_it_was_before_the_screen` has to put the sentinel in the log, or the assertion
        // after it proves nothing.
        for asked in [
            "vte::ansi=trace",
            "vte:=trace",
            "vte::=trace",
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
                "NYSIA_LOG={asked} out-specified the confinement: {written}"
            );
        }
    }

    #[test]
    fn a_span_directive_does_not_reach_the_log() {
        // The second way out, and a different hole rather than a wider version of the first.
        // `EnvFilter::enabled` asks its *dynamic* directives first and returns `true` straight
        // out of the span scope before it has consulted a single target directive — so a span
        // directive enables every target at its level however `CONFINED_TARGETS` is spelled,
        // and the span it names need have nothing to do with a terminal.
        //
        // The second spelling is the one worth having. It names a target of Nysia's own and
        // a span of Nysia's own, so it looks like a directive about the daemon and nothing
        // else — and it still hands over every byte `vte` printed, because the span scope is
        // consulted before any target is. Its target has to match the span's own, which is
        // this module's path, so if that moves the control below says so rather than passing
        // quietly.
        //
        // Control in the same body, for the same reason as above.
        for asked in ["[serving]=trace", "nysia[serving]=trace"] {
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
    fn the_confinement_is_lifted_only_by_its_own_entry_point() {
        // Two claims that are the same claim from either side, so they share a body.
        // `unconfined` really does hand over what `confine` refuses — a way out that did not
        // work would be a refusal with extra words, and nothing else here calls it — and
        // `confine` really does refuse the same string.
        //
        // `filter` is what chooses between the two, on `log_file::confinement_lifted`, and it
        // is not called here: that reads the process environment, which every test in this
        // binary shares. `the_confinement_is_not_lifted_unless_its_own_variable_is_set`, in
        // `log_file`'s own tests, is what holds the default to the confined branch.
        let asked = "vte::ansi=trace";

        let lifted = through(unconfined(asked), || as_the_terminal_crates_would(SENTINEL));
        assert!(
            lifted.contains(SENTINEL),
            "the way out does not let anything out, so there is no way out: {lifted}"
        );

        let held = through(confined(asked), || as_the_terminal_crates_would(SENTINEL));
        assert!(
            !held.contains(SENTINEL),
            "the same string was honoured without anybody naming the way out: {held}"
        );
    }

    #[test]
    fn refusing_every_directive_still_leaves_nysias_own_lines() {
        // A screen that answered `NYSIA_LOG=vte::ansi=trace` with a silent daemon would have
        // traded one bug for a worse one. Every directive in that value is refused, so nothing
        // is left to parse — and what has to happen then is the shipped default, not the empty
        // `EnvFilter` an empty string builds, which enables nothing at all.
        let written = through(confined("vte::ansi=trace"), || {
            tracing::info!(target: "nysia_core::rpc::server", verb = "session_create", "served");
            tracing::debug!(target: "nysia_core::rpc::server", "below the default");
        });

        assert!(
            written.contains("session_create"),
            "refusing the only directive left the daemon with no log at all: {written}"
        );
        assert!(
            !written.contains("below the default"),
            "the fallback is `info`, not `debug`: {written}"
        );
    }

    #[test]
    fn the_vt_hands_an_unhandled_osc_to_log_and_the_confinement_stops_it() {
        // The end no raised event can reach: that the bytes get as far as `vte`'s own `debug!`
        // at all, so the confinement is guarding something real rather than a target nothing
        // in this process ever writes to. Real bytes, the real parser, the real filter, and
        // `NYSIA_LOG=vte::ansi=trace` — the string the reproduction used — end to end.
        //
        // This used to be asserted through the VT's own state instead, on the grounds that
        // "this crate has no direct dependency on `tracing-log` with which to put one up for
        // one test". That was true of the manifest and misleading about the module:
        // `bridge_log_to_tracing` above stands exactly that bridge up, through
        // `tracing-subscriber`'s own `tracing-log` feature, so the line can be read back
        // rather than reasoned about.
        bridge_log_to_tracing();
        let printed = as_vte_prints_it(SENTINEL);

        let leaked = through(as_it_was_before_the_screen("vte::ansi=trace"), || {
            feed_an_unhandled_osc(SENTINEL);
        });
        assert!(
            leaked.contains(&printed),
            "the parser wrote nothing, so the assertion below would hold whatever the filter \
             did — `vte` may have stopped printing OSC parameters, or the `log` bridge may be \
             absent: {leaked}"
        );

        let written = through(confined("vte::ansi=trace"), || {
            feed_an_unhandled_osc(SENTINEL);
        });
        assert!(
            !written.contains(&printed),
            "an unhandled OSC's bytes reached a confined log: {written}"
        );
        assert!(
            !written.contains("vte::ansi"),
            "`vte` is held off and wrote anyway: {written}"
        );
    }

    #[test]
    fn a_failed_spawn_writes_a_caller_offered_path_without_the_confinement() {
        // The load-bearing half, and the only thing standing between the test below and a
        // vacuous pass (traps register #12): every assertion there is a `!contains`, which a
        // typo in a target name, a missing `log` bridge, or a spawn that quietly succeeded
        // would each satisfy on their own.
        //
        // `path.rs`'s `a_short_name_expands_to_the_long_one` is the anti-pattern here, so this
        // says which leg exercises what instead of leaving it to be inferred: the assertion on
        // `LEAKING_TARGET` names the module that has to have written the line, and it is a
        // different module on each platform.
        //
        // `info` is the shipped default with no `NYSIA_LOG` at all, so what this shows is not
        // a debug-only leak. It is what the daemon wrote into its file on an ordinary day.
        bridge_log_to_tracing();
        let (marker, dir) = a_marked_directory("unconfined");

        let written = through(EnvFilter::new("info"), || plant_a_failed_spawn(&dir));

        assert!(
            written.contains(&marker),
            "nothing wrote this path, so the confined case proves nothing: {written}"
        );
        assert!(
            written.contains(LEAKING_TARGET),
            "this leg's leak was expected from {LEAKING_TARGET} and came from elsewhere; \
             `portable_pty` may have moved the call site, which `log_file`'s docs cite by \
             file and line: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_spawns_path_does_not_reach_a_confined_log() {
        // The rule as the thing that can be observed. Not that a directive is in a list — that
        // is reading — but that the line is not in the file.
        //
        // **Depends on `a_failed_spawn_writes_a_caller_offered_path_without_the_confinement`**,
        // which is the positive half and is a test of its own because it is about the shipped
        // default rather than about any `NYSIA_LOG`. Running this one alone proves less than
        // it looks: the two are one argument.
        //
        // Five filters, because *when* the confinement is applied is the whole point. `info`
        // is what ships. `trace` is what somebody sets to debug the spawn that just failed,
        // which is the moment before they send the file to somebody else. `portable_pty=trace`
        // is that person naming the crate, and the last two are them naming the module that
        // actually wrote the line — which is what used to work, on one leg each.
        bridge_log_to_tracing();

        for asked in [
            "info",
            "trace",
            "portable_pty=trace",
            "portable_pty::win::pseudocon=trace",
            "portable_pty::cmdbuilder=trace",
        ] {
            let (marker, dir) = a_marked_directory("confined");

            let written = through(confined(asked), || {
                plant_a_failed_spawn(&dir);
            });

            assert!(
                !written.contains(&marker),
                "NYSIA_LOG={asked}: a path the caller offered reached the log: {written}"
            );
            // Not only the marker. On Windows `portable_pty::cmdbuilder` prints the whole
            // inherited environment at `trace`, name and value, and carries no marker at all —
            // the same rule broken by a different line. `off` means the crate wrote nothing,
            // so that is what is asserted.
            assert!(
                !written.contains("portable_pty"),
                "NYSIA_LOG={asked}: `portable_pty` is held off and wrote anyway: {written}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_confinement_survives_a_user_who_asks_for_everything() {
        // CLAUDE.md §6, as a test. `NYSIA_LOG` used to replace the filter wholesale, so a
        // person raising the level to debug something switched off a confinement they did not
        // know existed — at exactly the moment they were most likely to send somebody the
        // file.
        //
        // **Depends on `without_the_confinement_those_bytes_reach_the_file`** for its positive
        // half, which is a test of its own because it is about the unguarded filter rather
        // than about any one of these strings.
        //
        // The spellings fail for three different reasons. `debug` is a bare level, which any
        // target-specific directive out-specifies, so the fold alone answers it. `vte=trace`
        // names the same target the fold does, and used to be answered by `add_directive`
        // replacing an equally specific directive — the screen now refuses it before that ever
        // comes up, which is why nothing here still rests on that replacement. `vte::ansi=trace`
        // is the module-qualified form neither of those answers, and
        // `a_module_qualified_nysia_log_does_not_reach_the_log` is where it is measured against
        // its own control.
        for asked in [
            "debug",
            "trace",
            "vte=trace",
            "vte=trace,alacritty_terminal=trace",
            "vte::ansi=trace,alacritty_terminal::term=trace",
        ] {
            let written = through(confined(asked), || {
                as_the_terminal_crates_would(SENTINEL);
            });
            assert!(
                !written.contains(SENTINEL),
                "NYSIA_LOG={asked} undid the confinement: {written}"
            );
        }
    }

    #[test]
    fn without_the_confinement_those_bytes_reach_the_file() {
        // The proof that the guard is load-bearing (traps register #12). A check that passes
        // without exercising anything is worse than no check, and every assertion in
        // `the_confinement_survives_a_user_who_asks_for_everything` is `!contains` — which a
        // typo in a target name would also satisfy. That test is this one's other half.
        let written = through(EnvFilter::new("debug"), || {
            as_the_terminal_crates_would(SENTINEL);
        });
        assert!(
            written.contains(SENTINEL),
            "the unguarded filter dropped these too, so the test above proves nothing: {written}"
        );
    }

    #[test]
    fn each_target_is_held_to_the_level_it_names_and_nothing_else_is() {
        // The other way a confinement goes wrong: holding so much that the log stops being
        // worth reading. Nysia's own targets are untouched at whatever level was asked for,
        // and `alacritty_terminal` keeps the errors it is worth keeping.
        //
        // The `portable_pty` line is the one that decided that entry's spelling. It is an
        // `ERROR`, and `warn` — which is what `alacritty_terminal` is held to — permits
        // `ERROR`, so copying that spelling would have confined nothing at all.
        let written = through(confined("debug"), || {
            tracing::debug!(target: "nysia_core::rpc::server", verb = "session_create", "served");
            tracing::warn!(target: "alacritty_terminal::term", "a real fault");
            tracing::error!(target: "portable_pty::win::pseudocon", "a spawn in some cwd");
        });
        assert!(written.contains("session_create"), "{written}");
        assert!(
            written.contains("a real fault"),
            "alacritty_terminal is held to warn, not silenced: {written}"
        );
        assert!(
            !written.contains("a spawn in some cwd"),
            "portable_pty is held off; at warn this ERROR would have gone through: {written}"
        );
    }

    #[test]
    fn a_directive_that_is_none_of_the_confinements_business_is_left_alone() {
        // The screen refuses by target, so what needs checking is that it refuses *only* by
        // target. Somebody debugging the daemon writes three directives and one of them cannot
        // be honoured; the other two are what they were actually trying to do, and they have
        // to arrive intact and still mean what they said.
        let written = through(
            confined("info,nysia_core::rpc::server=trace,vte::ansi=trace"),
            || {
                tracing::trace!(target: "nysia_core::rpc::server", verb = "session_create", "served");
                as_the_terminal_crates_would(SENTINEL);
            },
        );

        assert!(
            written.contains("session_create"),
            "a directive with nothing to do with the confinement was dropped with it: {written}"
        );
        assert!(
            !written.contains(SENTINEL),
            "the refused directive was honoured anyway: {written}"
        );
    }

    #[test]
    fn every_confined_directive_parses() {
        // `confine` skips a directive it cannot parse rather than panicking in a daemon, so a
        // typo would be a confinement that is silently absent. This is what makes the skip
        // safe.
        //
        // It is also the limit of what a parse can tell you, which is why the list is held by
        // a planted spawn as well: `portable-pty=off` parses perfectly and confines nothing,
        // because an `EnvFilter` target is a Rust path and that crate's is spelled with an
        // underscore. `a_failed_spawns_path_does_not_reach_a_confined_log` is the check that
        // would notice.
        for directive in log_file::CONFINED_TARGETS {
            assert!(
                directive
                    .parse::<tracing_subscriber::filter::Directive>()
                    .is_ok(),
                "{directive} is not a directive `EnvFilter` accepts, so it would be skipped"
            );
        }
    }
}
