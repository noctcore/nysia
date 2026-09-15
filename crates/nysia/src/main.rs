//! `nysia` — the daemon and the CLI, in one binary, selected by argv (D-11).
//!
//! The GUI and the daemon are deliberately allowed to run different versions, because that
//! is what an in-place upgrade requires; the versioned socket handshake is what makes it
//! safe. This binary ships inside the app bundle and is symlinked onto `PATH`.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | the process did what was asked |
//! | 1 | it was asked for something valid and could not do it |
//! | 2 | clap's usage error — you typed it wrong |
//!
//! There used to be a third — "parsed and routed, but this build has no implementation" —
//! and `nysia hook` was the only thing that ever answered with it. It is gone because nothing
//! answers with it any more: every verb this binary routes, it serves. A code documented for
//! a case that cannot arise is a code somebody writes a branch for.
//!
//! **`nysia hook` never exits 2.** Claude reads exit 2 from a hook as "block, and feed stderr
//! back to the model", which is the influence §5.2 exists to make impossible; the hook's own
//! failures are exit 1. Clap's usage exit is the one route to a 2, and it is reachable only
//! by a hook entry whose argv is malformed.

mod cli;
mod daemon;
mod hook;
mod verbs;

use std::io::Write;
use std::process::ExitCode;

use crate::cli::{Cli, Mode};

/// The verb was asked for something valid and it did not work. The error envelope on stderr
/// says what and what to do about it.
const EXIT_FAILED: u8 = 1;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NYSIA_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        // Logs never go to stdout. Stdout is the verb's result, and a log line in the middle
        // of it is a parse error for whoever is reading.
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse_from_argv(std::env::args_os()).unwrap_or_else(|err| err.exit());
    let no_spawn = cli.no_spawn;
    match cli.into_mode() {
        Mode::Daemon { never_retire } => match daemon::run(never_retire).await {
            Ok(daemon::Outcome::Retired) => ExitCode::SUCCESS,
            Ok(daemon::Outcome::AlreadyRunning { endpoint }) => {
                // Losing the race is not a failure. What this process was started to
                // guarantee — that a daemon is listening there — is true.
                let _ = writeln!(
                    std::io::stderr(),
                    "a daemon is already listening on {endpoint}; leaving it alone"
                );
                ExitCode::SUCCESS
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "nysiad could not start: {err}");
                ExitCode::from(EXIT_FAILED)
            }
        },
        Mode::Client(verb) => {
            let json = verb.json();
            match verbs::run(*verb, no_spawn).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    verbs::print_error(&err, json);
                    ExitCode::from(EXIT_FAILED)
                }
            }
        }
        Mode::Hook(args) => {
            // `{}` is already on its way by the time this returns, whatever the answer is.
            // `true` means the status was delivered, spooled, or specified to be dropped;
            // `false` means the hook was asked for something it could not do.
            if hook::run(&args).await {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_FAILED)
            }
        }
    }
}
