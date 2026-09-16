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
//! nysia agent status      -> the dots, as the window and the phone read them
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
use nysia_proto::{ReadMode, SessionKind, ShellProfile, WaitFor};

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
    // D-16: Claude hooks ingest status over stdin to the daemon socket.
    /// Forward a Claude hook payload from stdin to the daemon.
    Hook(HookArgs),
    /// Read what the agents in this daemon's panes are doing.
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    // v0.3 §3: projects are registered folders, listed and forgotten here.
    /// Register folders as projects, list them, and forget them.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    // D-5: tasks are GitHub Issues, queried live, no local task domain model.
    /// List a project's GitHub issues.
    Tasks {
        #[command(subcommand)]
        action: TasksAction,
    },
}

/// `nysia tasks …`
#[derive(Debug, Subcommand)]
enum TasksAction {
    // D-5: tasks are GitHub Issues and nothing is stored, so this is a live query
    // every time.
    /// List a registered project's open GitHub issues, queried live.
    ///
    /// Needs the GitHub CLI installed and signed in. When either is missing the refusal
    /// says which, because "no issues" and "no credentials" are different answers.
    List {
        /// The project id, `proj_<32 hex digits>`.
        id: String,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `nysia project …`
#[derive(Debug, Subcommand)]
enum ProjectAction {
    /// Register a folder as a project.
    ///
    /// The folder must be a git repository. Nysia does not own it, does not move it, and
    /// does not write into it beyond ordinary git operations.
    Register(RegisterArgs),
    /// List the projects this daemon has registered.
    List {
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
    // D-6: the worktree is keyed by its branch and never by a task id.
    /// Create or adopt a branch-keyed worktree and start a session in it.
    ///
    /// The worktree is keyed by its branch, never by a task id. One that already exists
    /// for the branch is adopted rather than refused.
    Start(StartArgs),
    /// Forget a project's registration. Nothing on disk is touched.
    Forget {
        /// The project id, `proj_<32 hex digits>`.
        id: String,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `nysia project start …`
#[derive(Debug, Args)]
pub struct StartArgs {
    /// The project id, `proj_<32 hex digits>`.
    pub id: String,
    /// The branch the worktree is keyed by. Created if it is not there yet.
    #[arg(long)]
    pub branch: String,
    /// Which session to start: `shell` or `agent`. A shell unless this says otherwise.
    // Parsed as SessionKind itself — clap uses that type's FromStr, so the two spellings
    // live in nysia-proto (D-13) and a third one never becomes a request. ValueEnum would
    // restore clap's "Possible values" block but would duplicate the spellings here; the
    // FromStr match is the compile error that keeps a new variant off the wire.
    #[arg(long, default_value_t = SessionKind::Shell)]
    pub kind: SessionKind,
    /// Which shell to run, with `--kind shell`. Omit for the platform's default.
    ///
    /// Combined with `--kind agent` is refused rather than ignored: a profile names a
    /// shell, and an agent is not one.
    #[arg(long, value_enum)]
    pub profile: Option<ProfileArg>,
    /// Which WSL distribution, with `--profile wsl`. Omit for the default one.
    #[arg(long)]
    pub distro: Option<String>,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `nysia project register …`
#[derive(Debug, Args)]
pub struct RegisterArgs {
    // The daemon canonicalises it, because the id is derived from the canonical path
    // and a client that resolved it first would be one more spelling to disagree about.
    /// The folder to register. Relative and unresolved spellings are fine.
    ///
    /// The daemon canonicalises it. Registering the same folder twice is one project
    /// however it is spelled.
    pub path: PathBuf,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `nysia hook …`
#[derive(Debug, Args)]
pub struct HookArgs {
    // See `crate::hook`, which is the only place the two values exist at the same
    // instant.
    /// The hook event name, as Claude spells it.
    ///
    /// Fills in a payload that does not name one, and is refused when the payload names a
    /// different one.
    #[arg(long)]
    pub event: Option<String>,
    // §5.2: stdout is Claude's hook decision, so a second document after `{}` is a
    // parse error for whoever reads one.
    /// Print the error envelope as JSON.
    ///
    /// Only the error. Stdout is Claude's hook decision and carries `{}` whatever this
    /// says, because a second document after it is a parse error for whoever reads one.
    #[arg(long)]
    pub json: bool,
}

/// `nysia agent …`
#[derive(Debug, Subcommand)]
enum AgentAction {
    /// Print the status of every pane, or of one.
    Status(StatusArgs),
}

/// `nysia agent status …`
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// One pane, `<tabId>:<leafId>`. Omit for every pane the daemon holds a status for.
    #[arg(long)]
    pub pane: Option<String>,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `nysia session …`
#[derive(Debug, Subcommand)]
enum SessionAction {
    /// Start a session and print its handle.
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
    /// Which session to start: `shell` or `agent`. A shell unless this says otherwise.
    // Same FromStr as `project start --kind`: the spellings live in nysia-proto (D-13).
    #[arg(long, default_value_t = SessionKind::Shell)]
    pub kind: SessionKind,
    /// Which shell to run, with `--kind shell`. Omit for the platform's default.
    ///
    /// Combined with `--kind agent` is refused rather than ignored: a profile names a
    /// shell, and an agent is not one.
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
    /// `nysia hook`: a Claude hook payload on stdin.
    ///
    /// Its own mode rather than a [`Verb`], because its output contract is the opposite of
    /// every verb's. A verb puts its result on stdout when it has finished; the hook puts
    /// `{}` there **before it starts**, since stdout is Claude's decision rather than an
    /// answer (§5.2). Routing it through the verb runner would mean the first thing it did
    /// was something that could fail.
    Hook(HookArgs),
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
    /// `nysia agent status`
    AgentStatus(StatusArgs),
    /// `nysia project register`
    ProjectRegister(RegisterArgs),
    /// `nysia project list`
    ProjectList {
        /// Print the result as JSON.
        json: bool,
    },
    /// `nysia project start`
    ProjectStart(StartArgs),
    /// `nysia project forget`
    ProjectForget {
        /// The project id, as typed.
        id: String,
        /// Print the result as JSON.
        json: bool,
    },
    /// `nysia tasks list`
    TasksList {
        /// The project id, as typed.
        id: String,
        /// Print the result as JSON.
        json: bool,
    },
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
            Self::AgentStatus(args) => args.json,
            Self::ProjectRegister(args) => args.json,
            Self::ProjectStart(args) => args.json,
            Self::ProjectList { json } | Self::ProjectForget { json, .. } => *json,
            Self::TasksList { json, .. } => *json,
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
            Some(Command::Agent { action }) => Mode::Client(Box::new(match action {
                AgentAction::Status(args) => Verb::AgentStatus(args),
            })),
            Some(Command::Project { action }) => Mode::Client(Box::new(match action {
                ProjectAction::Register(args) => Verb::ProjectRegister(args),
                ProjectAction::List { json } => Verb::ProjectList { json },
                ProjectAction::Start(args) => Verb::ProjectStart(args),
                ProjectAction::Forget { id, json } => Verb::ProjectForget { id, json },
            })),
            Some(Command::Tasks { action }) => Mode::Client(Box::new(match action {
                TasksAction::List { id, json } => Verb::TasksList { id, json },
            })),
            Some(Command::Hook(args)) => Mode::Hook(args),
            // `arg_required_else_help` means clap has already exited when there is no
            // subcommand and no flag, so `None` here is only reachable from a unit test. It
            // answers with the hook because that is the mode that cannot fail before it has
            // printed, and a mode that is never reached still has to be some mode.
            None => Mode::Hook(HookArgs {
                event: None,
                json: false,
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
        let cases: [&[&str]; 13] = [
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
            &["nysia", "agent", "status", "--json"],
            &["nysia", "project", "register", ".", "--json"],
            &["nysia", "project", "list", "--json"],
            &["nysia", "project", "forget", "proj_x", "--json"],
            &[
                "nysia", "project", "start", "proj_x", "--branch", "feat/x", "--json",
            ],
            &["nysia", "tasks", "list", "proj_x", "--json"],
        ];
        for argv in cases {
            match parse(argv).into_mode() {
                Mode::Client(verb) => assert!(verb.json(), "{argv:?} should have asked for json"),
                other => panic!("{argv:?} should be a client verb, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_task_list_routes_to_the_spelling_the_acceptance_test_drives() {
        // `nysia tasks list <id> --json` is what makes the verb testable end to end without
        // a window, which is the reason it is in the spec at all — so the spelling is pinned
        // rather than left to whoever reads the help next.
        let Mode::Client(verb) = parse(&["nysia", "tasks", "list", "proj_x", "--json"]).into_mode()
        else {
            panic!("a task list is a client verb");
        };
        let Verb::TasksList { id, json } = *verb else {
            panic!("a task list parses as a task list");
        };
        assert_eq!(id, "proj_x");
        assert!(json);

        // The project is required. A bare `nysia tasks list` has no folder to fall back on —
        // unlike `project register`, the daemon's working directory is not the caller's, so
        // there is nothing sensible to default to and clap refuses it here instead.
        assert!(Cli::parse_from_argv(["nysia", "tasks", "list"]).is_err());
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
    fn the_hook_is_its_own_mode_and_never_a_verb() {
        // Not a `Verb`, and the distinction is load-bearing rather than tidy: the verb runner
        // parses arguments and opens a socket before it prints anything, and the hook has to
        // have printed `{}` before either of those can fail (§5.2).
        let Mode::Hook(args) = parse(&["nysia", "hook", "--event", "Stop"]).into_mode() else {
            panic!("`nysia hook` is its own mode");
        };
        assert_eq!(args.event.as_deref(), Some("Stop"));
        assert!(!args.json);

        // And it takes --json like every other verb, for the error envelope.
        let Mode::Hook(args) = parse(&["nysia", "hook", "--json"]).into_mode() else {
            panic!("`nysia hook` is its own mode");
        };
        assert!(args.json);
    }

    #[test]
    fn agent_status_takes_one_pane_or_all_of_them() {
        let Mode::Client(verb) = parse(&["nysia", "agent", "status"]).into_mode() else {
            panic!("a status read is a client verb");
        };
        let Verb::AgentStatus(args) = *verb else {
            panic!("a status read parses as one");
        };
        assert!(args.pane.is_none(), "no --pane means every pane");

        let Mode::Client(verb) =
            parse(&["nysia", "agent", "status", "--pane", "tab_1:leaf_1"]).into_mode()
        else {
            panic!("a status read is a client verb");
        };
        let Verb::AgentStatus(args) = *verb else {
            panic!("a status read parses as one");
        };
        assert_eq!(args.pane.as_deref(), Some("tab_1:leaf_1"));
    }

    #[test]
    fn the_project_verbs_route_to_the_spelling_the_acceptance_test_drives() {
        // `crates/nysia/tests/projects.rs` drives `nysia project list --json` and expects a
        // bare array on stdout. That spelling is fixed by a test this crate may not edit, so
        // it is pinned here too rather than left to be rediscovered from a red acceptance run.
        let Mode::Client(verb) = parse(&["nysia", "project", "list", "--json"]).into_mode() else {
            panic!("a project list is a client verb");
        };
        assert!(matches!(*verb, Verb::ProjectList { json: true }));

        let Mode::Client(verb) =
            parse(&["nysia", "project", "register", "some/folder"]).into_mode()
        else {
            panic!("a registration is a client verb");
        };
        let Verb::ProjectRegister(args) = *verb else {
            panic!("a registration parses as one");
        };
        assert_eq!(args.path, PathBuf::from("some/folder"));

        let Mode::Client(verb) = parse(&["nysia", "project", "forget", "proj_x"]).into_mode()
        else {
            panic!("forgetting is a client verb");
        };
        let Verb::ProjectForget { id, .. } = *verb else {
            panic!("forgetting parses as itself");
        };
        assert_eq!(id, "proj_x");
    }

    #[test]
    fn starting_takes_a_branch_and_has_no_way_to_name_an_issue() {
        // **D-6 at the argv surface.** A worktree is keyed by its branch, never by a task id,
        // and the CLI must not offer a second way to say it. `--issue` is the flag somebody
        // would reach for; it does not exist, and a test that says so is what keeps it from
        // being added as a convenience.
        let Mode::Client(verb) =
            parse(&["nysia", "project", "start", "proj_x", "--branch", "feat/x"]).into_mode()
        else {
            panic!("a start is a client verb");
        };
        let Verb::ProjectStart(args) = *verb else {
            panic!("a start parses as one");
        };
        assert_eq!(args.branch, "feat/x");
        assert_eq!(args.id, "proj_x");
        assert_eq!(
            args.kind,
            SessionKind::Shell,
            "a start is a shell unless --kind says otherwise"
        );

        assert!(
            Cli::parse_from_argv(["nysia", "project", "start", "proj_x", "--issue", "42"]).is_err(),
            "there is no way to key a worktree by an issue, and there must not be"
        );
        // And the branch is required rather than derived from anything.
        assert!(Cli::parse_from_argv(["nysia", "project", "start", "proj_x"]).is_err());
    }

    #[test]
    fn starting_takes_a_kind_and_a_typo_never_leaves_the_parser() {
        // clap parses `--kind` as SessionKind itself (FromStr in nysia-proto), so the two
        // spellings cannot drift from the wire and a third one never becomes a request.
        let Mode::Client(verb) = parse(&[
            "nysia", "project", "start", "proj_x", "--branch", "feat/x", "--kind", "shell",
        ])
        .into_mode() else {
            panic!("a start is a client verb");
        };
        let Verb::ProjectStart(args) = *verb else {
            panic!("a start parses as one");
        };
        assert_eq!(args.kind, SessionKind::Shell);

        let Mode::Client(verb) = parse(&[
            "nysia", "project", "start", "proj_x", "--branch", "feat/x", "--kind", "agent",
        ])
        .into_mode() else {
            panic!("a start is a client verb");
        };
        let Verb::ProjectStart(args) = *verb else {
            panic!("a start parses as one");
        };
        assert_eq!(args.kind, SessionKind::Agent);

        let err = Cli::parse_from_argv([
            "nysia", "project", "start", "proj_x", "--branch", "e1-x", "--kind", "nonsense",
        ])
        .expect_err("a third kind is a clap error, not a daemon round trip");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn creating_takes_a_kind_and_a_typo_never_leaves_the_parser() {
        let Mode::Client(verb) = parse(&["nysia", "session", "create"]).into_mode() else {
            panic!("a create is a client verb");
        };
        let Verb::SessionCreate(args) = *verb else {
            panic!("a create parses as one");
        };
        assert_eq!(
            args.kind,
            SessionKind::Shell,
            "a create is a shell unless --kind says otherwise"
        );

        let Mode::Client(verb) =
            parse(&["nysia", "session", "create", "--kind", "agent"]).into_mode()
        else {
            panic!("a create is a client verb");
        };
        let Verb::SessionCreate(args) = *verb else {
            panic!("a create parses as one");
        };
        assert_eq!(args.kind, SessionKind::Agent);

        let err = Cli::parse_from_argv(["nysia", "session", "create", "--kind", "nonsense"])
            .expect_err("a third kind is a clap error, not a daemon round trip");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn kind_help_names_both_spellings_and_keeps_the_rationale_out() {
        // clap has no "Possible values" for a FromStr argument, so the help text itself
        // has to name both spellings. The original comment named neither and rendered a
        // rustdoc link plus the D-13 rationale to the user.
        for argv in [
            ["nysia", "project", "start", "--help"].as_slice(),
            ["nysia", "session", "create", "--help"].as_slice(),
        ] {
            let err = Cli::parse_from_argv(argv).expect_err("--help is a clap error");
            let help = err.to_string();
            let kind = flag_help(&help, "--kind");
            assert!(
                kind.contains("shell") && kind.contains("agent"),
                "{argv:?} --kind help must name both spellings, got:\n{kind}"
            );
            assert!(
                !kind.contains("SessionKind")
                    && !kind.contains("FromStr")
                    && !kind.contains("D-13"),
                "{argv:?} --kind help must not leak internal notes, got:\n{kind}"
            );
        }
    }

    /// The help block for one long flag: its usage line and the indented description
    /// that follows, stopping at the next flag or the defaults footer.
    fn flag_help(help: &str, flag: &str) -> String {
        let mut lines = help.lines().peekable();
        while let Some(line) = lines.next() {
            if !line.trim_start().starts_with(flag) {
                continue;
            }
            let mut block = vec![line];
            while let Some(next) = lines.peek() {
                let trimmed = next.trim_start();
                if trimmed.starts_with("--") || trimmed.starts_with('[') {
                    break;
                }
                block.push(lines.next().expect("peeked"));
            }
            return block.join("\n");
        }
        panic!("{flag} was not in the help:\n{help}");
    }

    /// Artefacts a help string must not contain. A user reading the terminal is not
    /// the next maintainer of `docs/design`.
    fn maintainer_artefacts(help: &str) -> Vec<&'static str> {
        let mut found = Vec::new();
        if help.contains("**") {
            found.push("markdown emphasis (**)");
        }
        if help.contains("[`") {
            found.push("rustdoc link ([`)");
        }
        if help.contains("::") {
            found.push("rust path (::)");
        }
        if contains_hyphenated_decision_id(help) {
            found.push("decision id (D-n)");
        }
        if contains_unhyphenated_decision_id(help) {
            found.push("decision id (Dn)");
        }
        if help.contains('§') {
            found.push("section mark (§)");
        }
        if help.contains("ts-rs") {
            found.push("ts-rs");
        }
        if contains_trap_word(help) {
            found.push("trap");
        }
        if contains_wire_type(help) {
            found.push("wire type");
        }
        if help.contains("TODO(#") {
            found.push("TODO(#n)");
        }
        found
    }

    /// `D` at a token boundary, then `-` and a digit. `CMD-3` and `ID-1` do not count:
    /// the `D` there is in the middle of another token.
    fn contains_hyphenated_decision_id(help: &str) -> bool {
        let bytes = help.as_bytes();
        bytes.windows(3).enumerate().any(|(i, w)| {
            w[0] == b'D'
                && w[1] == b'-'
                && w[2].is_ascii_digit()
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        })
    }

    /// `D` at a token boundary, then a digit and no hyphen. `D16` is a decision id;
    /// `D-16` is the hyphenated form, counted separately.
    fn contains_unhyphenated_decision_id(help: &str) -> bool {
        let bytes = help.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'D' && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
                let rest = &bytes[i + 1..];
                if rest.first().is_some_and(|c| c.is_ascii_digit()) {
                    let mut j = i + 2;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j == bytes.len() || !bytes[j].is_ascii_alphanumeric() {
                        return true;
                    }
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
        false
    }

    fn contains_word(help: &str, word: &str) -> bool {
        let hay = help.as_bytes();
        let needle = word.as_bytes();
        if needle.is_empty() || hay.len() < needle.len() {
            return false;
        }
        hay.windows(needle.len()).enumerate().any(|(i, window)| {
            window == needle
                && (i == 0 || !hay[i - 1].is_ascii_alphabetic())
                && (i + needle.len() == hay.len() || !hay[i + needle.len()].is_ascii_alphabetic())
        })
    }

    /// `trap` as a word, including `Trap`, `traps` and `Traps`. The alphabetic
    /// boundary keeps `strap` and `entrapment` out; a trailing `s` is allowed
    /// because this codebase writes "the traps this avoids".
    fn contains_trap_word(help: &str) -> bool {
        let hay = help.as_bytes();
        let stem = b"trap";
        if hay.len() < stem.len() {
            return false;
        }
        hay.windows(stem.len()).enumerate().any(|(i, window)| {
            window.eq_ignore_ascii_case(stem) && (i == 0 || !hay[i - 1].is_ascii_alphabetic()) && {
                let after = i + stem.len();
                let end = if after < hay.len() && hay[after].eq_ignore_ascii_case(&b's') {
                    after + 1
                } else {
                    after
                };
                end == hay.len() || !hay[end].is_ascii_alphabetic()
            }
        })
    }

    /// The proto types this file already imports, as whole words. Naming them
    /// as turbofish types makes dropping one from the `use` a compile error; a
    /// type this file does not import is not a help leak this probe covers.
    /// `Request` is not among them: it is the English verb the about lines
    /// already use (`Create`, `Read`, `Forward`).
    fn contains_wire_type(help: &str) -> bool {
        [
            wire_type_ident::<SessionKind>(),
            wire_type_ident::<ReadMode>(),
            wire_type_ident::<ShellProfile>(),
            wire_type_ident::<WaitFor>(),
        ]
        .into_iter()
        .any(|name| contains_word(help, name))
    }

    fn wire_type_ident<T>() -> &'static str {
        std::any::type_name::<T>()
            .rsplit("::")
            .next()
            .expect("std::any::type_name is never empty")
    }

    fn rendered_help(cmd: &clap::Command) -> String {
        cmd.clone()
            .color(clap::ColorChoice::Never)
            .render_help()
            .to_string()
    }

    fn rendered_long_help(cmd: &clap::Command) -> String {
        cmd.clone()
            .color(clap::ColorChoice::Never)
            .render_long_help()
            .to_string()
    }

    /// Every command clap knows about, including ones this test does not name.
    ///
    /// `-h` renders `about`, `--help` renders `long_about`. The root sets both, so a
    /// walker that only calls `render_long_help` never sees the short string.
    fn rendered_help_tree(
        cmd: &clap::Command,
        path: &str,
        hits: &mut Vec<(String, Vec<&'static str>)>,
    ) {
        let mut found = Vec::new();
        for help in [rendered_long_help(cmd), rendered_help(cmd)] {
            for artefact in maintainer_artefacts(&help) {
                if !found.contains(&artefact) {
                    found.push(artefact);
                }
            }
        }
        if !found.is_empty() {
            hits.push((path.to_owned(), found));
        }
        for sub in cmd.get_subcommands() {
            rendered_help_tree(sub, &format!("{path} {}", sub.get_name()), hits);
        }
    }

    /// A command clap knows and this test does not name, two levels down.
    /// #102 planted at depth one; the leaks were on `project start` / `tasks list`.
    fn with_unnamed_leaf(leaf: clap::Command) -> clap::Command {
        Cli::command().subcommand(clap::Command::new("orchestration").subcommand(leaf))
    }

    fn planted_leaf_hit(hits: &[(String, Vec<&'static str>)], artefact: &str) -> bool {
        hits.iter().any(|(path, found)| {
            path == "nysia orchestration dispatch" && found.contains(&artefact)
        })
    }

    #[test]
    fn rendered_help_has_no_maintainer_artefacts() {
        // Walk clap's own tree rather than listing verbs: a roster you type is one a
        // new subcommand can drop out of, and that is how these leaks shipped.
        let cmd = Cli::command();
        let mut hits = Vec::new();
        rendered_help_tree(&cmd, cmd.get_name(), &mut hits);
        assert!(
            hits.is_empty(),
            "rendered -h/--help must not talk to a maintainer, got {hits:?}"
        );
    }

    #[test]
    fn the_help_artefact_check_trips_on_a_subcommand_it_was_not_named() {
        // Trap 12: a walker handed a list of verbs is one a new verb can drop out of.
        // Each artefact is planted on `orchestration dispatch`, which Cli does not
        // have, two levels down — the depth the original leaks sat at. Emptying any
        // one probe goes red here rather than on a real verb.
        let cases: &[(&str, &str)] = &[
            ("Forward a payload (D-16)", "decision id (D-n)"),
            ("fold into the D16 envelope", "decision id (Dn)"),
            ("keyed by its **branch**", "markdown emphasis (**)"),
            ("see [`Verb`]", "rustdoc link ([`)"),
            ("see `crate::hook`", "rust path (::)"),
            ("the SessionKind spelling", "wire type"),
            ("A ShellProfile on the wire.", "wire type"),
            ("ReadMode names the projection", "wire type"),
            ("WaitFor names the wait", "wire type"),
            ("TODO(#412): fold the envelope", "TODO(#n)"),
            ("described in v0.3 §3", "section mark (§)"),
            ("bindings from ts-rs", "ts-rs"),
            ("the trap this avoids", "trap"),
            (
                "The traps this avoids are listed in the design doc.",
                "trap",
            ),
            ("Trap 12: keep the walker off a roster", "trap"),
        ];
        for (about, artefact) in cases {
            let cmd = with_unnamed_leaf(clap::Command::new("dispatch").about(*about));
            let mut hits = Vec::new();
            rendered_help_tree(&cmd, cmd.get_name(), &mut hits);
            assert!(
                planted_leaf_hit(&hits, artefact),
                "planted {artefact:?} on `orchestration dispatch` must redden the check, got {hits:?}"
            );
        }
    }

    #[test]
    fn the_help_artefact_check_trips_on_short_help_the_long_help_hides() {
        // The root sets `about` and `long_about` separately. `-h` is the only clap
        // rendering that shows `about`; a walker that only calls `render_long_help`
        // never sees it.
        let cmd = with_unnamed_leaf(
            clap::Command::new("dispatch")
                .about("Forward a payload (D-16)")
                .long_about("Dispatch an orchestration payload."),
        );
        let mut hits = Vec::new();
        rendered_help_tree(&cmd, cmd.get_name(), &mut hits);
        assert!(
            planted_leaf_hit(&hits, "decision id (D-n)"),
            "planted D-16 on -h-only about must redden the check, got {hits:?}"
        );
    }

    #[test]
    fn maintainer_artefact_probes_ignore_lookalikes() {
        // The old `trap` substring matched `strap` and `entrapment`; the old D-n
        // window matched the `D-3` in `CMD-3` and the `D-1` in `ID-1`. Bare
        // `Request` matched the imperative the about lines already use.
        assert!(maintainer_artefacts("a strap and an entrapment").is_empty());
        assert!(maintainer_artefacts("CMD-3").is_empty());
        assert!(maintainer_artefacts("ID-1").is_empty());
        assert!(maintainer_artefacts("Request the daemon to start a worktree session.").is_empty());
    }

    #[test]
    fn registering_needs_a_folder_to_register() {
        // Not an optional argument defaulting to the current directory: the daemon's working
        // directory is not the caller's, so a bare `nysia project register` that "worked"
        // would register whichever folder the daemon happened to be started in.
        assert!(Cli::parse_from_argv(["nysia", "project", "register"]).is_err());
    }

    #[test]
    fn an_unknown_verb_is_refused_rather_than_ignored() {
        assert!(Cli::parse_from_argv(["nysia", "orchestration", "ask"]).is_err());
    }
}
