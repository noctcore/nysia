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
//! Everything except `--daemon` is a client of the daemon socket — the same socket, the same
//! verbs and the same handshake the GUI uses. That is D-1 made checkable: if this file could
//! reach a pty without going through the socket, "the window has no privileged path" would
//! be unfalsifiable, because there would be a second path to compare it against.
//!
//! # `--json`
//!
//! Every verb takes it, and the contract is the same everywhere: **the result on stdout, the
//! error envelope on stderr, and the exit code says which happened.** A tool that parses
//! stdout never has to decide whether what it is holding is an answer or an apology.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use nysia_proto::{ReadMode, ShellProfile, WaitFor};

/// A verb that parses, is routed correctly, and has no implementation behind it yet.
///
/// A typed error rather than a `todo!()`: a build that panics is indistinguishable from a
/// build that is broken, and being told which version serves a verb is honest about what
/// this one does.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`nysia {verb}` is not implemented in this build; it lands in {owner}")]
pub struct Unimplemented {
    /// The verb path the user asked for, e.g. `hook`.
    pub verb: String,
    /// Which delivery-plan milestone owns the implementation.
    pub owner: &'static str,
}

/// The Nysia daemon and CLI.
#[derive(Debug, Parser)]
#[command(
    name = "nysia",
    version,
    about = "Nysia — a terminal-first agentic development environment",
    long_about = "Nysia's single binary. With --daemon it becomes nysiad, the long-lived \
runtime that owns the PTYs, the store, git and orchestration. With a verb it is a client \
of that daemon's socket, which is the same surface the GUI and the Claude hooks use.

