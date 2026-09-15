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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nysia_core::rpc::{Endpoint, log_file};
use tracing_subscriber::fmt::writer::MakeWriterExt;

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
    let filter = tracing_subscriber::EnvFilter::try_from_env("NYSIA_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

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
            // `Arc<File>` is a `MakeWriter` because `&File` is `io::Write`, and `.and` layers
            // the two sinks. Every event therefore reaches the file *and* the terminal a
            // developer is watching, rather than one or the other depending on the build.
            let writer = Arc::new(file).and(std::io::stderr);
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
