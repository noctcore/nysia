//! The argv surface of the one Nysia binary.
//!
//! D-11: `nysia` is both the daemon and the CLI, and the mode is chosen by argv. There is
//! no second executable and no `nysiad` on disk — `nysia --daemon` *is* `nysiad`.
//!
//! ```text
//! nysia --daemon          -> becomes nysiad
//! nysia session create    -> CLI client
//! nysia terminal read     -> CLI client (what agents call)
//! nysia hook              -> status ingest, stdin -> socket
//! ```
//!
//! Everything except `--daemon` is a client of the daemon socket. In the v0.1 scaffold
//! there is no socket yet, so every verb resolves to [`Unimplemented`], which names the
//! wave that fills it in rather than pretending to have failed.

use std::ffi::OsString;

use clap::{CommandFactory, Parser, Subcommand};

/// A verb that parses, is routed correctly, and has no implementation behind it yet.
///
/// This is deliberately a typed error rather than a `todo!()`: a scaffold that panics is
/// indistinguishable from a scaffold that is broken, and `nysia session list` returning a
/// clear "wave 2 owns this" is honest about what this build is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`nysia {verb}` is not implemented in the v0.1 scaffold; it lands in {owner}")]
pub struct Unimplemented {
    /// The verb path the user asked for, e.g. `terminal read`.
    pub verb: String,
    /// Which delivery-plan wave owns the implementation.
    pub owner: &'static str,
}

impl Unimplemented {
    /// Build the error for a verb owned by `owner`.
    fn new(verb: impl Into<String>, owner: &'static str) -> Self {
        Self {
            verb: verb.into(),
            owner,
        }
    }
}

/// The Nysia daemon and CLI.
#[derive(Debug, Parser)]
#[command(
    name = "nysia",
    version,
    about = "Nysia — a terminal-first agentic development environment",
    long_about = "Nysia's single binary. With --daemon it becomes nysiad, the long-lived \
runtime that owns the PTYs, the store, git and orchestration. With a verb it is a client \
of that daemon's socket, which is the same surface the GUI and the Claude hooks use.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Become `nysiad`: the long-lived runtime that owns every PTY and survives the UI.
    #[arg(long)]
    pub daemon: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

/// The client verbs.
#[derive(Debug, Subcommand)]
enum Command {
    /// Create, list and close sessions.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Read from, write to and resize a session's terminal.
    Terminal {
        #[command(subcommand)]
        action: TerminalAction,
    },
    /// Forward a Claude hook payload from stdin to the daemon (D-16).
    Hook {
        /// The hook event name, as Claude spells it.
        #[arg(long)]
        event: Option<String>,
    },
}

/// `nysia session …`
#[derive(Debug, Subcommand)]
enum SessionAction {
    /// Start a session — a shell, or a Claude agent.
    Create {
        /// What to run: `pwsh`, `cmd`, `bash`, `wsl`, or `claude`.
        program: Option<String>,
    },
    /// List the sessions the daemon is holding open.
    List,
    /// Close a session and tree-kill its process group.
    Close {
        /// The session handle, `sess_<uuid>`.
        handle: Option<String>,
    },
}

/// `nysia terminal …`
#[derive(Debug, Subcommand)]
enum TerminalAction {
    /// Read a session's output: the rendered screen by default, raw bytes with `--stream`.
    Read {
        /// Return the raw byte stream instead of the rendered screen.
        #[arg(long)]
        stream: bool,
    },
    /// Write bytes to a session's input.
    Send {
        /// The text to write.
        text: Option<String>,
    },
    /// Resize a session's pseudo-terminal.
    Resize {
        /// Columns.
        #[arg(long)]
        cols: Option<u16>,
        /// Rows.
        #[arg(long)]
        rows: Option<u16>,
    },
}

/// What running the parsed argv means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// `nysia --daemon`: become the runtime.
    Daemon,
    /// A verb that will be a socket client once there is a socket.
    Client(Unimplemented),
}

