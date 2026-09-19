//! The sessions the daemon owns, and the pump that keeps their terminal state current.
//!
//! This is where D-1 and D-7 meet. The daemon owns every pty and every grid; a client is a
//! view. Killing the window drops connections and nothing else — the pump thread goes on
//! reading, the grid goes on updating, and the next client to ask gets the screen as it is
//! now rather than as it was when somebody last looked.
//!
//! # One thread per session, and what it does in order
//!
//! `nysia-core`'s pty layer is blocking by construction: a reader thread per session,
//! draining the master into a bounded queue. The daemon is async. The pump is the bridge,
//! and it runs on its own thread rather than a task because [`crate::pty::PtyOutput`] blocks
//! and a blocking receive inside a runtime worker starves every other session on it.
//!
//! Each turn of the loop:
//!
//! 1. **Flush** whatever has been coalesced, if a kilobyte has accumulated or sixteen
//!    milliseconds have passed (§7.3, trap 4: payloads under 1024 bytes go through `eval`).
//! 2. **Stop** — read nothing at all — when every attached stream is out of credit. This is
//!    the backpressure: the queue behind `PtyOutput` fills, its reader thread blocks on the
//!    send, the kernel pty buffer fills, and the child blocks in `write`.
//! 3. **Read** one chunk, feed it to the grid, and **write the emulator's replies back to
//!    the pty**. That third part is not optional and is easy to leave out: `pwsh` opens with
//!    `CSI 6 n` and will not draw its prompt until the cursor position comes back, so a pump
//!    that drops the replies leaves a session that looks permanently blank.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nysia_proto::{
    ErrorCode, ErrorEnvelope, ExitStatus, Frame, FrameKind, Incarnation, LineCursor, PaneKey,
    ProfileAvailability, ReadMode, SessionCreate, SessionCreated, SessionHandle, SessionKind,
    SessionSummary, ShellProfile as WireProfile, TerminalReadResult, TerminalSend, WaitFor,
    WaitOutcome, WorkingDirectory,
};

use crate::agent::LaunchError;
use crate::git::{CanonicalPath, PathError};
use crate::pty::{
    DEFAULT_GRACE, Output, ProfileError, PtyOutput, PtySession, ResolveError, SessionProgram,
    SessionSpec, ShellProfile, SpawnError,
};
use crate::rpc::errors::{IntoEnvelope, envelope};
use crate::rpc::stream::{SendOutcome, StreamSink};
use crate::vt::{ReadMode as VtReadMode, TerminalSize, TerminalState, VtConfig};

/// The smallest frame worth putting on the wire.
///
/// Trap 4: Tauri sends a raw payload under 1024 bytes through `eval` rather than the fetch
/// queue, so a stream of 40-byte frames is a stream of `eval` calls. Coalescing to at least
/// this much is the fix, and it costs at most [`FLUSH_INTERVAL`] of latency.
const COALESCE_MIN: usize = 1024;

/// The longest a byte waits to be flushed, and the pump's tick.
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// Flush immediately once this much has accumulated, whatever the clock says.
const FLUSH_CEILING: usize = 64 * 1024;

/// How quiet a session must be before [`WaitFor::Idle`] answers.
///
/// Heuristic by construction — proto says so, and a repainting TUI never truly stops — which
/// is why [`WaitFor::Exit`] is the one to reach for when the thing being waited on is a
/// command rather than a person's shell.
const IDLE_QUIET: Duration = Duration::from_millis(400);

/// How often a wait re-checks. Short enough to feel immediate, long enough not to spin.
const WAIT_POLL: Duration = Duration::from_millis(20);

/// Lines a `terminal read` returns when the caller names no limit.
const DEFAULT_READ_LIMIT: usize = 1000;

/// The most lines a `terminal read` will return however large a limit is asked for.
const MAX_READ_LIMIT: usize = 20_000;

