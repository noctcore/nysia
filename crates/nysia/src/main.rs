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
//! | 3 | parsed and routed, but this build has no implementation |
//!
//! Three and one are kept apart on purpose. "This build cannot do that" is a fact about the
//! build that no retry will change; "that did not work" is a fact about this attempt, and
//! the error envelope that comes with it says whether retrying would help.

mod cli;
mod daemon;
mod verbs;

use std::io::Write;
use std::process::ExitCode;

use crate::cli::{Cli, Mode};

/// The verb was asked for something valid and it did not work. The error envelope on stderr
/// says what and what to do about it.
const EXIT_FAILED: u8 = 1;

/// The verb parsed and routed correctly but has no implementation in this build.
const EXIT_UNIMPLEMENTED: u8 = 3;

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
        Mode::Unimplemented(err) => {
            let _ = writeln!(std::io::stderr(), "{err}");
            ExitCode::from(EXIT_UNIMPLEMENTED)
        }
    }
}
