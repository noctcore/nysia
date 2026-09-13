//! `nysia` — the daemon and the CLI, in one binary, selected by argv (D-11).
//!
//! The GUI and the daemon are deliberately allowed to run different versions, because that
//! is what an in-place upgrade requires; the versioned socket handshake is what makes it
//! safe. This binary ships inside the app bundle and is symlinked onto `PATH`.

mod cli;

use std::process::ExitCode;

use crate::cli::{Cli, Mode};

/// Returned when a verb parsed and routed correctly but has no implementation in this
/// build. Distinct from clap's usage exit (2) so a caller can tell "you typed it wrong"
/// from "this build cannot do that yet".
const EXIT_UNIMPLEMENTED: u8 = 3;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NYSIA_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse_from_argv(std::env::args_os()).unwrap_or_else(|err| err.exit());
    match cli.mode() {
        Mode::Daemon => {
            // Wave 2 (W4) replaces this with the real thing: bind the versioned socket,
            // write the pid record, adopt or refuse an existing daemon, then serve.
            println!(
                "nysia {}: would become nysiad here — bind \\\\.\\pipe\\nysiad-v1-<user> (Windows) \
                 or nysiad-v1.sock (macOS), write the pid record, and serve the verb surface.",
                env!("CARGO_PKG_VERSION")
            );
            println!("The v0.1 scaffold has no socket server yet; it lands in wave 2 (W4).");
            ExitCode::SUCCESS
        }
        Mode::Client(unimplemented) => {
            eprintln!("{unimplemented}");
            ExitCode::from(EXIT_UNIMPLEMENTED)
        }
    }
}
