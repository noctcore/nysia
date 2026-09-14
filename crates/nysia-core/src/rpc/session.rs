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
    ReadMode, SessionCreate, SessionCreated, SessionHandle, SessionKind, SessionSummary,
    ShellProfile as WireProfile, TerminalReadResult, TerminalSend, WaitFor, WaitOutcome,
};

use crate::pty::{DEFAULT_GRACE, Output, PtyOutput, PtySession, SessionSpec, ShellProfile};
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
    /// The requested working directory was refused.
    #[error("the working directory {} was refused: {reason}", path.display())]
    PathRefused {
        /// The path that was refused.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The request named something this build does not serve.
    #[error("{what} is not available in this build")]
    Unsupported {
        /// What was asked for.
        what: String,
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
        let message = self.to_string();
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
            Self::Spawn(_) => envelope(
                ErrorCode::SpawnFailed,
                message,
                "check the shell is installed and on PATH",
                &[
                    "on Windows an npm-installed shim is a `.cmd`, not an executable; name the \
                     real program",
                    "`nysia session create --profile pwsh` names a profile explicitly",
                ],
            ),
            Self::PathRefused { .. } => envelope(
                ErrorCode::PathRefused,
                message,
                "pass an absolute path to a directory that exists",
                &["omit --cwd to start the session where the daemon is running"],
            ),
            Self::Unsupported { .. } => envelope(
                ErrorCode::Unsupported,
                message,
                "v0.1 serves shell sessions; agent sessions land in v0.2",
                &["`nysia session create --profile pwsh` starts a shell"],
            ),
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

/// Confine a requested working directory.
///
/// v0.1's confinement is deliberately narrow: absolute, existing, and a directory. §7.5's
/// `safe_join` / `path_confine` — the worktree-relative rules — arrive with the worktree
/// module, and claiming them here before they exist would be worse than saying what this
/// actually checks.
fn confine_cwd(cwd: Option<&PathBuf>) -> Result<Option<PathBuf>, SessionError> {
    let Some(path) = cwd else {
        return Ok(None);
    };
    let refuse = |reason: &str| SessionError::PathRefused {
        path: path.clone(),
        reason: reason.to_owned(),
    };
    if !path.is_absolute() {
        return Err(refuse("a working directory must be absolute"));
    }
    let canonical = std::fs::canonicalize(path).map_err(|err| refuse(&err.to_string()))?;
    if !canonical.is_dir() {
        return Err(refuse("it is not a directory"));
    }
    Ok(Some(canonical))
}

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

    /// Spawn a session and start its pump.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the pane is taken, the working directory is refused, the
    /// request names something this build does not serve, or the shell will not start.
    pub fn create(&self, request: &SessionCreate) -> Result<SessionCreated, SessionError> {
        if request.kind == SessionKind::Agent {
            return Err(SessionError::Unsupported {
                what: "an agent session".to_owned(),
            });
        }
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

        let profile = core_profile(request.profile.as_ref());
        let title = profile.label();
        let size = TerminalSize::new(request.cols, request.rows);
        let mut spec = SessionSpec::new(profile).with_size(size);
        if let Some(cwd) = confine_cwd(request.cwd.as_ref())? {
            spec = spec.with_cwd(cwd);
        }
        for (key, value) in &request.env_overrides {
            // `with_env`, never `with_env_overriding_the_scrub`: the scrub runs after this,
            // so a caller cannot reintroduce `ANTHROPIC_API_KEY` or a
            // `CLAUDE_CODE_CHILD_SESSION` by naming it here. A security default an ordinary
            // caller can undo is a suggestion, not a default.
            spec = spec.with_env(key.as_str(), value.as_str());
        }

        let (pty, output) = PtySession::spawn(spec).map_err(SessionError::Spawn)?;
        let handle = pty.handle().clone();
        let incarnation = self.next_incarnation(&pane_key);

        let session = Arc::new(OwnedSession {
            handle: handle.clone(),
            pane_key: pane_key.clone(),
            incarnation: incarnation.clone(),
            kind: request.kind,
            title,
            created_at_ms: now_ms(),
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
                cwd: Some(cwd.clone()),
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
    fn an_agent_session_says_which_version_serves_it_rather_than_failing_blankly() {
        let registry = SessionRegistry::new();
        let refused = registry.create(&SessionCreate {
            kind: SessionKind::Agent,
            pane_key: None,
            profile: None,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        });
        let Err(error) = refused else {
            panic!("v0.1 serves no agent sessions");
        };
        let envelope = error.into_envelope();
        assert_eq!(*envelope.code(), ErrorCode::Unsupported);
        assert!(
            envelope
                .next_steps()
                .iter()
                .any(|step| step.contains("v0.2")),
            "an agent request should name the version that serves it"
        );
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
