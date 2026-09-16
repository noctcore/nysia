//! The subscriber filter, and the confinement the user cannot switch off.
//!
//! One function, because the interesting part is not the level — it is that
//! [`nysia_core::rpc::log_file::CONFINED_TARGETS`] is applied **after** whatever `NYSIA_LOG`
//! asked for, so raising the level to debug something does not also switch off the rule that
//! keeps terminal bytes out of the file.
//!
//! This binary is the one that matters most for that: under D-1 and D-7 the daemon owns every
//! PTY and the terminal state, so it is the process in which `vte`, `alacritty_terminal` and
//! `portable_pty` actually run. The window applies the same list — see `nysia-desktop`'s `log`
//! module — for the same reason a lock on one door is not a policy.

use nysia_core::rpc::log_file;
use tracing_subscriber::EnvFilter;

/// The filter this process runs: `NYSIA_LOG` if it parses, `info` if not, confined either way.
///
/// The fallback is `info` rather than a refusal because a malformed `NYSIA_LOG` is a typo, and
/// a daemon that would not start over one would be worse than a daemon that logs at its
/// default. The confinement is applied to the fallback too — it is not conditional on the
/// user having asked for anything.
///
/// A directive in [`log_file::CONFINED_TARGETS`] that does not parse is skipped rather than
/// panicking here, which would be a crash on a path nobody can fix at runtime;
/// `every_confined_directive_parses` is what stops one reaching a release, because a skipped
/// directive is a confinement silently absent.
pub fn filter() -> EnvFilter {
    confine(EnvFilter::try_from_env("NYSIA_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
}

/// Layer the confinement over a filter, whatever that filter says.
///
/// Split out so the tests can put the same question to a filter they built themselves, and so
/// the "without this" case can be written down as a test rather than imagined.
fn confine(filter: EnvFilter) -> EnvFilter {
    log_file::CONFINED_TARGETS
        .iter()
        .filter_map(|directive| directive.parse().ok())
        .fold(filter, EnvFilter::add_directive)
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

    /// The two lines the terminal crates emit, as `tracing` sees them.
    ///
    /// `vte` and `alacritty_terminal` call the `log` crate, which `tracing-log` converts into
    /// events carrying the module path as the target and the level unchanged — so an event
    /// raised here on the same target at the same level is what the filter is asked about in
    /// production. Raising them directly is what lets both halves of the claim be tested in
    /// one process; `the_vt_really_does_hand_those_bytes_to_log` covers the other end, that
    /// the bytes reach that call at all.
    fn as_the_terminal_crates_would(sentinel: &str) {
        tracing::debug!(target: "vte::ansi", "[unhandled osc_dispatch]: [{sentinel}]");
        tracing::trace!(target: "alacritty_terminal::term", "Setting title to '{sentinel}'");
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
            "this leg's leak was expected from {LEAKING_TARGET} and came from elsewhere.              `portable_pty` may have moved the call site; `log_file`'s docs cite it by file              and line and would need the same correction: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_spawns_path_does_not_reach_a_confined_log() {
        // The rule as the thing that can be observed. Not that a directive is in a list — that
        // is reading — but that the line is not in the file.
        //
        // Three filters, because applying the confinement after `NYSIA_LOG` is the whole
        // point. `info` is what ships. `trace` is what somebody sets to debug the spawn that
        // just failed, which is the moment before they send the file to somebody else.
        // `portable_pty=trace` is that person naming the target directly, which only fails if
        // a later `add_directive` replaces an equally specific one.
        bridge_log_to_tracing();

        for asked in ["info", "trace", "portable_pty=trace"] {
            let (marker, dir) = a_marked_directory("confined");

            let written = through(confine(EnvFilter::new(asked)), || {
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
        // Both spellings, because they fail for different reasons. `debug` is a bare level,
        // which a target-specific directive out-specifies. `vte=trace` names the same target,
        // so it is only beaten if a later `add_directive` *replaces* an equally specific one.
        for asked in [
            "debug",
            "trace",
            "vte=trace",
            "vte=trace,alacritty_terminal=trace",
        ] {
            let written = through(confine(EnvFilter::new(asked)), || {
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
        // without exercising anything is worse than no check, and every assertion above is
        // `!contains` — which a typo in a target name would also satisfy.
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
        let written = through(confine(EnvFilter::new("debug")), || {
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
            "portable_pty is held off; at warn this ERROR would have gone straight through:              {written}"
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

    #[test]
    fn the_vt_really_does_hand_those_bytes_to_log() {
        // The end the filter tests cannot reach: that an OSC the parser does not handle gets
        // as far as `vte`'s `debug!` at all, so the confinement is guarding something real
        // rather than a target nothing writes to.
        //
        // It is asserted through the VT's own state rather than through the log, because the
        // `log`-to-`tracing` bridge is a global install and this crate has no direct
        // dependency on `tracing-log` with which to put one up for one test. What this holds
        // is the reachability half: the bytes go in, `vte` does not recognise the OSC, and
        // nothing in Nysia's own output records them. `vte-0.15.0/src/ansi.rs:1341` is what
        // does, and it is quoted in `log_file`'s docs with its file and line.
        let mut terminal = TerminalState::new(TerminalSize::new(80, 24), VtConfig::default());
        let osc = format!("\x1b]9001;CmdNotFound;{SENTINEL}\x07");

        let written = through(confine(EnvFilter::new("trace")), || {
            terminal.feed(osc.as_bytes());
        });

        assert!(
            !written.contains(SENTINEL),
            "feeding an unhandled OSC through the VT put its bytes in the log: {written}"
        );
    }
}