impl Cli {
    /// Parse argv, rejecting `--daemon` combined with a client verb.
    ///
    /// clap's `conflicts_with` cannot name a subcommand, so the mutual exclusion between
    /// the daemon mode and the client verbs is checked here and reported as a normal clap
    /// error — same formatting, same usage exit code.
    ///
    /// # Errors
    ///
    /// Returns the clap error for malformed argv, for `--help`/`--version`, and for
    /// `--daemon` given alongside a verb.
    pub fn parse_from_argv<I, T>(argv: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let cli = Self::try_parse_from(argv)?;
        if cli.daemon && cli.command.is_some() {
            return Err(Self::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--daemon becomes the runtime and cannot also run a client verb",
            ));
        }
        Ok(cli)
    }

    /// Resolve parsed argv into the mode the process should run in.
    ///
    /// Routing is separated from execution so that the verb table can be tested without
    /// spawning anything, which is the only part of the CLI that is real in wave 0.
    #[must_use]
    pub fn mode(&self) -> Mode {
        if self.daemon {
            return Mode::Daemon;
        }
        match &self.command {
            Some(Command::Session { action }) => {
                let verb = match action {
                    SessionAction::Create { .. } => "session create",
                    SessionAction::List => "session list",
                    SessionAction::Close { .. } => "session close",
                };
                Mode::Client(Unimplemented::new(verb, "wave 2 (W4)"))
            }
            Some(Command::Terminal { action }) => {
                let verb = match action {
                    TerminalAction::Read { .. } => "terminal read",
                    TerminalAction::Send { .. } => "terminal send",
                    TerminalAction::Resize { .. } => "terminal resize",
                };
                Mode::Client(Unimplemented::new(verb, "wave 2 (W4)"))
            }
            // `arg_required_else_help` means clap has already exited when there is no
            // subcommand and no flag, so `None` here is only reachable from a unit test.
            Some(Command::Hook { .. }) | None => {
                Mode::Client(Unimplemented::new("hook", "v0.2 (Claude agent sessions)"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Cli {
        Cli::parse_from_argv(argv).expect("argv should parse")
    }

    #[test]
    fn the_argv_surface_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn daemon_mode_is_selected_by_argv() {
        assert_eq!(parse(&["nysia", "--daemon"]).mode(), Mode::Daemon);
    }

    #[test]
    fn daemon_and_a_verb_are_mutually_exclusive() {
        let err = Cli::parse_from_argv(["nysia", "--daemon", "session", "list"])
            .expect_err("--daemon and a verb are mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn every_verb_routes_to_a_typed_unimplemented_naming_its_wave() {
        let cases: [(&[&str], &str, &str); 7] = [
            (
                &["nysia", "session", "create", "pwsh"],
                "session create",
                "wave 2 (W4)",
            ),
            (&["nysia", "session", "list"], "session list", "wave 2 (W4)"),
            (
                &["nysia", "session", "close", "sess_x"],
                "session close",
                "wave 2 (W4)",
            ),
            (
                &["nysia", "terminal", "read"],
                "terminal read",
                "wave 2 (W4)",
            ),
            (
                &["nysia", "terminal", "send", "ls"],
                "terminal send",
                "wave 2 (W4)",
            ),
            (
                &["nysia", "terminal", "resize"],
                "terminal resize",
                "wave 2 (W4)",
            ),
            (&["nysia", "hook"], "hook", "v0.2 (Claude agent sessions)"),
        ];
        for (argv, verb, owner) in cases {
            match parse(argv).mode() {
                Mode::Client(err) => {
                    assert_eq!(err.verb, verb);
                    assert_eq!(err.owner, owner);
                    assert!(
                        err.to_string()
                            .contains("not implemented in the v0.1 scaffold")
                    );
                }
                Mode::Daemon => panic!("{argv:?} should not have selected daemon mode"),
            }
        }
    }

    #[test]
    fn an_unknown_verb_is_refused_rather_than_ignored() {
        assert!(Cli::parse_from_argv(["nysia", "orchestration", "ask"]).is_err());
    }
}
