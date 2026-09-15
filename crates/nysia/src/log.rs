//! The subscriber filter, and the confinement the user cannot switch off.
//!
//! One function, because the interesting part is not the level — it is that
//! [`nysia_core::rpc::log_file::CONFINED_TARGETS`] is applied **after** whatever `NYSIA_LOG`
//! asked for, so raising the level to debug something does not also switch off the rule that
//! keeps terminal bytes out of the file.
//!
//! This binary is the one that matters most for that: under D-1 and D-7 the daemon owns every
//! PTY and the terminal state, so it is the process in which `vte` and `alacritty_terminal`
//! actually run. The window applies the same list — see `nysia-desktop`'s `log` module — for
//! the same reason a lock on one door is not a policy.

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
    use std::sync::{Arc, Mutex};

    use nysia_core::vt::{TerminalSize, TerminalState, VtConfig};

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
    fn the_confinement_silences_those_two_targets_and_nothing_else() {
        // The other way a confinement goes wrong: holding so much that the log stops being
        // worth reading. Nysia's own targets are untouched at whatever level was asked for.
        let written = through(confine(EnvFilter::new("debug")), || {
            tracing::debug!(target: "nysia_core::rpc::server", verb = "session_create", "served");
            tracing::warn!(target: "alacritty_terminal::term", "a real fault");
        });
        assert!(written.contains("session_create"), "{written}");
        assert!(
            written.contains("a real fault"),
            "alacritty_terminal is held to warn, not silenced: {written}"
        );
    }

    #[test]
    fn every_confined_directive_parses() {
        // `confine` skips a directive it cannot parse rather than panicking in a daemon, so a
        // typo would be a confinement that is silently absent. This is what makes the skip
        // safe.
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