/// Why a session verb failed.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// No session by that handle.
    #[error("no session {handle}")]
    Unknown {
        /// The handle that named nothing.
        handle: SessionHandle,
    },
    /// The pane already has a live session.
    #[error("pane {pane} already has a live session ({handle})")]
    PaneTaken {
        /// The pane that is taken.
        pane: PaneKey,
        /// What is in it.
        handle: SessionHandle,
    },
    /// The shell could not be started.
    #[error("could not start the session: {0}")]
    Spawn(#[source] crate::pty::SpawnError),
    /// The agent's CLI could not be resolved to something launchable.
    ///
    /// Separate from [`Self::Spawn`] because it is answered **before** anything is spawned
    /// and because the thing to do about it is different: a shell that will not start is a
    /// machine problem, and this is a program that is not installed.
    #[error("could not start the agent: {0}")]
    Launch(#[source] crate::agent::LaunchError),
    /// The requested working directory was refused.
    #[error("the working directory {} was refused: {reason}", path.display())]
    PathRefused {
        /// The path that was refused.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The session would run through `cmd.exe`, and the folder it was asked for is a network
    /// one.
    ///
    /// `cmd.exe` refuses every UNC working directory. It prints *"UNC paths are not
    /// supported. Defaulting to Windows directory."* and runs in `C:\Windows`, so without this
    /// the session would open somewhere it was not asked for, which is the defect
    /// `confine_cwd`'s spelling fix exists to remove. A drive letter mapped to a share
    /// resolves to the share's own path, so it arrives here too. No path is carried: the
    /// folder was refused, and a refused value is not the daemon's to write down.
    #[error("Command Prompt cannot start in a network folder")]
    CmdOnNetworkFolder,
    /// The program would not start, and the folder's path is longer than Windows will start
    /// one in.
    ///
    /// Diagnosed after the spawn has failed, never predicted before it: a folder is refused
    /// only when Windows has refused it, and then the length is the reason given rather than
    /// "check the shell is installed", which is not the cause.
    #[error("could not start the session: its folder's path is {length} characters long")]
    FolderTooLong {
        /// The folder's length, in the spelling a spawn is handed.
        length: usize,
        /// What the spawn said, kept for the daemon's log.
        source: crate::pty::SpawnError,
    },
    /// The request was not well formed.
    #[error("{0}")]
    Invalid(String),
    /// The session exists but the write did not land.
    #[error("the session is closing and took no further input")]
    Closing(#[source] std::io::Error),
}

impl IntoEnvelope for SessionError {
    fn into_envelope(self) -> ErrorEnvelope {
        // **Built per variant, never from `self.to_string()`.** Three of these wrap an error
        // whose `Display` spells a file on this machine. [`Self::Launch`] and the profile
        // half of [`Self::Spawn`] both carry a [`ResolveError`], and every path-carrying
        // variant of that names the candidate it rejected — which needs no attacker to
        // reach: a `claude` directory on `PATH`, an npm script that lost its exec bit, a
        // `.ps1`-only install with no PowerShell behind it. [`crate::pty::SpawnError::Spawn`]
        // carries argv\[0\], which for an agent is the person's own `claude.exe`, and a
        // `reason` that `portable-pty` formats the whole command line *and the working
        // directory* into. [`Self::PathRefused`] repeats the path it was handed.
        //
        // An envelope is the thing a daemon is most likely to log verbatim and a window is
        // the thing a person screenshots (traps register #13), and none of it is needed to
        // act on: **which** check refused is, and that is what these phrases say. The detail
        // goes to the daemon's own log below — the same split `rpc::project`'s
        // `store_refusal` makes, for the same reason.
        let message = match &self {
            Self::Launch(err) => format!("could not start the agent: {}", launch_kind(err)),
            Self::Spawn(err) => format!("could not start the session: {}", spawn_kind(err)),
            // Dropped for #94's reason rather than a new one: the guard `rpc/project.rs`
            // carries is `a_refusal_does_not_repeat_the_path_it_was_given`, and a path that
            // arrived over a socket is exactly a path this daemon was given. `reason` names
            // which of the three checks refused it, which is the actionable half.
            Self::PathRefused { reason, .. } => {
                format!("that working directory was refused: {reason}")
            }
            Self::CmdOnNetworkFolder => "that working directory was refused: it is a network \
                                         folder, and this session runs through Command Prompt \
                                         (`cmd.exe`), which cannot start in one and would open \
                                         in C:\\Windows instead"
                .to_owned(),
            Self::FolderTooLong { length, .. } => format!(
                "could not start the session: its folder's path is {length} characters long, \
                 and Windows will not start a program in a folder whose path is longer than \
                 {MAX_WORKING_DIRECTORY}"
            ),
            // A handle, a pane key, a sentence this module wrote, and an `io::Error` from a
            // write to a pty that is closing. None of the four has a path to lose.
            other => other.to_string(),
        };
        match self {
            Self::Unknown { .. } => envelope(
                ErrorCode::UnknownSession,
                message,
                "run `nysia session list` to see the handles this daemon holds",
                &[
                    "a handle from before a daemon restart does not survive it; create a new \
                     session",
                ],
            )
            .with_next_command_args(["nysia", "session", "list"]),
            Self::PaneTaken { .. } => envelope(
                ErrorCode::SessionBusy,
                message,
                "close the session in that pane first, or create this one without a pane key",
                &["`nysia session list` shows which pane holds what"],
            )
            .with_next_command_args(["nysia", "session", "list"]),
            Self::Spawn(err) => {
                // [`spawn_log_line`] to the daemon's own log and a phrase in the envelope.
                // The log is the daemon's own file, where the command line and the working
                // directory are what make the line worth keeping; the envelope is read by a
                // person, rendered in a window and pasted into issues.
                tracing::warn!(err = %spawn_log_line(&err), "a session could not be spawned");
                envelope(
                    ErrorCode::SpawnFailed,
                    message,
                    "check the shell is installed and on PATH",
                    &[
                        "on Windows an npm-installed shim is a `.cmd`, not an executable; name \
                         the real program",
                        "`nysia session create --profile pwsh` names a profile explicitly",
                    ],
                )
            }
            Self::PathRefused { reason, .. } => {
                // `reason` and **not** the path, which is the split `rpc::project`'s `refuse`
                // makes for the same kind of value: a folder that arrived over a socket is
                // not this daemon's to write down, and the log is a file that outlives the
                // request. The line the split falls on is **refused or used**, not whose the
                // value was: a path this arm names is one `confine_cwd` just threw out, and
                // writing it down turns a rejection into a record of it. [`spawn_log_line`]
                // says what the arm above keeps for the other half of that, and why.
                tracing::debug!(reason, "a working directory was refused");
                envelope(
                    ErrorCode::PathRefused,
                    message,
                    "pass an absolute path to a directory that exists",
                    &[
                        "the directory this refused is the one the request named; it is not \
                         repeated here because an envelope is logged and screenshotted",
                        "omit --cwd to start the session where the daemon is running",
                    ],
                )
            }
            // The agent's own name is in `message`, taken from `LaunchError` by
            // [`launch_kind`]. Naming it here instead would be the D-4 seam in the wrong
            // place, and it would be wrong twice over on the day a second agent lands.
            Self::Launch(err) => {
                tracing::warn!(%err, "an agent CLI could not be resolved");
                let LaunchError::Unavailable { agent, .. } = &err;
                // **The person can still see the file this refuses to print** — by asking
                // the same question the daemon asked, in their own terminal, where the
                // answer is already theirs. `where.exe` rather than `where`, which
                // PowerShell has taken for `Where-Object`.
                //
                // "Runs the same search" and not "prints the file", because `which` finds
                // executables only: against the case this is most often reached for on a Mac
                // — an npm script that lost its exec bit — it prints nothing at all, and a
                // next step that promises output there is one more thing to be puzzled by.
                let locate = format!(
                    "`{} {agent}` in a terminal runs the same PATH search the daemon ran",
                    if cfg!(windows) { "where.exe" } else { "which" }
                );
                envelope(
                    ErrorCode::SpawnFailed,
                    message,
                    "install the agent's CLI and make sure a terminal can run it by name",
                    &[
                        &locate,
                        "the daemon resolves the CLI on `PATH` at the moment a session is \
                         asked for, so an install does not need a daemon restart",
                        "a session started in a worktree searches the daemon's `PATH`, not \
                         the project's",
                    ],
                )
            }
            Self::CmdOnNetworkFolder => {
                // Nothing about the folder: it was refused, for `PathRefused`'s reason.
                tracing::debug!("a network folder was refused to a session run through cmd.exe");
                envelope(
                    ErrorCode::PathRefused,
                    message,
                    "open a network folder in PowerShell 7 or Git Bash, which can start in one",
                    &[
                        "a drive letter mapped to a network share resolves to the share's own \
                         path, so it is refused the same way",
                        "an agent CLI installed as an npm `.cmd` shim runs through Command \
                         Prompt too",
                    ],
                )
            }
            Self::FolderTooLong { length, source } => {
                // The spawn's own account, kept for the reason the `Spawn` arm keeps it: the
                // daemon tried to start a program in this folder, and a failure that was not
                // the length is diagnosed from the command line and the directory or not at
                // all.
                tracing::warn!(
                    length,
                    err = %spawn_log_line(&source),
                    "a session could not be started: its folder's path is too long"
                );
                envelope(
                    ErrorCode::SpawnFailed,
                    message,
                    "open the session in a folder whose path is shorter; the shell itself is \
                     not the problem",
                    &[],
                )
            }
            Self::Invalid(_) => envelope(
                ErrorCode::InvalidRequest,
                message,
                "check the verb's flags with `nysia <verb> --help`",
                &[],
            ),
            Self::Closing(_) => envelope(
                ErrorCode::SessionBusy,
                message,
                "the session is being torn down; create a new one",
                &["`nysia session list` shows what is still live"],
            )
            .retryable(false),
        }
    }
}

/// Why an agent could not be launched, as a phrase that names no file.
///
/// The agent's own name comes from the error rather than from here, which is the D-4 seam:
/// this module does not know what agents exist and must not learn.
fn launch_kind(err: &LaunchError) -> String {
    let LaunchError::Unavailable { agent, source } = err;
    format!("the {agent} CLI is unavailable: {}", resolve_kind(source))
}

/// Why a spawn failed, as a phrase that names no file.
///
/// **The variant and never the message**, which is the split `rpc::project`'s `store_kind`
/// makes. Two of these would otherwise spell a path: the profile half wraps a
/// [`ResolveError`], and [`SpawnError::Spawn`] carries argv\[0\] together with a `reason`
/// that `portable-pty` builds as ``CreateProcessW `<command line>` in cwd `<directory>`
/// failed`` — a worktree under somebody's project, in an envelope (traps register #13).
///
/// The rest are reduced too, rather than only the two that leak today. The rule a caller
/// can rely on is then the whole rule: nothing an envelope from this module says was taken
/// from an error's own text. What dropping this leaves on the daemon's log, at `warn`, is
/// [`spawn_log_line`] — which is not quite everything, and says which part and why.
fn spawn_kind(err: &SpawnError) -> String {
    match err {
        SpawnError::Profile(err) => profile_kind(err),
        SpawnError::OpenPty(_) => "a pty could not be opened".to_owned(),
        SpawnError::Spawn { .. } => "the program could not be started".to_owned(),
        SpawnError::Writer(_) => "the pty's writer half could not be taken".to_owned(),
        SpawnError::Confinement(_) => {
            "the process tree could not be confined, so closing the session could not promise \
             to take it with it"
                .to_owned()
        }
        SpawnError::Thread(_) => "one of the session's two threads could not be started".to_owned(),
    }
}

/// What the daemon's own log says about a failed spawn.
///
/// [`SpawnError`]'s own `Display`, with one value taken out of it — the near-mirror of
/// [`spawn_kind`], which takes nearly everything out because an envelope is read by a person
/// and screenshotted. A daemon's log is allowed to keep more than an envelope is. It is not
/// allowed to keep *everything*, and the difference used to be stated wrongly.
///
/// # What this line carries, said plainly, because the file used to say otherwise
///
/// #96's merge review found the comment beside [`SessionError::PathRefused`] justifying the
/// `Spawn` arm's whole-error log on the grounds that "what those name is this machine's own
/// PATH resolution". That holds for [`SessionError::Launch`] — a `&'static str` this crate
/// chose, beside a [`ResolveError`] naming what this machine's own `PATH` search turned up.
/// It did not hold here. Two caller-offered values reach this function, counted by hand with
/// the match below as the tripwire that makes somebody recount, and the honest rule is not
/// *whose the value was* but **whether the daemon used it or threw it out**.
///
/// - **The working directory, inside [`SpawnError::Spawn`]'s `reason`.** On Windows
///   `portable-pty` builds that text as ``CreateProcessW `<command line>` in cwd
///   `<directory>` failed``, so the `--cwd` a request asked for is in it verbatim. **It
///   stays.** A spawn can only fail after `confine_cwd` accepted that directory and this
///   daemon tried to start a process in it, so the line records what the daemon did rather
///   than what it refused — and a spawn that failed is diagnosed from the command line and
///   the directory it failed in or it is not diagnosed at all.
/// - **The WSL distribution name, inside [`ProfileError::BadDistro`].** **That one goes.**
///   `check_distro` threw it out for carrying a character that would be read as another
///   `wsl.exe` argument, so nothing was ever done with it: it is the shape
///   [`SessionError::PathRefused`] keeps out of the log, with none of the reason above for
///   keeping it, and the caller who sent it already knows what they sent.
///
/// Keeping the first is payable because of a layer outside this file: the log is confined to
/// the account the daemon runs as. On Unix that is *set* — `0700` on the runtime directory by
/// `rpc::endpoint` and `0600` on the file by `rpc::log_file`, which
/// `a_log_and_its_rotations_are_owner_only` holds it to. On Windows it is *inherited*, from
/// the ACL `%LOCALAPPDATA%` already carries, and both of those functions say so and do
/// nothing there; no test on that leg checks it. It is a trade and not a freedom, and
/// `a_log_line_about_a_spawn_does_not_repeat_a_refused_distribution_name` guards the half
/// that is not traded away.
fn spawn_log_line(err: &SpawnError) -> String {
    match err {
        SpawnError::Profile(ProfileError::BadDistro(_)) => {
            "could not start the session: the WSL distribution name was refused".to_owned()
        }
        // Spelled out rather than left to `_`, so a variant added to either enum has no arm
        // here and this file stops compiling until somebody decides which of the two
        // paragraphs above it falls under. The same tripwire `variant_of` is.
        SpawnError::Profile(
            ProfileError::Unavailable { .. } | ProfileError::WrongPlatform { .. },
        )
        | SpawnError::OpenPty(_)
        | SpawnError::Spawn { .. }
        | SpawnError::Writer(_)
        | SpawnError::Confinement(_)
        | SpawnError::Thread(_) => err.to_string(),
    }
}

/// Why a shell profile could not be turned into a command, as a phrase that names no file.
fn profile_kind(err: &ProfileError) -> String {
    match err {
        ProfileError::Unavailable { profile, source } => {
            format!(
                "the {profile} profile is unavailable: {}",
                resolve_kind(source)
            )
        }
        // A `&'static str` this crate chose, not something a caller handed it.
        ProfileError::WrongPlatform { profile } => {
            format!("the {profile} profile does not exist on this platform")
        }
        // The name is not repeated, for [`SessionError::PathRefused`]'s reason: it arrived
        // over a socket, and a caller that passed one distribution knows which one it passed.
        ProfileError::BadDistro(_) => {
            "the WSL distribution name carries a character that would be read as another \
             argument"
                .to_owned()
        }
    }
}

/// What resolution said, as a phrase that names no file.
///
/// Every variant of [`ResolveError`] except `NotFound` carries the candidate path it
/// rejected, and `NotFound` carries the program as the caller spelled it — which is a bare
/// name for an agent and can be an absolute path for a profile. So both are reduced: the
/// path-carrying ones to what was wrong with the file, and the name to its final component.
fn resolve_kind(err: &ResolveError) -> String {
    match err {
        ResolveError::NotFound { program } => {
            format!("{} was not found on PATH", program_name(program))
        }
        ResolveError::NotAFile { .. } => {
            "what that name resolves to is a directory, not a file".to_owned()
        }
        ResolveError::NotExecutable { .. } => {
            "the file it resolves to has no execute permission for anyone".to_owned()
        }
        ResolveError::UnknownExtension { .. } => {
            "the file it resolves to is not something this platform can launch".to_owned()
        }
        ResolveError::MissingInterpreter { interpreter, .. } => format!(
            "it is a shim needing {}, which is not installed",
            program_name(interpreter)
        ),
        ResolveError::UnsafeArgument { .. } => {
            "an argument to it carries a character `cmd.exe` would act on".to_owned()
        }
    }
}

/// The final component of a program name, cut at **both** platforms' separators.
///
/// `Path::file_name` splits on the host's separators only, so a name spelled with the other
/// platform's would come back whole — which is a path that leaks on one leg and not the
/// other, and therefore a path that leaks in production and not in the test that looked.
fn program_name(program: &str) -> &str {
    program.rsplit(['/', '\\']).next().unwrap_or(program)
}

/// Unix milliseconds, now.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Take a lock, treating poisoning as recoverable.
///
/// Every structure guarded here is a map or a grid, and a panic while one was held leaves it
/// consistent. Refusing every later request because one thread panicked would turn a bug in
/// one session into an outage for all of them — which is the opposite of what D-1 promises.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One session the daemon owns: its pty, its terminal state, and the clients watching it.
pub struct OwnedSession {
    handle: SessionHandle,
    pane_key: PaneKey,
    incarnation: Incarnation,
    kind: SessionKind,
    title: String,
    created_at_ms: u64,
    /// The folder this session is running in, in the spelling the project verbs compare.
    ///
    /// `None` for a session that was started with no `cwd`, which inherits the daemon's own
    /// and therefore belongs to no worktree in particular. Resolved once, here, rather than
    /// per `project_list`: attribution asks "is this session inside that worktree" for every
    /// session of every project, and canonicalising on that path would put one blocking
    /// `stat` per session behind a verb the sidebar waits on.
    cwd: Option<CanonicalPath>,
    pty: Arc<PtySession>,
    vt: Arc<Mutex<TerminalState>>,
    sinks: Arc<Mutex<Vec<Arc<StreamSink>>>>,
    last_output_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    pump: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for OwnedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedSession")
            .field("handle", &self.handle)
            .field("pane_key", &self.pane_key)
            .field("incarnation", &self.incarnation)
            .finish_non_exhaustive()
    }
}

impl OwnedSession {
    /// The runtime-scoped routing id.
    #[must_use]
    pub fn handle(&self) -> &SessionHandle {
        &self.handle
    }

    /// The durable pane identity.
    #[must_use]
    pub fn pane_key(&self) -> &PaneKey {
        &self.pane_key
    }

    /// This spawn, distinct from a later relaunch in the same pane.
    #[must_use]
    pub fn incarnation(&self) -> &Incarnation {
        &self.incarnation
    }

    /// The folder this session was started in, resolved.
    ///
    /// `None` when it was started without one. It is **where the session started**, not where
    /// the shell is now: a person who types `cd` moves the shell and not this, and following
    /// them would mean the sidebar lost a session out of its worktree the moment they looked
    /// somewhere else.
    #[must_use]
    pub fn cwd(&self) -> Option<&CanonicalPath> {
        self.cwd.as_ref()
    }

    /// The session leader's pid, which is where §3.2's ancestry walk ends.
    #[must_use]
    pub fn leader_pid(&self) -> Option<u32> {
        self.pty.pid()
    }

    /// The row this session contributes to `session list`.
    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            handle: self.handle.clone(),
            pane_key: self.pane_key.clone(),
            kind: self.kind,
            title: self.title.clone(),
            created_at_ms: self.created_at_ms,
            // Trap 11: the child's `wait()`, never the pty's EOF. A backgrounded server
            // holding the slave open keeps EOF away long after the shell is gone, and a
            // client that believed EOF would show a dead session as live.
            exit_status: self.pty.exit_status().as_ref().map(exit_status),
        }
    }

    /// Read the rendered screen, or the scrollback from a cursor.
    ///
    /// Screen is the default, and the default is the point: §7.2 shows what the alternative
    /// costs — Orca's `terminal read` returns the accumulated escape-stripped stream, so a
    /// `clear` typed one key at a time reads back as `cclclecleaclear`.
    #[must_use]
    pub fn read(
        &self,
        mode: ReadMode,
        cursor: Option<LineCursor>,
        limit: Option<u32>,
    ) -> TerminalReadResult {
        let limit = limit
            .map_or(DEFAULT_READ_LIMIT, |limit| limit as usize)
            .clamp(1, MAX_READ_LIMIT);
        let cursor = cursor.unwrap_or(LineCursor::START).get();
        let vt = lock(&self.vt);
        let read = vt.read(vt_mode(mode), cursor, limit);
        let log_end = vt.line_log().next_id();
        drop(vt);

        let lines = if read.text.is_empty() {
            Vec::new()
        } else {
            read.text.split('\n').map(str::to_owned).collect()
        };
        TerminalReadResult {
            lines,
            cursor: LineCursor(read.next_cursor),
            mode,
            // True only when the page stopped short of what the log holds. A caller that
            // asked for the screen is never told it was truncated: the screen is complete by
            // definition, and there is nothing further to page to.
            truncated: mode == ReadMode::Stream && read.next_cursor < log_end,
        }
    }

    /// Write to the session's input.
    ///
    /// Proto fixes the order — interrupt, then text, then enter — so one frame can say
    /// "cancel whatever is running and then send this command".
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Closing`] once the session has been torn down.
    pub fn send(&self, request: &TerminalSend) -> Result<(), SessionError> {
        let mut bytes = Vec::new();
        if request.interrupt {
            // Not a signal: ConPTY has none. This is the byte the line discipline or the
            // foreground TUI interprets, which is what a person pressing Ctrl-C sends.
            bytes.push(0x03);
        }
        bytes.extend_from_slice(request.text.as_bytes());
        if request.enter {
            bytes.push(b'\r');
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.pty.write(&bytes).map_err(SessionError::Closing)
    }

    /// Resize the pty and the grid together.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Invalid`] for a degenerate size — ConPTY panics on a
    /// zero-sized console rather than refusing one — and [`SessionError::Closing`] once the
    /// session has been torn down.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        if cols == 0 || rows == 0 {
            return Err(SessionError::Invalid(
                "a terminal is at least one cell in each direction".to_owned(),
            ));
        }
        let size = TerminalSize::new(cols, rows);
        self.pty.resize(size).map_err(SessionError::Closing)?;
        lock(&self.vt).resize(size);
        Ok(())
    }

    /// Block until the session exits or goes quiet, or until `timeout` elapses.
    ///
    /// Blocking, and meant to be called from a blocking context. A timed-out wait is not an
    /// error: the caller asked for a bounded wait and got a bounded answer.
    #[must_use]
    pub fn wait(&self, wait_for: WaitFor, timeout: Option<Duration>) -> WaitOutcome {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            if let Some(status) = self.pty.exit_status() {
                return WaitOutcome::Exited {
                    status: exit_status(&status),
                };
            }
            if wait_for == WaitFor::Idle {
                let quiet = now_ms().saturating_sub(self.last_output_ms.load(Ordering::Acquire));
                if Duration::from_millis(quiet) >= IDLE_QUIET {
                    return WaitOutcome::Idle;
                }
            }
            match deadline {
                Some(deadline) if Instant::now() >= deadline => return WaitOutcome::TimedOut,
                _ => {}
            }
            // A short poll rather than a condvar per wait target: `exit` has one already and
            // `idle` cannot have one, because "stopped producing output" is the absence of an
            // event rather than an event.
            std::thread::sleep(WAIT_POLL);
        }
    }

    /// Start routing this session's output to `sink`.
    ///
    /// The replay ring goes first, so a client that attaches to a session that has been
    /// running for an hour sees what is on the screen rather than an empty pane. That is the
    /// reattach half of the D-1 acceptance: the window died, the session did not, and the
    /// scrollback comes back.
    ///
    /// **And the replay is marked where it ends.** A replay is indistinguishable from live
    /// output in the frames themselves — it *is* the bytes the child once wrote, escape
    /// sequences intact — so it carries every `ESC[6n` and `ESC[c` the child ever emitted,
    /// and a terminal emulator answers a query when it parses one whether or not it is
    /// reading history. Those answers leave as input: on Windows ConPTY reads the `…R` of a
    /// cursor-position report as F3, which `cmd` treats as recall-previous-command, so a
    /// re-attached pane came back showing a line nobody typed. One
    /// [`FrameKind::ReplayEnd`] closes that, exactly once per attach, after the last replayed
    /// byte and before anything live — see [`nysia_proto::stream`] for what a client owes it.
    pub fn attach(&self, sink: &Arc<StreamSink>) {
        let stream = sink.stream_id();
        sink.send(&sink.opening_grant());
        let replay = lock(&self.vt).replay();
        // The window's chunk, not the flush ceiling. A 64 KiB frame charged against a
        // smaller allowance drives the credit deeply negative in one go, and the stream then
        // sends nothing until the client has acked its way back above zero — which under a
        // tight window is a pane that sits blank after a re-attach for no reason the client
        // can see. Chunked to the window, a replay longer than the opening allowance stops at
        // it and says so below, which is the behaviour the rest of this function already
        // documents.
        for chunk in replay.chunks(sink.window().chunk as usize) {
            if sink.send(&Frame::new(FrameKind::Output, stream, chunk.to_vec()))
                != SendOutcome::Sent
            {
                // A client that cannot take its own replay is one the pump would stall on
                // immediately. Better to hand it a short history than to wedge the session.
                tracing::warn!(
                    stream = stream.get(),
                    "dropped part of a replay the client could not take"
                );
                break;
            }
        }
        // **Sent on every path out of that loop, including the `break`.** A client holds its
        // outbound input from the moment it attaches until this frame arrives, so a marker
        // skipped because the replay was truncated would leave the pane mute until the
        // client's own deadline rescued it — and it is the sessions with the most scrollback,
        // the ones most likely to outrun the opening allowance, that would get it. It costs
        // no credit: only `Output` spends, so a marker cannot be refused for a window the
        // replay just drained.
        if sink.send(&Frame::empty(FrameKind::ReplayEnd, stream)) != SendOutcome::Sent {
            // The outbox was full or the client has gone. Nothing here can retry — the
            // replay it would have followed is already partly on the floor — but a pane that
            // stays mute until its deadline fires needs a reason in the log, or the symptom
            // is "input does nothing for a few seconds after re-attach" with nothing to read.
            tracing::warn!(
                stream = stream.get(),
                "the replay boundary was not delivered; the client will fall back to its \
                 deadline before it accepts input"
            );
        }
        if let Some(status) = self.pty.exit_status() {
            // A session that has already exited still owes the client the exit frame, or an
            // attaching client waits forever for an event that happened before it arrived.
            send_exit(sink, stream, &exit_status(&status));
        }
        lock(&self.sinks).push(Arc::clone(sink));
    }

    /// The terminal state this session feeds, for tests that need to hold its lock.
    ///
    /// Holding it is how a test pins down the attach ordering without a sleep: the replay
    /// ring cannot be read while it is held, so a daemon that replays *before* it answers
    /// cannot answer at all, while a daemon that answers first is unaffected. What would
    /// otherwise be a race becomes an assertion either way.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn terminal_state(&self) -> &Arc<Mutex<TerminalState>> {
        &self.vt
    }

    /// Tear down the process tree and stop the pump.
    ///
    /// Ordered deliberately. The pump is told to stop first so it stops holding the output
    /// queue open; `PtySession::shutdown` then runs `ClosePseudoConsole` from this thread —
    /// never the reader's — after setting the flag that lets the reader discard rather than
    /// block, which is trap 6 and the deadlock it names.
    pub fn close(&self) -> Option<ExitStatus> {
        self.stop.store(true, Ordering::Release);
        let status = self.pty.shutdown(DEFAULT_GRACE).as_ref().map(exit_status);
        for sink in lock(&self.sinks).drain(..) {
            if let Some(status) = &status {
                send_exit(&sink, sink.stream_id(), status);
            }
            sink.close();
        }
        if let Some(pump) = lock(&self.pump).take() {
            let _ = pump.join();
        }
        status
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Translate `portable-pty`'s exit status into the wire's.
///
/// The two disagree about signals on purpose. `portable-pty` carries a *name* — it runs the
/// number through `strsignal` — and proto carries a number, because a number is what a client
/// can branch on. The table below converts the spellings both platforms produce.
///
/// A name the table does not know falls back to [`ExitStatus::Exited`] with the code
/// `portable-pty` derived, which is honest in a way that inventing a signal number would not
/// be: the caller sees a real exit code rather than a signal Nysia made up.
fn exit_status(status: &portable_pty::ExitStatus) -> ExitStatus {
    match status.signal().and_then(signal_number) {
        Some(signal) => ExitStatus::Signaled { signal },
        None => ExitStatus::Exited {
            code: i32::try_from(status.exit_code()).unwrap_or(i32::MAX),
        },
    }
}

/// The number behind a signal name, for the signals a session teardown actually produces.
fn signal_number(name: &str) -> Option<i32> {
    // `portable-pty` falls back to this spelling when `strsignal` returns null, and it is the
    // one case where the number is already right there in the string.
    if let Some(number) = name.strip_prefix("Signal ") {
        return number.trim().parse().ok();
    }
    match name {
        "Hangup" => Some(1),
        "Interrupt" => Some(2),
        "Quit" => Some(3),
        "Illegal instruction" => Some(4),
        // macOS says "Abort trap", Linux says "Aborted".
        "Abort trap" | "Aborted" => Some(6),
        "Floating point exception" => Some(8),
        "Killed" => Some(9),
        "Bus error" => Some(10),
        "Segmentation fault" => Some(11),
        "Broken pipe" => Some(13),
        "Alarm clock" => Some(14),
        "Terminated" => Some(15),
        _ => None,
    }
}

/// Write an exit frame to one sink.
fn send_exit(sink: &StreamSink, stream: nysia_proto::StreamId, status: &ExitStatus) {
    let payload = serde_json::to_vec(status).unwrap_or_default();
    sink.send(&Frame::new(FrameKind::Exit, stream, payload));
}

/// Translate the wire's read mode into the grid's.
fn vt_mode(mode: ReadMode) -> VtReadMode {
    match mode {
        ReadMode::Screen => VtReadMode::Screen,
        ReadMode::Stream => VtReadMode::Stream,
    }
}

/// Translate the wire's shell profile into the one the pty layer spawns.
///
/// `None` takes the platform default, which is what a client with no opinion sends — and on
/// Unix that is the user's login shell, which the wire has no spelling for. That asymmetry is
/// deliberate: proto's four profiles are the ones a menu offers, and "whatever your shell is"
/// is not a menu entry.
fn core_profile(profile: Option<&WireProfile>) -> ShellProfile {
    match profile {
        None => ShellProfile::platform_default(),
        Some(WireProfile::Pwsh) => ShellProfile::PowerShell7,
        Some(WireProfile::Cmd) => ShellProfile::CommandPrompt,
        Some(WireProfile::GitBash) => ShellProfile::GitBash,
        Some(WireProfile::Wsl { distro }) => ShellProfile::Wsl {
            distro: distro.clone(),
        },
    }
}

/// The shells a menu offers, in [`WireProfile`]'s order: WSL once, as the distribution a
/// bare `wsl.exe` opens, because that is the one WSL entry a menu has.
const MENU_PROFILES: [WireProfile; 4] = [
    WireProfile::Pwsh,
    WireProfile::Cmd,
    WireProfile::GitBash,
    WireProfile::Wsl { distro: None },
];

/// Which of the menu's shells this daemon can launch now, and why not for the rest.
///
/// Each profile is asked [`ShellProfile::launchable`] — the question a spawn asks before it
/// starts anything, answered by the same code, against this daemon's `PATH` **at the moment
/// of the call**. Nothing is cached, so the answer is never older than the request: a shell
/// that appears in or vanishes from a directory already on that `PATH` is reflected the next
/// time a window asks. A directory an installer *adds* to `PATH` is not — this process keeps
/// the `PATH` it started with — so that shell is offered once the daemon is restarted from an
/// environment that has it.
///
/// The reason is [`profile_kind`]'s phrase — the same one a refused launch carries in its
/// envelope — which names the shell and what resolution found, and never the file a
/// resolution rejected.
///
/// Blocking: filesystem probes, several per profile. The caller runs it on the blocking pool
/// and holds no lock across it.
#[must_use]
pub fn profile_availability() -> Vec<ProfileAvailability> {
    MENU_PROFILES
        .iter()
        .map(|profile| ProfileAvailability {
            profile: profile.clone(),
            unavailable: core_profile(Some(profile))
                .launchable()
                .err()
                .map(|err| profile_kind(&err)),
        })
        .collect()
}

/// What a request asks to be run, as a spec with nothing else filled in yet.
///
/// The one place the two session kinds differ, and the difference is only *where the program
/// comes from*: a shell is named on the wire and [`ShellProfile`] resolves it, an agent's CLI
/// is resolved by [`crate::agent`] and arrives already validated. Everything after this —
/// the confined directory, the caller's environment, the pane key that goes on last, the
/// pump, the teardown — is the same code for both, which is the point of returning a spec
/// rather than branching twice.
///
/// **Nothing here names an agent** (D-4). [`crate::agent::launch`] answers with a label and
/// an argv, and the agent's own name reaches a person only through [`LaunchError`]'s
/// `Display`. A literal in this file would be the seam in the wrong place, and
/// `no-claude-specifics-outside-agent` would say so with a line number.
///
/// # Errors
///
/// Returns [`SessionError::Launch`] when the agent's CLI cannot be resolved to something
/// this platform will launch — **before** the spawn, because neither platform reports that
/// usefully afterwards (traps register #8 and #11).
fn program_for(request: &SessionCreate) -> Result<SessionSpec, SessionError> {
    match request.kind {
        SessionKind::Shell => Ok(SessionSpec::new(core_profile(request.profile.as_ref()))),
        SessionKind::Agent => {
            // No arguments: a session is the CLI's interactive form, and everything a person
            // would pass on a command line they type at the prompt instead.
            let launch = crate::agent::launch(NO_ARGUMENTS).map_err(SessionError::Launch)?;
            SessionSpec::for_program(launch.argv().to_vec(), launch.label())
                .map_err(SessionError::Spawn)
        }
    }
}

/// What an agent session is launched with, which is nothing.
const NO_ARGUMENTS: [&std::ffi::OsStr; 0] = [];

/// Confine a requested working directory, and resolve it to the spelling a spawn can use.
///
/// v0.1's confinement is deliberately narrow: absolute, existing, and a directory. §7.5's
/// `safe_join` / `path_confine` — the worktree-relative rules — arrive with the worktree
/// module, and claiming them here before they exist would be worse than saying what this
/// actually checks.
///
/// # Why the answer is a [`CanonicalPath`] and not what `fs::canonicalize` returned
///
/// On Windows `fs::canonicalize` answers in the verbatim form, `\\?\C:\…`, and **`cmd.exe`
/// cannot run in one**. Handed it as a working directory, it prints *"UNC paths are not
/// supported. Defaulting to Windows directory."* and opens in `C:\Windows` — a Command
/// Prompt asked for a project, running somewhere else entirely, with only a line of its own
/// banner to say so. `pwsh` and the agent CLI accept the prefix, which is how it went
/// unnoticed: every session anybody had looked at was one of those.
///
/// [`CanonicalPath`] drops the prefix wherever the plain spelling means the same folder, and
/// it is also the spelling `git` prints and the project verbs compare — so the directory a
/// session is spawned in and the directory it is listed under are one value rather than two
/// that have to be kept in step. A path that must keep its prefix (at or over `MAX_PATH`, or
/// with a component Win32 would rewrite) keeps it, and a shell that cannot use it says so.
///
/// Every refusal's `reason` is a sentence written here or the OS's own error, which names no
/// path; [`PathError`]'s `Display` does, which is why it is matched on and never rendered.
fn confine_cwd(cwd: Option<&PathBuf>) -> Result<Option<CanonicalPath>, SessionError> {
    let Some(path) = cwd else {
        return Ok(None);
    };
    let refuse = |reason: String| SessionError::PathRefused {
        path: path.clone(),
        reason,
    };
    if !path.is_absolute() {
        return Err(refuse("a working directory must be absolute".to_owned()));
    }
    CanonicalPath::of(path).map(Some).map_err(|err| {
        refuse(match err {
            PathError::Missing { .. } => "it does not exist".to_owned(),
            PathError::Unreadable { source, .. } => source.to_string(),
            PathError::NotADirectory { .. } => "it is not a directory".to_owned(),
        })
    })
}

/// The longest working directory Windows will start a program in, in UTF-16 units.
///
/// `MAX_PATH` less the terminating null and the trailing separator a current directory always
/// carries. Measured as well as documented: a Command Prompt asked for a 258-character folder
/// started there, and one asked for 259 did not start at all, on a machine with
/// `LongPathsEnabled` off. `a_folder_too_long_to_start_in_says_so_rather_than_blaming_the_shell`
/// holds both sides of it.
const MAX_WORKING_DIRECTORY: usize = 258;

/// Whether `program` runs through `cmd.exe`: a Command Prompt, or a program that resolved to a
/// batch shim, which `cmd.exe` runs on its behalf — an agent CLI installed by npm is one.
///
/// **Read off argv\[0\], and not from `ResolvedProgram::through_cmd`**, which is dropped
/// before [`SessionProgram::Resolved`] is built. That flag answers a different question:
/// whether `cmd.exe` parses the *arguments* again (`pty::resolve`'s BatBadBut guard). It is
/// `false` for a program resolved straight to `cmd.exe`, which still cannot start in a network
/// folder. The question here is which process is started in the folder, and argv\[0\] is
/// that process. If the two ever need to agree, carry the answer to this question through
/// `SessionProgram`; do not reuse the flag.
fn runs_through_cmd(program: &SessionProgram) -> bool {
    match program {
        SessionProgram::Shell(profile) => matches!(profile, ShellProfile::CommandPrompt),
        SessionProgram::Resolved { argv, .. } => argv.first().is_some_and(|first| {
            program_name(&first.to_string_lossy()).eq_ignore_ascii_case("cmd.exe")
        }),
    }
}

/// Whether `path` is a network folder: `\\server\share\…`, or `\\?\UNC\server\share\…`.
///
/// Read from the spelling rather than from `Path::components`, which parses a UNC prefix on
/// Windows only. The rule is the same on both platforms, and a test of it runs on both.
fn is_network_folder(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    let starts = |prefix: &str| {
        text.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    starts(r"\\?\UNC\") || (starts(r"\\") && !starts(r"\\?\") && !starts(r"\\.\"))
}

/// `path`'s length the way Windows measures a working directory: UTF-16 units, in the
/// spelling without the `\\?\` prefix, which [`CanonicalPath`] keeps on a path this long.
fn windows_length(path: &std::path::Path) -> usize {
    let text = path.to_string_lossy();
    let plain = match text.get(..8).zip(text.get(8..)) {
        Some((head, rest)) if head.eq_ignore_ascii_case(r"\\?\UNC\") => format!(r"\\{rest}"),
        _ => text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned(),
    };
    plain.encode_utf16().count()
}

/// A spawn that failed, told apart from one Windows refused for its folder's length.
///
/// **Inferred from the length, not read off the failure.** It runs only after a spawn has
/// already failed, so it cannot refuse a folder that would have started. But nothing in the
/// failure says the length was the cause: [`SpawnError::Spawn`] carries the pty layer's
/// sentence, not an error code, and the OS text inside it is localised. The threshold was
/// measured with `LongPathsEnabled` off. On a machine where it is on and a program can start
/// in a longer folder, a failure for some other reason in a folder over the threshold is
/// still put down to the length. The daemon's log keeps the spawn's own account for that
/// case.
fn spawn_refusal(err: SpawnError, cwd: Option<&CanonicalPath>) -> SessionError {
    if cfg!(windows)
        && matches!(err, SpawnError::Spawn { .. })
        && let Some(length) = cwd
            .map(|cwd| windows_length(cwd.as_path()))
            .filter(|length| *length > MAX_WORKING_DIRECTORY)
    {
        return SessionError::FolderTooLong {
            length,
            source: err,
        };
    }
    SessionError::Spawn(err)
}

/// The pane key every session carries in its environment.
///
/// §3.2 names it as the hint: a process inside a pane can read it without a round trip, and
/// nothing may treat it as proof. `nysia hook` sends it as `pane_hint` and the daemon
/// overrules it from the process tree; the one place it is load-bearing is the disk spool,
/// where the daemon that could have proved anything is the daemon that was not there.
pub const PANE_KEY_VAR: &str = "NYSIA_PANE_KEY";

/// Every session the daemon holds, and the incarnation counter behind them.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: Mutex<BTreeMap<String, Arc<OwnedSession>>>,
    /// The next generation for each pane key.
    ///
    /// Daemon-owned, as proto requires: a client that counted for itself would restart at
    /// zero across its own restart and make two spawns indistinguishable, which is the one
    /// thing an incarnation exists to prevent.
    generations: Mutex<HashMap<String, u32>>,
}

impl SessionRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer every refusal a request can earn **before** its caller changes anything.
    ///
    /// [`Self::create`] is the last step of `Start →` and the steps before it create a branch
    /// and a worktree in somebody's repository. A refusal that only arrives once the session
    /// is being spawned is therefore a refusal that arrives *after* the durable part of the
    /// verb has already happened, and `ProjectStarted` has no field to mention what was left
    /// behind.
    ///
    /// So this is every check [`Self::create`] makes that needs nothing built first: the
    /// size, the pane, and — for an agent — whether the CLI resolves at all. It is a
    /// pre-flight and not a promise: the pane can be taken and the CLI uninstalled between
    /// this and the spawn, which is why `create` still makes all three itself. Passing here
    /// means the request is not *already* impossible, which is the only thing a caller about
    /// to create a worktree needs to know.
    ///
    /// # Errors
    ///
    /// Returns the same [`SessionError`] the request would have earned from [`Self::create`].
    pub fn precheck(&self, request: &SessionCreate) -> Result<(), SessionError> {
        if request.cols == 0 || request.rows == 0 {
            return Err(SessionError::Invalid(
                "a session is at least one cell in each direction".to_owned(),
            ));
        }
        if let Some(pane) = &request.pane_key
            && let Some(handle) = self.pane_holder(pane)
        {
            return Err(SessionError::PaneTaken {
                pane: pane.clone(),
                handle,
            });
        }
        program_for(request).map(|_| ())
    }

    /// Spawn a session and start its pump.
    ///
    /// Both kinds come through here and take the same path (§3): [`program_for`] decides what
    /// is spawned and everything after it — the confined directory, the caller's environment,
    /// the daemon's pane key last, the pump, the teardown when the pump will not start — is
    /// one piece of code serving a shell and an agent alike. That is what makes "every tab is
    /// a session" true of the daemon rather than only of the window.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the pane is taken, the working directory is refused, the
    /// request is not well formed, the agent's CLI cannot be resolved, or the program will
    /// not start.
    pub fn create(&self, request: &SessionCreate) -> Result<SessionCreated, SessionError> {
        if request.cols == 0 || request.rows == 0 {
            return Err(SessionError::Invalid(
                "a session is at least one cell in each direction".to_owned(),
            ));
        }

        let pane_key = match &request.pane_key {
            Some(pane) => {
                // Refused, never silently rebound. Two clients racing for one pane is a real
                // case, and the loser has to be told rather than left believing it owns a
                // session it does not.
                if let Some(existing) = self.pane_holder(pane) {
                    return Err(SessionError::PaneTaken {
                        pane: pane.clone(),
                        handle: existing,
                    });
                }
                pane.clone()
            }
            // The CLI has no pane and no tab. Requiring a key from it would put the minting
            // logic in every client rather than in the one process that owns persistence.
            None => synthetic_pane(),
        };

        let requested = match &request.cwd {
            None => None,
            Some(WorkingDirectory::Path { path }) => Some(path),
            // Refused rather than read as "no directory", which is the one answer this must
            // never give: a project the registry cannot resolve would otherwise open in the
            // daemon's own directory, and that is the defect the variant exists to fix. The
            // registry holds sessions and not registrations, so the folder is looked up by
            // `rpc::project`'s `ProjectService::create_session`, which hands this a `Path`.
            Some(WorkingDirectory::Project { .. }) => {
                return Err(SessionError::Invalid(
                    "a project's folder is resolved from its registration, and this session \
                     registry holds none; the request has to reach the project service"
                        .to_owned(),
                ));
            }
        };

        let mut spec = program_for(request)?;
        let title = spec.program.label();
        let size = TerminalSize::new(request.cols, request.rows);
        spec = spec.with_size(size);
        let cwd = confine_cwd(requested)?;
        if let Some(cwd) = &cwd {
            // Refused rather than spawned: `cmd.exe` would start, say it cannot use the
            // folder, and run in `C:\Windows` instead. Nothing this side would ever see it.
            if runs_through_cmd(&spec.program) && is_network_folder(cwd.as_path()) {
                return Err(SessionError::CmdOnNetworkFolder);
            }
            spec = spec.with_cwd(cwd.as_path());
        }
        for (key, value) in &request.env_overrides {
            // `with_env`, never `with_env_overriding_the_scrub`: the scrub runs after this,
            // so a caller cannot reintroduce `ANTHROPIC_API_KEY` or a
            // `CLAUDE_CODE_CHILD_SESSION` by naming it here. A security default an ordinary
            // caller can undo is a suggestion, not a default.
            spec = spec.with_env(key.as_str(), value.as_str());
        }
        // **After** the overrides, so the last word is the daemon's. §3.2 makes this a hint
        // and never the proof — the daemon resolves a hook's pane from the process tree and
        // only logs a hint that disagrees — but it is the hint `nysia hook` files a spooled
        // row under when no daemon is listening, and a caller that could set it would be
        // choosing which pane its own unreachable status is later restored into.
        spec = spec.with_env(PANE_KEY_VAR, pane_key.as_str());

        let (pty, output) =
            PtySession::spawn(spec).map_err(|err| spawn_refusal(err, cwd.as_ref()))?;
        let handle = pty.handle().clone();
        let incarnation = self.next_incarnation(&pane_key);

        let session = Arc::new(OwnedSession {
            handle: handle.clone(),
            pane_key: pane_key.clone(),
            incarnation: incarnation.clone(),
            kind: request.kind,
            title,
            created_at_ms: now_ms(),
            cwd,
            pty: Arc::new(pty),
            vt: Arc::new(Mutex::new(TerminalState::new(size, VtConfig::default()))),
            sinks: Arc::new(Mutex::new(Vec::new())),
            last_output_ms: Arc::new(AtomicU64::new(now_ms())),
            stop: Arc::new(AtomicBool::new(false)),
            pump: Mutex::new(None),
        });
        let pump = spawn_pump(&session, output).map_err(|err| {
            // A session with no pump is a session whose grid never updates. Tearing it down
            // beats handing back a handle that will never show anything.
            session.close();
            SessionError::Spawn(crate::pty::SpawnError::Thread(err))
        })?;
        *lock(&session.pump) = Some(pump);

        lock(&self.sessions).insert(handle.as_str().to_owned(), Arc::clone(&session));
        Ok(SessionCreated {
            handle,
            pane_key,
            incarnation,
        })
    }

    /// Every session, oldest handle first.
    #[must_use]
    pub fn list(&self) -> Vec<SessionSummary> {
        lock(&self.sessions)
            .values()
            .map(|session| session.summary())
            .collect()
    }

    /// The sessions running inside `folder` and in no deeper one of `folders`.
    ///
    /// Containment rather than equality, which is [`CanonicalPath::contains`]'s own rule and
    /// the same one [`crate::git::Repository`] uses to decide which worktree a registered
    /// folder is in: a session started in `…/repo/crates/core` belongs to the worktree at
    /// `…/repo`. A session with no `cwd` belongs to no worktree — it inherited the daemon's,
    /// which is nobody's project.
    ///
    /// # Why the whole set has to be passed in
    ///
    /// **Worktrees nest, and this is the code that made them.** Nysia puts the ones it
    /// creates at `<project>/.nysia/worktrees/<slug>` (D-6, `crate::worktree::WORKTREE_BASE`),
    /// so every one of them is inside the main worktree, and `contains` is true of both. A
    /// session in `…/repo/.nysia/worktrees/feat-x` was listed under `feat/x` *and* under
    /// `main` — one session in the sidebar twice, once under a worktree it is not in.
    ///
    /// The doc comment this replaces justified that with "git refuses to place a worktree
    /// inside another's working tree". Git does not, `git worktree list` prints both, and the
    /// shape it was describing is the one this repository ships.
    ///
    /// The rule is the one [`crate::worktree::mark_primary`] already uses for the same
    /// question: **longest match**. A session belongs to the deepest worktree containing it,
    /// which is one answer per session however many worktrees nest around it.
    ///
    /// `folders` is every worktree of the repository that has a directory — including the
    /// detached and branchless ones the wire cannot spell and `project` leaves out of its
    /// answer. It has to be: a session whose deepest worktree is one of those belongs to it,
    /// and passing only the listable ones would float it up to the main worktree instead. Such
    /// a session is listed under nothing, which is the honest answer — its worktree is not on
    /// the wire either.
    #[must_use]
    pub fn summaries_under(
        &self,
        folder: &CanonicalPath,
        folders: &[CanonicalPath],
    ) -> Vec<SessionSummary> {
        lock(&self.sessions)
            .values()
            .filter(|session| {
                session
                    .cwd()
                    .is_some_and(|cwd| deepest_containing(folders, cwd) == Some(folder))
            })
            .map(|session| session.summary())
            .collect()
    }

    /// The session a handle names.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Unknown`] when nothing answers to it.
    pub fn get(&self, handle: &SessionHandle) -> Result<Arc<OwnedSession>, SessionError> {
        lock(&self.sessions)
            .get(handle.as_str())
            .map(Arc::clone)
            .ok_or_else(|| SessionError::Unknown {
                handle: handle.clone(),
            })
    }

    /// Close a session and forget it.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Unknown`] when nothing answers to the handle.
    pub fn close(&self, handle: &SessionHandle) -> Result<(), SessionError> {
        let session = lock(&self.sessions)
            .remove(handle.as_str())
            .ok_or_else(|| SessionError::Unknown {
                handle: handle.clone(),
            })?;
        session.close();
        Ok(())
    }

    /// Every session leader's pid, for §3.2's ancestry walk.
    #[must_use]
    pub fn leaders(&self) -> HashMap<u32, (SessionHandle, Incarnation)> {
        lock(&self.sessions)
            .values()
            .filter_map(|session| {
                session.leader_pid().map(|pid| {
                    (
                        pid,
                        (session.handle().clone(), session.incarnation().clone()),
                    )
                })
            })
            .collect()
    }

    /// How many sessions the daemon holds.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.sessions).len()
    }

    /// Whether the daemon holds no session — half of the idle-retire condition.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Close every session. Called when the daemon is retiring.
    pub fn close_all(&self) {
        let sessions: Vec<Arc<OwnedSession>> =
            lock(&self.sessions).values().map(Arc::clone).collect();
        lock(&self.sessions).clear();
        for session in sessions {
            session.close();
        }
    }

    /// Which session holds `pane`, if any.
    fn pane_holder(&self, pane: &PaneKey) -> Option<SessionHandle> {
        lock(&self.sessions).values().find_map(|session| {
            (session.pane_key() == pane && session.summary().exit_status.is_none())
                .then(|| session.handle().clone())
        })
    }

    /// The next incarnation for `pane`, counting from zero and owned here.
    fn next_incarnation(&self, pane: &PaneKey) -> Incarnation {
        let mut generations = lock(&self.generations);
        let generation = generations.entry(pane.as_str().to_owned()).or_insert(0);
        let incarnation = Incarnation::new(pane, *generation);
        *generation = generation.saturating_add(1);
        incarnation
    }
}

/// The deepest of `folders` containing `path`, or `None` when none of them does.
///
/// Depth is the component count, which is the same measure
/// [`crate::worktree::mark_primary`] takes for the same reason — and it is measured rather
/// than taken from string length, because one folder name is not one component and a longer
/// string can be a shallower path.
///
/// Ties cannot arise: two entries containing `path` at equal depth would have to be the same
/// directory, and `git worktree list` does not print one twice. If one ever did, `max_by_key`
/// takes the **last** of them and the answer is still exactly one worktree — which is what
/// matters here, and is said the way the code actually behaves rather than the way a reader
/// might assume.
fn deepest_containing<'a>(
    folders: &'a [CanonicalPath],
    path: &CanonicalPath,
) -> Option<&'a CanonicalPath> {
    folders
        .iter()
        .filter(|folder| folder.contains(path))
        .max_by_key(|folder| folder.as_path().components().count())
}