Every verb takes --json: the result goes to stdout, the error envelope to stderr, and the \
exit code says which happened.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Become `nysiad`: the long-lived runtime that owns every PTY and survives the UI.
    #[arg(long)]
    pub daemon: bool,

    /// Keep serving with no clients and no sessions, instead of retiring.
    ///
    /// For a supervisor that expects the process it started to stay started.
    #[arg(long, requires = "daemon")]
    pub no_idle_retire: bool,

    /// Fail instead of starting a daemon when none is listening.
    #[arg(long, global = true)]
    pub no_spawn: bool,

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
    /// Read from, write to, resize and wait on a session's terminal.
    Terminal {
        #[command(subcommand)]
        action: TerminalAction,
    },
    /// Forward a Claude hook payload from stdin to the daemon (D-16).
    Hook {
        /// The hook event name, as Claude spells it.
        #[arg(long)]
        event: Option<String>,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `nysia session …`
#[derive(Debug, Subcommand)]
enum SessionAction {
    /// Start a shell session and print its handle.
    Create(CreateArgs),
    /// List the sessions the daemon is holding open.
    List {
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Close a session and tree-kill its process group.
    Close {
        /// The session handle, `sess_<uuid>`.
        handle: String,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `nysia session create …`
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Which shell to run. Omit for the platform's default.
    #[arg(long, value_enum)]
    pub profile: Option<ProfileArg>,
    /// Which WSL distribution, with `--profile wsl`. Omit for the default one.
    #[arg(long)]
    pub distro: Option<String>,
    /// Where to start. Must be an absolute path to a directory that exists.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// The pane this session belongs to, `<tabId>:<leafId>`.
    ///
    /// The GUI supplies one because it owns the tab tree. The CLI has no pane, so leaving
    /// this out has the daemon mint a synthetic key rather than putting the minting logic in
    /// every client.
    #[arg(long)]
    pub pane_key: Option<String>,
    /// An environment variable for the session, `KEY=VALUE`. Repeatable.
    ///
    /// Layered over the daemon's scrubbed base environment, never instead of it: naming one
    /// of the scrubbed variables here does not bring it back.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Initial width in cells.
    #[arg(long, default_value_t = 120)]
    pub cols: u16,
    /// Initial height in cells.
    #[arg(long, default_value_t = 30)]
    pub rows: u16,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// The shells `--profile` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProfileArg {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// The Windows command processor (`cmd.exe`).
    Cmd,
    /// The bash that ships with Git for Windows.
    GitBash,
    /// A WSL distribution, as a plain shell.
    Wsl,
}

impl ProfileArg {
    /// The wire profile this names, with `--distro` folded in.
    #[must_use]
    pub fn to_wire(self, distro: Option<String>) -> ShellProfile {
        match self {
            Self::Pwsh => ShellProfile::Pwsh,
            Self::Cmd => ShellProfile::Cmd,
            Self::GitBash => ShellProfile::GitBash,
            Self::Wsl => ShellProfile::Wsl { distro },
        }
    }
}

/// `nysia terminal …`
#[derive(Debug, Subcommand)]
enum TerminalAction {
    /// Read a session's output: the rendered screen by default.
    Read(ReadArgs),
    /// Write bytes to a session's input.
    Send(SendArgs),
    /// Resize a session's pseudo-terminal.
    Resize(ResizeArgs),
    /// Block until a session exits or goes idle.
    Wait(WaitArgs),
}

/// `nysia terminal read …`
#[derive(Debug, Args)]
pub struct ReadArgs {
    /// The session handle.
    pub handle: String,
    /// Return the rendered screen: what a person looking at the pane would see. The default.
    #[arg(long, conflicts_with = "stream")]
    pub screen: bool,
    /// Return the logical lines that have scrolled past, paged by cursor.
    #[arg(long)]
    pub stream: bool,
    /// Where to resume from, with `--stream`.
    #[arg(long)]
    pub cursor: Option<u64>,
    /// At most this many lines.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

impl ReadArgs {
    /// Which projection this asks for.
    ///
    /// Screen unless `--stream` says otherwise. Nysia defaults to the rendered screen and
    /// §7.2 says why: the alternative returns the accumulated escape-stripped stream, so a
    /// `clear` typed one key at a time reads back as `cclclecleaclear`.
    #[must_use]
    pub fn mode(&self) -> ReadMode {
        if self.stream {
            ReadMode::Stream
        } else {
            ReadMode::Screen
        }
    }
}

/// `nysia terminal send …`
#[derive(Debug, Args)]
pub struct SendArgs {
    /// The session handle.
    pub handle: String,
    /// The literal text to write. Sent as typed; no shell quoting is applied.
    #[arg(long, default_value = "")]
    pub text: String,
    /// Append a carriage return, as pressing return would.
    #[arg(long)]
    pub enter: bool,
    /// Send Ctrl-C first, before the text.
    #[arg(long)]
    pub interrupt: bool,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `nysia terminal resize …`
#[derive(Debug, Args)]
pub struct ResizeArgs {
    /// The session handle.
    pub handle: String,
    /// New width in cells.
    #[arg(long)]
    pub cols: u16,
    /// New height in cells.
    #[arg(long)]
    pub rows: u16,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `nysia terminal wait …`
#[derive(Debug, Args)]
pub struct WaitArgs {
    /// The session handle.
    pub handle: String,
    /// What to wait for.
    ///
    /// `exit` is the authoritative one. `idle` is a heuristic by construction — a repainting
    /// TUI never truly stops — so prefer `exit` whenever the thing being waited on is a
    /// command rather than a person's shell.
    #[arg(long = "for", value_enum, default_value_t = WaitForArg::Exit)]
    pub wait_for: WaitForArg,
    /// Give up after this many milliseconds. Omit to wait as long as the connection lives.
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// What `--for` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WaitForArg {
    /// The child process exited.
    Exit,
    /// The session stopped producing output.
    Idle,
}

impl From<WaitForArg> for WaitFor {
    fn from(arg: WaitForArg) -> Self {
        match arg {
            WaitForArg::Exit => Self::Exit,
            WaitForArg::Idle => Self::Idle,
        }
    }
}

/// What running the parsed argv means.
#[derive(Debug)]
pub enum Mode {
    /// `nysia --daemon`: become the runtime.
    Daemon {
        /// Whether to stay up with no clients and no sessions.
        never_retire: bool,
    },
    /// A verb that speaks to the daemon over its socket.
    Client(Box<Verb>),
    /// A verb that parses but is not in this build.
    Unimplemented(Unimplemented),
}

/// One resolved client verb, with its arguments and its output format.
#[derive(Debug)]
pub enum Verb {
    /// `nysia session create`
    SessionCreate(CreateArgs),
    /// `nysia session list`
    SessionList {
        /// Print the result as JSON.
        json: bool,
    },
    /// `nysia session close`
    SessionClose {
        /// The session handle, as typed.
        handle: String,
        /// Print the result as JSON.
        json: bool,
    },
    /// `nysia terminal read`
    TerminalRead(ReadArgs),
    /// `nysia terminal send`
    TerminalSend(SendArgs),
    /// `nysia terminal resize`
    TerminalResize(ResizeArgs),
    /// `nysia terminal wait`
    TerminalWait(WaitArgs),
}

impl Verb {
    /// Whether this verb was asked for JSON output.
    #[must_use]
    pub fn json(&self) -> bool {
        match self {
            Self::SessionCreate(args) => args.json,
            Self::SessionList { json } | Self::SessionClose { json, .. } => *json,
            Self::TerminalRead(args) => args.json,
            Self::TerminalSend(args) => args.json,
            Self::TerminalResize(args) => args.json,
            Self::TerminalWait(args) => args.json,
        }
    }
}

impl Cli {
    /// Parse argv, rejecting `--daemon` combined with a client verb.
    ///
    /// clap's `conflicts_with` cannot name a subcommand, so the mutual exclusion between the
    /// daemon mode and the client verbs is checked here and reported as a normal clap error —
    /// same formatting, same usage exit code.
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
    /// Routing is separated from execution so the verb table can be tested without opening a
    /// socket or spawning anything.
    #[must_use]
    pub fn into_mode(self) -> Mode {
        if self.daemon {
            return Mode::Daemon {
                never_retire: self.no_idle_retire,
            };
        }
        match self.command {
            Some(Command::Session { action }) => Mode::Client(Box::new(match action {
                SessionAction::Create(args) => Verb::SessionCreate(args),
                SessionAction::List { json } => Verb::SessionList { json },
                SessionAction::Close { handle, json } => Verb::SessionClose { handle, json },
            })),
            Some(Command::Terminal { action }) => Mode::Client(Box::new(match action {
                TerminalAction::Read(args) => Verb::TerminalRead(args),
                TerminalAction::Send(args) => Verb::TerminalSend(args),
                TerminalAction::Resize(args) => Verb::TerminalResize(args),
                TerminalAction::Wait(args) => Verb::TerminalWait(args),
            })),
            // `arg_required_else_help` means clap has already exited when there is no
            // subcommand and no flag, so `None` here is only reachable from a unit test.
            Some(Command::Hook { .. }) | None => Mode::Unimplemented(Unimplemented {
                verb: "hook".to_owned(),
                owner: "v0.2 (Claude agent sessions)",
            }),
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
        assert!(matches!(
            parse(&["nysia", "--daemon"]).into_mode(),
            Mode::Daemon {
                never_retire: false
            }
        ));
        assert!(matches!(
            parse(&["nysia", "--daemon", "--no-idle-retire"]).into_mode(),
            Mode::Daemon { never_retire: true }
        ));
    }

    #[test]
    fn daemon_and_a_verb_are_mutually_exclusive() {
        let err = Cli::parse_from_argv(["nysia", "--daemon", "session", "list"])
            .expect_err("--daemon and a verb are mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn no_idle_retire_is_meaningless_without_the_daemon_flag() {
        // A flag that silently does nothing is worse than one that is refused: somebody would
        // pass it to a client verb and believe a daemon had been kept alive.
        assert!(Cli::parse_from_argv(["nysia", "--no-idle-retire", "session", "list"]).is_err());
    }

    #[test]
    fn every_verb_takes_json() {
        // §6.2's contract only holds if it holds everywhere. A verb without --json is one an
        // agent has to scrape.
        let cases: [&[&str]; 7] = [
            &["nysia", "session", "create", "--json"],
            &["nysia", "session", "list", "--json"],
            &["nysia", "session", "close", "sess_x", "--json"],
            &["nysia", "terminal", "read", "sess_x", "--json"],
            &[
                "nysia", "terminal", "send", "sess_x", "--text", "ls", "--json",
            ],
            &[
                "nysia", "terminal", "resize", "sess_x", "--cols", "80", "--rows", "24", "--json",
            ],
            &["nysia", "terminal", "wait", "sess_x", "--json"],
        ];
        for argv in cases {
            match parse(argv).into_mode() {
                Mode::Client(verb) => assert!(verb.json(), "{argv:?} should have asked for json"),
                other => panic!("{argv:?} should be a client verb, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_read_defaults_to_the_screen_and_the_two_modes_are_exclusive() {
        let Mode::Client(verb) = parse(&["nysia", "terminal", "read", "sess_x"]).into_mode() else {
            panic!("a read is a client verb");
        };
        let Verb::TerminalRead(args) = *verb else {
            panic!("a read parses as a read");
        };
        assert_eq!(args.mode(), ReadMode::Screen);

        let Mode::Client(verb) =
            parse(&["nysia", "terminal", "read", "sess_x", "--stream"]).into_mode()
        else {
            panic!("a read is a client verb");
        };
        let Verb::TerminalRead(args) = *verb else {
            panic!("a read parses as a read");
        };
        assert_eq!(args.mode(), ReadMode::Stream);

        assert!(
            Cli::parse_from_argv([
                "nysia", "terminal", "read", "sess_x", "--screen", "--stream"
            ])
            .is_err(),
            "asking for both projections at once is a mistake, not a preference"
        );
    }

    #[test]
    fn a_wsl_profile_carries_its_distribution() {
        assert_eq!(
            ProfileArg::Wsl.to_wire(Some("Ubuntu-24.04".to_owned())),
            ShellProfile::Wsl {
                distro: Some("Ubuntu-24.04".to_owned())
            }
        );
        // A distro named against another profile is simply not part of that profile's shape,
        // so it cannot be smuggled into one.
        assert_eq!(
            ProfileArg::Pwsh.to_wire(Some("Ubuntu-24.04".to_owned())),
            ShellProfile::Pwsh
        );
    }

    #[test]
    fn the_hook_verb_names_the_version_that_serves_it() {
        match parse(&["nysia", "hook"]).into_mode() {
            Mode::Unimplemented(err) => {
                assert_eq!(err.verb, "hook");
                assert!(err.owner.contains("v0.2"));
            }
            other => panic!("hook is not implemented in this build, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_verb_is_refused_rather_than_ignored() {
        assert!(Cli::parse_from_argv(["nysia", "orchestration", "ask"]).is_err());
    }
}