/// A pane key for a caller that owns no pane.
fn synthetic_pane() -> PaneKey {
    let tab = uuid::Uuid::new_v4().as_hyphenated().to_string();
    let leaf = uuid::Uuid::new_v4().as_hyphenated().to_string();
    match PaneKey::new(&tab, &leaf) {
        Ok(pane) => pane,
        // Unreachable: a hyphenated uuid carries no separator, no whitespace and no control
        // character, which is the whole of what `PaneKey::new` checks.
        Err(_) => unreachable!("a hyphenated uuid is a well-formed pane key half"),
    }
}

/// Start the thread that drains a session's output into its grid and its streams.
fn spawn_pump(session: &Arc<OwnedSession>, output: PtyOutput) -> std::io::Result<JoinHandle<()>> {
    let vt = Arc::clone(&session.vt);
    let pty = Arc::clone(&session.pty);
    let sinks = Arc::clone(&session.sinks);
    let last_output_ms = Arc::clone(&session.last_output_ms);
    let stop = Arc::clone(&session.stop);
    let handle = session.handle.as_str().to_owned();

    std::thread::Builder::new()
        .name("nysia-rpc-pump".to_owned())
        .spawn(move || {
            let mut pending: Vec<u8> = Vec::with_capacity(COALESCE_MIN);
            let mut oldest: Option<Instant> = None;
            let mut exit_sent = false;

            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                flush(&sinks, &mut pending, &mut oldest);

                // The backpressure, in one branch. Reading nothing is what makes the child
                // block: the queue behind `PtyOutput` fills, its reader thread blocks on the
                // send, and the kernel pty buffer fills. Absorbing the bytes here instead
                // would move the flood into the daemon's memory and call it flow control.
                if blocked(&sinks) || pending.len() >= FLUSH_CEILING {
                    std::thread::sleep(FLUSH_INTERVAL);
                    continue;
                }

                match output.recv_timeout(FLUSH_INTERVAL) {
                    Output::Chunk(chunk) => {
                        last_output_ms.store(now_ms(), Ordering::Release);
                        let mut state = lock(&vt);
                        state.feed(&chunk);
                        let replies = state.take_replies();
                        drop(state);
                        if !replies.is_empty() {
                            // Not optional. `pwsh` opens with `CSI 6 n` and will not draw its
                            // prompt until the cursor position comes back, so a pump that
                            // drops these leaves a pane that never paints.
                            if let Err(err) = pty.write(&replies) {
                                tracing::debug!(%handle, %err, "could not answer a device query");
                            }
                        }
                        if oldest.is_none() {
                            oldest = Some(Instant::now());
                        }
                        pending.extend_from_slice(&chunk);
                    }
                    Output::Timeout => {}
                    Output::Eof => {
                        // EOF is not exit, and exit is not EOF (trap 11). This is the end of
                        // the *bytes*; whether the child has gone is a separate question the
                        // exit slot answers.
                        flush_all(&sinks, &mut pending, &mut oldest);
                        break;
                    }
                }

                if !exit_sent && let Some(status) = pty.exit_status() {
                    flush_all(&sinks, &mut pending, &mut oldest);
                    let status = exit_status(&status);
                    for sink in lock(&sinks).iter() {
                        send_exit(sink, sink.stream_id(), &status);
                    }
                    exit_sent = true;
                }
            }
            flush_all(&sinks, &mut pending, &mut oldest);
        })
}

/// Whether every attached stream is out of credit.
///
/// No streams means not blocked. That matters more than it looks: the CLI attaches nothing at
/// all, so a pump that waited for credit unconditionally would never feed the grid, and
/// `terminal read` would return an empty screen forever.
fn blocked(sinks: &Mutex<Vec<Arc<StreamSink>>>) -> bool {
    let mut sinks = lock(sinks);
    sinks.retain(|sink| !sink.is_closed());
    !sinks.is_empty() && sinks.iter().all(|sink| sink.is_blocked())
}

/// Flush the coalesced bytes if a kilobyte has built up or sixteen milliseconds have passed.
fn flush(sinks: &Mutex<Vec<Arc<StreamSink>>>, pending: &mut Vec<u8>, oldest: &mut Option<Instant>) {
    if pending.is_empty() {
        return;
    }
    let due = pending.len() >= COALESCE_MIN
        || oldest.is_some_and(|since| since.elapsed() >= FLUSH_INTERVAL);
    if due {
        flush_all(sinks, pending, oldest);
    }
}

/// Write the coalesced bytes to every sink that will take them, and drop what all of them did.
///
/// A sink that will not take them keeps the bytes pending: this is where "stop reading"
/// becomes true, because the caller loops back and finds `pending` non-empty and the sink
/// still blocked.
///
/// Each sink resumes from [`StreamSink::delivered`] rather than from the head of the buffer,
/// and only the bytes the *slowest* sink has taken are dropped. Re-offering the buffer from
/// the start instead — which is what "did everybody take all of it?" amounts to — re-sends
/// what has already been sent, and under sustained backpressure never converges: the buffer
/// grows to [`FLUSH_CEILING`] while each turn delivers only an allowance's worth of its head,
/// so the terminal both duplicates and stops advancing. That is precisely the state a `yes`
/// flood reaches once the opening window is spent.
fn flush_all(
    sinks: &Mutex<Vec<Arc<StreamSink>>>,
    pending: &mut Vec<u8>,
    oldest: &mut Option<Instant>,
) {
    if pending.is_empty() {
        return;
    }
    let mut sinks = lock(sinks);
    sinks.retain(|sink| !sink.is_closed());
    if sinks.is_empty() {
        // Nobody is watching. The grid already has the bytes — that is what makes a
        // headless session readable — so the transient buffer is dropped rather than grown.
        pending.clear();
        *oldest = None;
        return;
    }
    let mut common = pending.len();
    for sink in sinks.iter() {
        let mut done = sink.delivered().min(pending.len());
        let chunk = sink.window().chunk as usize;
        while done < pending.len() {
            let end = pending.len().min(done + chunk);
            match sink.send(&Frame::new(
                FrameKind::Output,
                sink.stream_id(),
                pending[done..end].to_vec(),
            )) {
                SendOutcome::Sent => done = end,
                SendOutcome::WouldBlock => break,
                // A sink that has gone is retained out on the next turn. Counting it as
                // finished keeps it from holding the buffer for everybody else in between.
                SendOutcome::Closed => {
                    done = pending.len();
                    break;
                }
            }
        }
        sink.set_delivered(done);
        common = common.min(done);
    }
    if common > 0 {
        pending.drain(..common);
        for sink in sinks.iter() {
            sink.set_delivered(sink.delivered().saturating_sub(common));
        }
    }
    if pending.is_empty() {
        *oldest = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rpc::testing::{DEADLINE, TOKEN, TestShell};

    #[test]
    fn a_replay_is_chunked_to_the_window_rather_than_to_the_flush_ceiling() {
        // A frame larger than the allowance is charged in one go and drives the credit
        // negative, after which the stream sends nothing until the client has acked its way
        // back above zero. On a re-attach under a tight window that is a pane that sits blank
        // with nothing to explain it.
        let window = nysia_proto::CreditWindow {
            per_stream_initial: 2 * 1024,
            per_stream_max: 4 * 1024,
            total_initial: 2 * 1024,
            total_max: 4 * 1024,
            pending_cap: 2 * 1024,
            ack_batch: 512,
            chunk: 256,
        };
        assert!(window.is_coherent());
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        await_prompt(&session);
        // Enough scrollback that the replay cannot fit in one chunk, which is the only case
        // this test is about. A fresh prompt is shorter than 256 bytes and would pass either
        // way — the shape of a test that proves nothing.
        session
            .send(&TerminalSend::line(
                created.handle.clone(),
                TestShell::pick().flood,
            ))
            .expect("writes");
        // Wait for the screen to actually fill. `Idle` alone is satisfied by the quiet
        // *before* the shell starts producing, which is how this came to measure a replay of
        // 194 bytes and pass whatever the chunking did.
        until(&session, |text| text.len() > 2000);
        let _ = session.wait(WaitFor::Idle, Some(DEADLINE));
        let replay_len = lock(&session.vt).replay().len();
        assert!(
            replay_len > window.chunk as usize,
            "the replay is {replay_len} bytes against a {} byte chunk; it has to be longer              than one chunk or this test proves nothing",
            window.chunk
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
        let sink = Arc::new(StreamSink::new(nysia_proto::StreamId::FIRST, tx, window));
        session.attach(&sink);

        let mut frames = 0;
        while let Ok(frame) = rx.try_recv() {
            let (frame, _) = nysia_proto::decode(&frame)
                .expect("decodes")
                .expect("a whole frame");
            if frame.kind != FrameKind::Output {
                continue;
            }
            frames += 1;
            assert!(
                frame.payload.len() <= window.chunk as usize,
                "a replay frame of {} bytes against a {}-byte chunk",
                frame.payload.len(),
                window.chunk
            );
        }
        assert!(frames > 0, "the session had scrollback to replay");
        assert!(
            sink.credit() >= 0,
            "a replay must stop at the allowance rather than overshoot it: {} left",
            sink.credit()
        );
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_replay_the_allowance_cut_short_still_ends_with_its_boundary() {
        // The marker is what a client waits on before it will accept input, so a replay that
        // ran out of allowance and skipped it would leave the pane refusing keystrokes until
        // the client's own deadline rescued it — and it is the sessions with the *most*
        // scrollback, the ones most likely to outrun the opening window, that would get it.
        // This is the case the `break` in `attach` creates, made deliberate: an allowance
        // small enough that the replay cannot possibly finish.
        let window = nysia_proto::CreditWindow {
            per_stream_initial: 512,
            per_stream_max: 512,
            total_initial: 512,
            total_max: 512,
            pending_cap: 1024,
            ack_batch: 128,
            chunk: 128,
        };
        assert!(window.is_coherent());
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        await_prompt(&session);
        session
            .send(&TerminalSend::line(
                created.handle.clone(),
                TestShell::pick().flood,
            ))
            .expect("writes");
        until(&session, |text| text.len() > 4000);
        let _ = session.wait(WaitFor::Idle, Some(DEADLINE));
        let replay_len = lock(&session.vt).replay().len();
        assert!(
            replay_len > window.per_stream_initial as usize,
            "the replay is {replay_len} bytes against a {}-byte allowance; it has to outrun \
             the allowance or this test exercises the ordinary path instead of the cut-short \
             one",
            window.per_stream_initial
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
        let sink = Arc::new(StreamSink::new(nysia_proto::StreamId::FIRST, tx, window));
        session.attach(&sink);

        let mut kinds = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            let (frame, _) = nysia_proto::decode(&frame)
                .expect("decodes")
                .expect("a whole frame");
            kinds.push(frame.kind);
        }
        let replayed = kinds.iter().filter(|k| **k == FrameKind::Output).count();
        assert!(replayed > 0, "the session had scrollback to replay");
        assert_eq!(
            kinds.iter().filter(|k| **k == FrameKind::ReplayEnd).count(),
            1,
            "a truncated replay still owes exactly one boundary; the attach wrote {kinds:?}"
        );
        // And it is last. A marker that arrived mid-replay would open the client's gate while
        // replayed queries were still on the way, which is the whole defect over again.
        assert_eq!(
            kinds.last(),
            Some(&FrameKind::ReplayEnd),
            "the boundary must follow the last replayed byte; the attach wrote {kinds:?}"
        );
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_sink_that_stops_half_way_resumes_rather_than_being_offered_the_buffer_again() {
        // Sustained backpressure is the case where the coalesced buffer is bigger than the
        // allowance, so every flush stops part way through it. Offering it from the head
        // again re-sends what has already gone and never converges: the buffer grows to the
        // flush ceiling while each turn moves only an allowance's worth of its head, and the
        // pane both duplicates and stops advancing. Reaching that state takes nothing more
        // exotic than `yes` once the opening window is spent.
        let window = nysia_proto::CreditWindow {
            per_stream_initial: 256,
            per_stream_max: 256,
            total_initial: 256,
            total_max: 256,
            pending_cap: 1024,
            ack_batch: 256,
            chunk: 64,
        };
        assert!(window.is_coherent());
        let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
        let sink = Arc::new(StreamSink::new(nysia_proto::StreamId::FIRST, tx, window));
        let sinks = Mutex::new(vec![Arc::clone(&sink)]);

        // A pattern rather than a repeated byte, so an out-of-order or duplicated stretch
        // cannot pass for the real thing.
        let source: Vec<u8> = (0..4096u32).map(|byte| (byte % 251) as u8).collect();
        let mut pending = source.clone();
        let mut oldest = Some(Instant::now());
        let mut turns = 0;
        while !pending.is_empty() && turns < 200 {
            flush_all(&sinks, &mut pending, &mut oldest);
            // The client rendering what it was sent and acking it, which is the only thing
            // that makes the next turn possible.
            sink.replenish(window.per_stream_max);
            turns += 1;
        }
        assert!(
            pending.is_empty(),
            "the buffer never drained: {} bytes left after {turns} turns",
            pending.len()
        );
        assert!(oldest.is_none(), "a drained buffer has no age");

        let mut delivered = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            let (frame, _) = nysia_proto::decode(&frame)
                .expect("decodes")
                .expect("a whole frame");
            assert_eq!(frame.kind, FrameKind::Output);
            delivered.extend_from_slice(&frame.payload);
        }
        assert_eq!(
            delivered, source,
            "every byte exactly once and in order; a re-offered buffer shows up here as a \
             repeated stretch"
        );
    }

    fn create(registry: &SessionRegistry, pane: Option<PaneKey>) -> SessionCreated {
        registry
            .create(&SessionCreate {
                kind: SessionKind::Shell,
                pane_key: pane,
                profile: TestShell::pick().profile,
                cwd: None,
                env_overrides: BTreeMap::new(),
                cols: 80,
                rows: 24,
            })
            .expect("a shell session starts")
    }

    /// Wait until the shell has drawn its prompt and gone quiet.
    ///
    /// Typing before this reliably loses rather than occasionally: a shell that has not
    /// finished starting echoes what is typed and then redraws the line when its line editor
    /// takes over, so the command appears twice and runs zero times. An empty screen means it
    /// has written nothing yet; quiet alone would be satisfied by the silence before it starts.
    fn await_prompt(session: &OwnedSession) {
        until(session, |text| !text.trim().is_empty());
        let _ = session.wait(WaitFor::Idle, Some(DEADLINE));
    }

    /// Wait until the session's screen satisfies `predicate`, or give up.
    fn until(session: &OwnedSession, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let read = session.read(ReadMode::Screen, None, None);
            let text = read.lines.join("\n");
            if predicate(&text) {
                return text;
            }
            if Instant::now() >= deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn a_session_paints_its_prompt_which_proves_the_pump_answers_device_queries() {
        // If the pump did not write `take_replies` back to the pty, `pwsh` would sit waiting
        // for its cursor position and this screen would stay empty forever.
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        let screen = until(&session, |text| !text.trim().is_empty());
        assert!(
            !screen.trim().is_empty(),
            "a shell should paint something; the pump may not be answering device queries"
        );
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn what_the_shell_computed_reaches_both_the_screen_and_the_scrollback() {
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        await_prompt(&session);

        for line in TestShell::pick().lines {
            session
                .send(&TerminalSend::line(created.handle.clone(), line))
                .expect("writes");
            // Settle between lines rather than wait for the token: only the *last* line
            // produces it, so waiting for it after the first would burn the whole budget on a
            // condition that cannot be true yet. `cmd` also needs the assignment to have run
            // before it parses the line that expands it.
            let _ = session.wait(WaitFor::Idle, Some(DEADLINE));
        }

        let screen = until(&session, |text| text.contains(TOKEN));
        assert!(screen.contains(TOKEN), "got {screen:?}");

        let stream = session.read(ReadMode::Stream, Some(LineCursor::START), None);
        assert_eq!(stream.mode, ReadMode::Stream);
        assert!(
            stream.lines.iter().any(|line| line.contains(TOKEN)),
            "the scrollback should hold it too, got {:?}",
            stream.lines
        );
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_read_defaults_to_the_screen_and_says_which_mode_it_ran() {
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        let read = session.read(ReadMode::Screen, None, None);
        assert_eq!(read.mode, ReadMode::Screen);
        assert!(!read.truncated, "the screen is never a truncated answer");
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_pane_that_already_has_a_session_is_refused_rather_than_rebound() {
        let registry = SessionRegistry::new();
        let pane = PaneKey::new("tab_1", "leaf_1").expect("a well-formed pane key");
        let first = create(&registry, Some(pane.clone()));

        let second = registry.create(&SessionCreate {
            kind: SessionKind::Shell,
            pane_key: Some(pane.clone()),
            profile: TestShell::pick().profile,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        });
        assert!(matches!(second, Err(SessionError::PaneTaken { .. })));

        // And once it is closed the pane is free again, with a *new* incarnation — a relaunch
        // in one pane must not be indistinguishable from the first spawn.
        registry.close(&first.handle).expect("closes");
        let third = create(&registry, Some(pane.clone()));
        assert_eq!(third.pane_key, pane);
        assert_ne!(third.incarnation, first.incarnation);
        assert_eq!(first.incarnation.generation(), 0);
        assert_eq!(third.incarnation.generation(), 1);
        registry.close(&third.handle).expect("closes");
    }

    #[test]
    fn a_closed_session_is_gone_from_the_list_and_its_handle_stops_resolving() {
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        assert_eq!(registry.len(), 1);
        assert!(
            registry
                .list()
                .iter()
                .any(|row| row.handle == created.handle)
        );

        registry.close(&created.handle).expect("closes");
        assert!(registry.is_empty());
        assert!(matches!(
            registry.get(&created.handle),
            Err(SessionError::Unknown { .. })
        ));
        assert!(matches!(
            registry.close(&created.handle),
            Err(SessionError::Unknown { .. })
        ));
    }

    #[test]
    fn waiting_for_exit_returns_the_status_the_child_actually_had() {
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        await_prompt(&session);

        session
            .send(&TerminalSend::line(created.handle.clone(), "exit 7"))
            .expect("writes");
        match session.wait(WaitFor::Exit, Some(DEADLINE)) {
            WaitOutcome::Exited {
                status: ExitStatus::Exited { code },
            } => assert_eq!(code, 7),
            other => panic!("expected a clean exit with code 7, got {other:?}"),
        }
        // And the summary follows the child's `wait()`, not the pty's EOF (trap 11).
        assert!(session.summary().exit_status.is_some());
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_bounded_wait_that_runs_out_says_so_rather_than_failing() {
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        assert_eq!(
            session.wait(WaitFor::Exit, Some(Duration::from_millis(150))),
            WaitOutcome::TimedOut
        );
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_quiet_session_is_idle() {
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        until(&session, |text| !text.trim().is_empty());
        assert_eq!(
            session.wait(WaitFor::Idle, Some(DEADLINE)),
            WaitOutcome::Idle
        );
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_working_directory_that_is_not_one_is_refused_with_next_steps() {
        let registry = SessionRegistry::new();
        for cwd in [
            PathBuf::from("relative/path"),
            PathBuf::from("/no/such/place/at/all"),
        ] {
            let refused = registry.create(&SessionCreate {
                kind: SessionKind::Shell,
                pane_key: None,
                profile: None,
                cwd: Some(WorkingDirectory::Path { path: cwd.clone() }),
                env_overrides: BTreeMap::new(),
                cols: 80,
                rows: 24,
            });
            let Err(error) = refused else {
                panic!("{} should have been refused", cwd.display());
            };
            let envelope = error.into_envelope();
            assert_eq!(*envelope.code(), ErrorCode::PathRefused);
            assert!(!envelope.next_steps().is_empty());
        }
    }

    #[test]
    #[cfg(windows)]
    fn a_command_prompt_given_a_folder_runs_in_it_rather_than_in_the_windows_directory() {
        // `cmd.exe` cannot run in a verbatim `\\?\` path. Handed one, it says "UNC paths are
        // not supported", opens in `C:\Windows` and carries on — so a Command Prompt started
        // in a project ran somewhere else entirely. `pwsh` accepts the prefix, and CI has
        // `pwsh` on both legs, so every other test that sets a working directory drives the
        // shell that could not see this; this one names `cmd` outright.
        //
        // Asserted from inside the shell: the file is printed by a name relative to its
        // working directory, and only the requested folder holds it.
        let dir = std::env::temp_dir().join(format!("nysia-cmd-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder to start in");
        std::fs::write(dir.join("nysia-cwd-probe.txt"), "PROBE-CMD-SEEN").expect("the probe");

        let registry = SessionRegistry::new();
        let created = registry
            .create(&SessionCreate {
                kind: SessionKind::Shell,
                pane_key: None,
                profile: Some(WireProfile::Cmd),
                cwd: Some(WorkingDirectory::Path { path: dir.clone() }),
                env_overrides: BTreeMap::new(),
                cols: 200,
                rows: 24,
            })
            .expect("a command prompt starts");
        let session = registry.get(&created.handle).expect("the session is there");
        await_prompt(&session);
        session
            .send(&TerminalSend::line(
                created.handle.clone(),
                "type nysia-cwd-probe.txt",
            ))
            .expect("writes");
        let screen = until(&session, |text| text.contains("PROBE-CMD-SEEN"));
        assert!(
            screen.contains("PROBE-CMD-SEEN"),
            "the command prompt could not print a file from the folder it was started in: \
             {screen}"
        );
        registry.close(&created.handle).expect("closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_program_run_through_cmd_is_refused_a_network_folder() {
        // The rule by its two halves, on both platforms: which programs `cmd.exe` runs, and
        // which folders are network ones. An agent installed by npm resolves to a batch shim
        // that `cmd.exe` runs on its behalf, so it is in the first set whatever it is called.
        let through_cmd = [
            SessionProgram::Shell(ShellProfile::CommandPrompt),
            SessionProgram::Resolved {
                argv: vec![
                    std::ffi::OsString::from(r"C:\WINDOWS\System32\CMD.EXE"),
                    std::ffi::OsString::from("/d"),
                ],
                label: "an agent".to_owned(),
            },
        ];
        let not_through_cmd = [
            SessionProgram::Shell(ShellProfile::PowerShell7),
            SessionProgram::Shell(ShellProfile::GitBash),
            SessionProgram::Resolved {
                argv: vec![std::ffi::OsString::from(
                    r"C:\Users\me\.local\bin\an-agent.exe",
                )],
                label: "an agent".to_owned(),
            },
        ];
        for program in &through_cmd {
            assert!(
                runs_through_cmd(program),
                "{program:?} runs through cmd.exe"
            );
        }
        for program in &not_through_cmd {
            assert!(!runs_through_cmd(program), "{program:?} does not");
        }

        for network in [
            r"\\server\share\repo",
            r"\\localhost\C$\Users\me\repo",
            r"\\?\UNC\server\share\repo",
            r"\\?\unc\server\share\repo",
        ] {
            assert!(
                is_network_folder(std::path::Path::new(network)),
                "{network} is a network folder"
            );
        }
        for local in [
            r"C:\src\repo",
            r"\\?\C:\src\a-folder-too-long-to-lose-its-prefix",
            r"\\.\pipe\nysiad",
            "/home/me/repo",
        ] {
            assert!(
                !is_network_folder(std::path::Path::new(local)),
                "{local} is not a network folder"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn a_command_prompt_asked_for_a_network_folder_is_refused_rather_than_opened_elsewhere() {
        // `cmd.exe` refuses every UNC working directory: it says so in its own banner and
        // runs in `C:\Windows`. A drive letter mapped to a share arrives as the share's path,
        // so that is the same case. Staged through this machine's own administrative share,
        // which reaches a local folder by its network path.
        let dir = std::env::temp_dir().join(format!("nysia-cmd-unc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder to share");
        let local = CanonicalPath::of(&dir).expect("the folder resolves");
        let text = local.as_path().to_string_lossy().into_owned();
        let (drive, tail) = text.split_once(r":\").expect("a drive-letter folder");
        let network = PathBuf::from(format!(r"\\localhost\{drive}$\{tail}"));
        assert!(
            network.is_dir(),
            "the administrative share does not reach {network:?}, so this cannot stage a \
             network folder at all"
        );

        let registry = SessionRegistry::new();
        let refused = registry.create(&SessionCreate {
            kind: SessionKind::Shell,
            pane_key: None,
            profile: Some(WireProfile::Cmd),
            cwd: Some(WorkingDirectory::Path { path: network }),
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        });
        let err = match refused {
            Err(err @ SessionError::CmdOnNetworkFolder) => err,
            other => panic!(
                "a Command Prompt asked for a network folder was {}: it would have opened in \
                 C:\\Windows",
                other.map_or_else(
                    |err| format!("refused otherwise ({err})"),
                    |_| { "started".to_owned() }
                )
            ),
        };
        assert!(registry.list().is_empty(), "something was spawned anyway");
        let envelope = err.into_envelope();
        assert!(envelope.message().contains("network folder"));
        assert!(envelope.message().contains("Command Prompt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn a_folder_too_long_to_start_in_says_so_rather_than_blaming_the_shell() {
        // A folder of 259 characters or more is one Windows will not start a program in, and
        // the refusal used to say "check the shell is installed and on PATH" — which sends a
        // person to their shell when the cause is the path. 258 starts; 259 and 260 do not,
        // and 260 is also the length at which the folder keeps its `\\?\` prefix.
        let base = std::env::temp_dir().join(format!("nysia-long-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("a base folder");
        let base = CanonicalPath::of(&base)
            .expect("the base resolves")
            .as_path()
            .to_path_buf();
        let at_length = |length: usize| {
            let folder = base.join("a".repeat(length - windows_length(&base) - 1));
            std::fs::create_dir_all(&folder).expect("a folder of that length");
            assert_eq!(windows_length(&folder), length);
            folder
        };
        let create = |registry: &SessionRegistry, folder: PathBuf| {
            registry.create(&SessionCreate {
                kind: SessionKind::Shell,
                pane_key: None,
                profile: Some(WireProfile::Cmd),
                cwd: Some(WorkingDirectory::Path { path: folder }),
                env_overrides: BTreeMap::new(),
                cols: 80,
                rows: 24,
            })
        };

        let registry = SessionRegistry::new();
        let started = create(&registry, at_length(MAX_WORKING_DIRECTORY))
            .expect("Windows starts a program in a folder of exactly the longest length");
        registry.close(&started.handle).expect("closes");

        for length in [MAX_WORKING_DIRECTORY + 1, MAX_WORKING_DIRECTORY + 2] {
            let err = match create(&registry, at_length(length)) {
                Err(err @ SessionError::FolderTooLong { .. }) => err,
                Err(other) => panic!("a {length}-character folder was refused as {other:?}"),
                Ok(_) => panic!("a {length}-character folder started a session"),
            };
            let envelope = err.into_envelope();
            let said = format!("{} {:?}", envelope.message(), envelope.next_steps());
            assert!(
                said.contains(&format!("{length} characters")),
                "the refusal does not say how long the path is: {said}"
            );
            for blamed in ["installed", "PATH", "shim"] {
                assert!(
                    !said.contains(blamed),
                    "the refusal still blames the shell ({blamed}): {said}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Printed by the child before its answer, so the outer test can find the line.
    const PROFILES_LINE: &str = "NYSIA-PROFILES=";

    /// Printed by a child that got all the way through, so the outer test can tell "passed"
    /// from "was filtered out and never ran".
    const PROFILES_CHILD_OK: &str = "NYSIA-PROFILES-CHILD-OK";

    /// Ask a fresh copy of this test binary which shells it can launch, with `dir` as the
    /// **whole** of its `PATH`.
    ///
    /// By re-exec rather than `set_var`, for `agent::claude::launch`'s reason: resolution
    /// reads `PATH` on every call, the pty tests next door spawn shells on other threads, and
    /// changing the environment under them is a data race. The whole of `PATH` rather than a
    /// prefix, so a `pwsh` the machine really has cannot stand in for the one this test put
    /// there — or be missed when this test put none.
    fn availability_on(dir: &std::path::Path) -> Vec<ProfileAvailability> {
        let path = module_path!();
        let without_crate = path.split_once("::").map_or(path, |(_, rest)| rest);
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                &format!("{without_crate}::child_reports_which_shells_it_can_launch"),
                "--ignored",
                "--nocapture",
            ])
            .env(
                "PATH",
                std::env::join_paths([dir]).expect("a PATH with no separator in it"),
            )
            .output()
            .expect("the child test binary runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success() && text.contains(PROFILES_CHILD_OK),
            "the child did not run to the end:\n{text}"
        );
        let answer = text
            .lines()
            .find_map(|line| line.strip_prefix(PROFILES_LINE))
            .unwrap_or_else(|| panic!("the child printed no answer:\n{text}"));
        serde_json::from_str(answer).expect("the child's answer parses")
    }

    #[test]
    #[ignore = "driven by its outer test, which owns PATH"]
    fn child_reports_which_shells_it_can_launch() {
        println!(
            "{PROFILES_LINE}{}",
            serde_json::to_string(&profile_availability()).expect("serialises")
        );
        println!("{PROFILES_CHILD_OK}");
    }

    #[test]
    fn which_shells_are_offered_is_what_resolution_finds_on_path_not_a_platform_list() {
        // The `+` menu used to list every profile the wire can spell, and a person learned
        // that PowerShell 7 was not installed by picking it. The answer has to come from the
        // same resolution a spawn runs, on the daemon's own `PATH` — so this asks it on three
        // `PATH`s it controls: one holding a `pwsh`, one holding nothing, and one holding a
        // `pwsh` that resolution finds and rejects.
        //
        // A `cfg!(windows)` list fails on each leg: it offers `pwsh` on Windows where the
        // empty `PATH` has none, and withholds it on macOS where the fixture put one.
        // A directory of its own rather than `git::testing::Scratch`, which needs a `git` to
        // build fixtures with: nothing here is a repository, and a machine without git still
        // has shells to list.
        let root = std::env::temp_dir().join(format!("nysia-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir_all(&path).expect("a folder for a PATH");
            path
        };
        let with_pwsh = folder("with-pwsh");
        let empty = folder("empty");
        // A distinctive component, so the leak check below cannot pass merely because the
        // folder's name was short or ordinary.
        let broken = folder("a-clients-private-toolchain");
        if cfg!(windows) {
            // A batch shim, which resolution launches through `cmd.exe` from `System32` —
            // found by `SystemRoot`, not by `PATH`, so it resolves with nothing else on it.
            std::fs::write(with_pwsh.join("pwsh.cmd"), "@echo off\r\n").expect("a pwsh");
        } else {
            std::fs::write(with_pwsh.join("pwsh"), "#!/bin/sh\n").expect("a pwsh");
        }
        // Found by name and refused by validation: no launchable extension on Windows, no
        // execute bit on Unix. Either refusal names the file it rejected, which is what
        // makes this the case the leak check below can actually trip on.
        std::fs::write(broken.join("pwsh"), "not a program\n").expect("a broken pwsh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                with_pwsh.join("pwsh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("the pwsh is executable");
            std::fs::set_permissions(broken.join("pwsh"), std::fs::Permissions::from_mode(0o644))
                .expect("the broken pwsh is not");
        }

        let pwsh = |rows: &[ProfileAvailability]| {
            rows.iter()
                .find(|row| row.profile == WireProfile::Pwsh)
                .cloned()
                .expect("every menu profile is a row, launchable or not")
        };

        let offered = availability_on(&with_pwsh);
        assert_eq!(
            offered.iter().map(|row| &row.profile).collect::<Vec<_>>(),
            MENU_PROFILES.iter().collect::<Vec<_>>(),
            "one row per menu entry, in the menu's order"
        );
        assert_eq!(
            pwsh(&offered).unavailable,
            None,
            "a pwsh on PATH is a pwsh the daemon can launch: {offered:?}"
        );

        let absent = availability_on(&empty);
        let reason = pwsh(&absent)
            .unavailable
            .expect("no pwsh on PATH is a pwsh the daemon cannot launch");
        assert!(
            reason.contains("pwsh") && reason.contains("not found"),
            "the reason says which shell and what was wrong with it: {reason:?}"
        );

        let rejected = availability_on(&broken);
        assert!(
            pwsh(&rejected).unavailable.is_some(),
            "a pwsh resolution finds and cannot launch is not offered: {rejected:?}"
        );

        // The reason reaches a menu a person screenshots. It names the shell, never a file —
        // and the rejected `pwsh` is the row whose error, rendered whole, would name one.
        for row in offered.iter().chain(&absent).chain(&rejected) {
            let said = row.unavailable.as_deref().unwrap_or_default();
            for folder in [&with_pwsh, &empty, &broken] {
                assert!(
                    !said.contains(folder.to_string_lossy().as_ref()),
                    "{said:?} names a folder"
                );
            }
            assert!(
                !said.contains("a-clients-private-toolchain"),
                "{said:?} names a folder"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A request for an agent session, with nothing else asked for.
    fn agent_request() -> SessionCreate {
        SessionCreate {
            kind: SessionKind::Agent,
            pane_key: None,
            profile: None,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn an_agent_request_is_a_program_to_resolve_rather_than_a_refusal() {
        // The whole of the gap this closes, stated as the two answers that are allowed. One
        // of them depends on whether a CLI is installed, so neither is asserted on its own —
        // what is asserted is that **nothing else** may come back, which is what a blanket
        // refusal was.
        //
        // Both legs of CI take the `Launch` arm, because neither runner has the CLI. The
        // other arm is proved on both legs by
        // `agent::claude::launch::tests::an_agent_session_reaches_an_interactive_prompt`,
        // which writes its own CLI onto a controlled `PATH` rather than waiting for one.
        match program_for(&agent_request()) {
            Ok(spec) => {
                assert!(
                    spec.program.shell().is_none(),
                    "an agent session is not one of the shells"
                );
                let label = spec.program.label();
                assert!(!label.is_empty(), "a tab needs something to be called");
                assert!(
                    !label.contains('/') && !label.contains('\\'),
                    "a tab is labelled with a name, never a path: {label:?}"
                );
            }
            Err(SessionError::Launch(_)) => {}
            Err(other) => panic!(
                "an agent request is served or says the CLI is missing, and nothing else: \
                 {other}"
            ),
        }
    }

    #[test]
    fn a_missing_agent_cli_is_actionable_rather_than_a_blank_refusal() {
        // Constructed rather than provoked, so this runs the same way on a machine with the
        // CLI and on one without — and it is the arm both CI legs take for real.
        let error = SessionError::Launch(crate::agent::LaunchError::Unavailable {
            agent: "an-agent",
            source: crate::pty::ResolveError::NotFound {
                program: "an-agent".to_owned(),
            },
        });
        let envelope = error.into_envelope();

        assert_eq!(
            *envelope.code(),
            ErrorCode::SpawnFailed,
            "a CLI that is not installed is a spawn that will not happen, not a verb this \
             build does not serve"
        );
        assert!(
            envelope.message().contains("an-agent"),
            "the refusal names which agent: {:?}",
            envelope.message()
        );
        assert!(
            !envelope.next_steps().is_empty(),
            "§6.2 makes a next step non-optional"
        );
    }

    /// No refusal this module answers with carries a path into the envelope.
    ///
    /// #96's finding, and the rule `rpc/project.rs` has carried since #94 as
    /// `a_refusal_does_not_repeat_the_path_it_was_given`. Two envelopes here did it with no
    /// attacker anywhere: a `claude` on `PATH` with no launchable extension answered
    ///
    /// ```text
    /// could not start the agent: the claude CLI is unavailable:
    /// C:\Users\<name>\AppData\Local\Temp\…\badbin\claude has no launchable extension
    /// ```
    ///
    /// and the spawn half was worse, because `portable-pty` formats the whole command line
    /// *and the working directory* into the error it answers a failed `CreateProcessW` with.
    ///
    /// # What is checked, and the two things that are not
    ///
    /// Every value reaching an envelope from here is a phrase chosen in this file or an
    /// identifier the daemon minted, with two exceptions, both named rather than left to be
    /// discovered:
    ///
    /// - [`SessionError::Invalid`] carries a sentence written at its call sites, every one
    ///   of which is a literal in this file with no path to write.
    /// - [`SessionError::PathRefused`] carries `reason`, which is a sentence written in
    ///   `confine_cwd` or the `io::Error` behind a [`PathError::Unreadable`] — which the OS
    ///   reports without the path it was given.
    ///
    /// The roster puts [`SECRET`] into every **other** position: every field of every error
    /// this module wraps, and the path `PathRefused` was handed. The roster itself is by
    /// hand. What is not by hand is noticing when it goes stale — [`variant_of`] matches
    /// exhaustively, so a variant added to [`SessionError`] stops this file compiling until
    /// somebody stands where the roster is. That is a tripwire and not a proof, and it is
    /// worth exactly what it says: the compiler makes them look.
    #[test]
    fn no_refusal_this_module_answers_with_carries_a_path() {
        let resolve_failures = [
            ResolveError::NotFound {
                program: SECRET.to_owned(),
            },
            ResolveError::NotAFile {
                path: SECRET.to_owned(),
            },
            ResolveError::NotExecutable {
                path: SECRET.to_owned(),
            },
            ResolveError::UnknownExtension {
                path: SECRET.to_owned(),
            },
            ResolveError::MissingInterpreter {
                path: SECRET.to_owned(),
                interpreter: SECRET.to_owned(),
            },
            ResolveError::UnsafeArgument {
                argument: SECRET.to_owned(),
            },
        ];

        let mut roster = vec![
            SessionError::Unknown {
                handle: SessionHandle::generate(),
            },
            SessionError::PaneTaken {
                pane: synthetic_pane(),
                handle: SessionHandle::generate(),
            },
            SessionError::Spawn(SpawnError::Spawn {
                program: SECRET.to_owned(),
                // Shaped like the real one, which is the point: this is what the envelope
                // used to repeat.
                reason: format!("CreateProcessW `{SECRET}` in cwd `{SECRET}` failed: …"),
            }),
            SessionError::Spawn(SpawnError::OpenPty(SECRET.to_owned())),
            SessionError::Spawn(SpawnError::Writer(SECRET.to_owned())),
            SessionError::Spawn(SpawnError::Confinement(std::io::Error::other(SECRET))),
            SessionError::Spawn(SpawnError::Thread(std::io::Error::other(SECRET))),
            SessionError::Spawn(SpawnError::Profile(ProfileError::WrongPlatform {
                profile: "pwsh",
            })),
            SessionError::Spawn(SpawnError::Profile(ProfileError::BadDistro(
                SECRET.to_owned(),
            ))),
            SessionError::PathRefused {
                path: PathBuf::from(SECRET),
                reason: "it is not a directory".to_owned(),
            },
            SessionError::CmdOnNetworkFolder,
            SessionError::FolderTooLong {
                length: 263,
                source: SpawnError::Spawn {
                    program: SECRET.to_owned(),
                    reason: format!("CreateProcessW `{SECRET}` in cwd `{SECRET}` failed: …"),
                },
            },
            SessionError::Invalid("a session is at least one cell in each direction".to_owned()),
            SessionError::Closing(std::io::Error::other(SECRET)),
        ];
        // Both doors on to `ResolveError`, each through every variant of it: the agent's, and
        // the shell profile's — which is the third one the finding did not name.
        for source in resolve_failures {
            roster.push(SessionError::Launch(LaunchError::Unavailable {
                agent: "claude",
                source: source.clone(),
            }));
            roster.push(SessionError::Spawn(SpawnError::Profile(
                ProfileError::Unavailable {
                    profile: "pwsh",
                    source,
                },
            )));
        }

        let mut covered = std::collections::BTreeSet::new();
        for error in roster {
            covered.insert(variant_of(&error));
            let envelope = error.into_envelope();
            let said = format!("{} {:?}", envelope.message(), envelope.next_steps());
            for secret in [SECRET, SECRET_COMPONENT, &whoami()] {
                assert!(
                    !said.contains(secret),
                    "the refusal carried {secret:?}, which names somebody's disk and the \
                     account they run as: {said}"
                );
            }
            assert!(
                !envelope.next_steps().is_empty(),
                "and still says what to do about it: {said}"
            );
        }
        assert_eq!(
            covered.len(),
            EVERY_VARIANT.len(),
            "the roster is missing a variant: it covers {covered:?}"
        );
    }

    /// The daemon's own log does not repeat a value the daemon refused either.
    ///
    /// The other half of #96's review finding, and a **narrower** rule than the one above:
    /// an envelope names no file at all, while the log deliberately keeps the command line
    /// and the working directory of a spawn that failed. [`spawn_log_line`] is where that
    /// line is drawn and why it falls where it does; this holds the one value it drops.
    ///
    /// # What is checked, and the one thing that is not
    ///
    /// Both halves of that function, so neither can drift into the other: a refused WSL
    /// distribution name is gone, and a failed spawn's own text is still there. Answering
    /// the `BadDistro` arm with `err.to_string()` — the narrowing removed, leaving the
    /// whole-error log this replaced — reds the first assertion; answering the rest with
    /// [`spawn_kind`] reds the second. Both were run. Deleting the `BadDistro` arm outright
    /// does not red anything, because it does not compile: that is the tripwire, and it is
    /// the compiler's and not this test's.
    ///
    /// Not checked: the `tracing` call site, which is one line from the function and read by
    /// eye, so putting `%err` back *there* is a change this would go on passing through.
    /// [`no_refusal_this_module_answers_with_carries_a_path`] is the same — it calls
    /// `into_envelope`, not the wire.
    #[test]
    fn a_log_line_about_a_spawn_does_not_repeat_a_refused_distribution_name() {
        // Constructed rather than provoked, for the reason the test above gives: `Wsl` is
        // `WrongPlatform` before it is anything else off Windows, so provoking this would
        // run on one leg. The *name* is still one `check_distro` would genuinely refuse —
        // it carries whitespace — rather than one only this test calls refused. A distro
        // arrives as an arbitrary string in `ShellProfile::Wsl`, so a path-shaped one is a
        // thing a caller can send.
        let refused = format!("{SECRET} --and-another-argument");
        let line = spawn_log_line(&SpawnError::Profile(ProfileError::BadDistro(refused)));
        for secret in [SECRET, SECRET_COMPONENT, &whoami()] {
            assert!(
                !line.contains(secret),
                "`check_distro` threw this name out and the log wrote it down anyway: {line}"
            );
        }

        // The half that is deliberately kept, so that "drop the refused one" cannot quietly
        // become "drop everything" and take the daemon's only account of the failure with it.
        let kept = spawn_log_line(&SpawnError::Spawn {
            program: SECRET.to_owned(),
            reason: format!("CreateProcessW `{SECRET}` in cwd `{SECRET}` failed: …"),
        });
        assert!(
            kept.contains(SECRET),
            "a failed spawn is diagnosed from the command line and the directory it failed \
             in, and this is the only place either is written down: {kept}"
        );
    }

    /// A path with a component nothing else in this file could produce by accident.
    ///
    /// Absolute and profile-shaped, so a message that repeats only part of it — the account
    /// name, say — still fails on [`SECRET_COMPONENT`] or on [`whoami`].
    const SECRET: &str = r"C:\Users\nysia-test\an-agents-private-toolchain\claude";

    /// The distinctive component of [`SECRET`], so a message that shortens the path is still
    /// caught shortening it.
    const SECRET_COMPONENT: &str = "an-agents-private-toolchain";

    /// Every [`SessionError`] variant, by the name [`variant_of`] answers with.
    const EVERY_VARIANT: &[&str] = &[
        "Unknown",
        "PaneTaken",
        "Spawn",
        "Launch",
        "PathRefused",
        "CmdOnNetworkFolder",
        "FolderTooLong",
        "Invalid",
        "Closing",
    ];

    /// Which variant a refusal is, matched exhaustively.
    ///
    /// The tripwire the roster leans on: a variant added to [`SessionError`] has no arm here
    /// and this file stops compiling until it does.
    fn variant_of(error: &SessionError) -> &'static str {
        match error {
            SessionError::Unknown { .. } => "Unknown",
            SessionError::PaneTaken { .. } => "PaneTaken",
            SessionError::Spawn(_) => "Spawn",
            SessionError::Launch(_) => "Launch",
            SessionError::PathRefused { .. } => "PathRefused",
            SessionError::CmdOnNetworkFolder => "CmdOnNetworkFolder",
            SessionError::FolderTooLong { .. } => "FolderTooLong",
            SessionError::Invalid(_) => "Invalid",
            SessionError::Closing(_) => "Closing",
        }
    }

    /// The account this process runs as, as it appears inside a profile path.
    ///
    /// Asserted on as well as the path itself, because a message could name the account
    /// without naming the whole file — and that is the part that identifies a person.
    fn whoami() -> String {
        for var in ["USERNAME", "USER", "LOGNAME"] {
            if let Some(name) = std::env::var_os(var)
                && !name.is_empty()
            {
                return name.to_string_lossy().into_owned();
            }
        }
        // Nothing to compare against rather than something that matches everything.
        "\u{0}".to_owned()
    }

    #[test]
    fn a_degenerate_size_is_refused_rather_than_handed_to_conpty() {
        let registry = SessionRegistry::new();
        assert!(matches!(
            registry.create(&SessionCreate {
                kind: SessionKind::Shell,
                pane_key: None,
                profile: None,
                cwd: None,
                env_overrides: BTreeMap::new(),
                cols: 0,
                rows: 24,
            }),
            Err(SessionError::Invalid(_))
        ));

        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        assert!(matches!(
            session.resize(0, 0),
            Err(SessionError::Invalid(_))
        ));
        session.resize(100, 40).expect("a real size is taken");
        registry.close(&created.handle).expect("closes");
    }

    #[test]
    fn a_session_leader_is_offered_to_the_ancestry_walk() {
        // §3.2's whole claim rests on this map: the daemon spawned the leader, so it knows
        // the pid to walk a caller up to.
        let registry = SessionRegistry::new();
        let created = create(&registry, None);
        let session = registry.get(&created.handle).expect("the session is there");
        let leaders = registry.leaders();
        if let Some(pid) = session.leader_pid() {
            assert_eq!(
                leaders.get(&pid).map(|(handle, _)| handle),
                Some(&created.handle)
            );
        }
        registry.close(&created.handle).expect("closes");
    }
}
