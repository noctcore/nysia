//! A PTY session: the pseudo-terminal, the process tree behind it, and the one blocking
//! thread that drains its output.
//!
//! # Threads
//!
//! Two per session, both blocking, neither async:
//!
//! - the **reader**, which does nothing but `read` the master and hand chunks to a bounded
//!   channel. Reading a pty from an async task is what turns one slow consumer into a
//!   stalled runtime, so this is deliberately a plain thread.
//! - the **waiter**, which blocks in `wait()` and records the child's exit status.
//!
//! # Exit is not EOF
//!
//! They are separate threads because they observe separate events (traps register #11).
//! EOF on the master arrives only once *every* slave fd is closed, so a shell that
//! backgrounded a server and exited leaves the master readable indefinitely. The exit
//! status comes from `wait()` and is never inferred from the reader ending, nor the other
//! way round.
//!
//! # Shutdown order
//!
//! On Windows `ClosePseudoConsole` — which runs when the last handle to the pty pair drops
//! — flushes conhost's remaining output into the pipe and blocks until it has been read.
//! Calling it from the reader thread therefore deadlocks (traps register #6), and so does
//! joining the reader before dropping the master. [`PtySession::shutdown`] kills the tree,
//! drops the master from the calling thread *while the reader is still looping*, and only
//! then joins the reader. The bounded channel is a second way to deadlock the same flush,
//! so the reader switches to discarding chunks once shutdown has begun.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use nysia_proto::SessionHandle;
use portable_pty::{ChildKiller, ExitStatus, MasterPty, PtySize, native_pty_system};

use super::profile::{ProfileError, ShellProfile};
use super::teardown::{self, DEFAULT_GRACE};
use crate::vt::TerminalSize;

/// How much is read from the master in one `read` call.
const READ_BUFFER: usize = 64 * 1024;

/// How long the reader waits before retrying a send into a full output channel. Only ever
/// reached when the consumer has stopped draining, which is exactly when the child should
/// be made to block.
const BACKPRESSURE_POLL: Duration = Duration::from_millis(2);

/// How long [`PtySession::shutdown`] will wait for a thread to finish before detaching it.
/// A detached thread cannot hold anything up; blocking forever could.
const JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// How many output chunks may be queued before the reader stops reading.
///
/// This is the backpressure chokepoint: once it fills the reader blocks, the kernel pty
/// buffer fills, and the child blocks in its own `write`. That is the whole answer to a
/// `yes` flood.
pub const DEFAULT_OUTPUT_QUEUE: usize = 64;

/// What to spawn, and how.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Which shell to run.
    pub profile: ShellProfile,
    /// The working directory, or the caller's own when `None`.
    pub cwd: Option<PathBuf>,
    /// The initial grid size.
    pub size: TerminalSize,
    /// Environment applied after the scrub, so a caller can deliberately set something the
    /// scrub would otherwise have removed.
    pub env: Vec<(OsString, OsString)>,
    /// How many output chunks may be queued before the reader stops reading.
    pub output_queue: usize,
}

impl SessionSpec {
    /// A spec for `profile` with every other field at its default.
    #[must_use]
    pub fn new(profile: ShellProfile) -> Self {
        Self {
            profile,
            cwd: None,
            size: TerminalSize::default(),
            env: Vec::new(),
            output_queue: DEFAULT_OUTPUT_QUEUE,
        }
    }

    /// Run the session in `cwd`.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Start the session at `size`.
    #[must_use]
    pub fn with_size(mut self, size: TerminalSize) -> Self {
        self.size = size;
        self
    }

    /// Set an environment variable for the session, after the scrub.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// Why a session could not be started.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The shell profile could not be turned into a command.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The pty pair itself could not be opened.
    #[error("could not open a pty: {0}")]
    OpenPty(String),
    /// The child process could not be started.
    #[error("could not spawn {program}: {reason}")]
    Spawn {
        /// What was being spawned.
        program: String,
        /// The underlying failure, as the pty layer reported it.
        reason: String,
    },
    /// The master's writer half could not be taken.
    #[error("could not take the pty writer: {0}")]
    Writer(String),
    /// The process tree could not be confined, so a tree-kill could not be guaranteed.
    #[error("could not confine the process tree: {0}")]
    Confinement(#[source] std::io::Error),
}

/// One chunk of output, or the reason there is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Raw bytes the child wrote, in order.
    Chunk(Vec<u8>),
    /// Nothing arrived within the caller's timeout. The session is still live.
    Timeout,
    /// The master reached EOF and no further chunk will ever arrive. This says nothing
    /// about whether the child has exited.
    Eof,
}

/// The receiving half of a session's output channel.
///
/// Held separately from [`PtySession`] so that one thread can consume output while another
/// writes, resizes or shuts the session down.
#[derive(Debug)]
pub struct PtyOutput {
    chunks: Receiver<Vec<u8>>,
}

impl PtyOutput {
    /// Wait up to `timeout` for the next chunk.
    ///
    /// Every wait in this module is bounded; a test that can hang is a test that will.
    pub fn recv_timeout(&self, timeout: Duration) -> Output {
        match self.chunks.recv_timeout(timeout) {
            Ok(chunk) => Output::Chunk(chunk),
            Err(RecvTimeoutError::Timeout) => Output::Timeout,
            Err(RecvTimeoutError::Disconnected) => Output::Eof,
        }
    }

    /// Take whatever is already queued without waiting.
    #[must_use]
    pub fn drain(&self) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| self.chunks.try_recv().ok()).collect()
    }
}

/// Where the waiter thread records the child's exit status.
#[derive(Debug, Default)]
struct ExitSlot {
    status: Mutex<Option<ExitStatus>>,
    changed: Condvar,
}

impl ExitSlot {
    /// The exit status if the child has already exited.
    fn peek(&self) -> Option<ExitStatus> {
        self.status
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// Wait up to `timeout` for the child to exit.
    fn wait(&self, timeout: Duration) -> Option<ExitStatus> {
        let guard = self.status.lock().unwrap_or_else(|err| err.into_inner());
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |status| status.is_none())
            .unwrap_or_else(|err| err.into_inner());
        guard.clone()
    }

    /// Record the exit status and wake every waiter.
    fn set(&self, status: ExitStatus) {
        let mut guard = self.status.lock().unwrap_or_else(|err| err.into_inner());
        *guard = Some(status);
        drop(guard);
        self.changed.notify_all();
    }
}

/// A live pty session.
pub struct PtySession {
    handle: SessionHandle,
    pid: Option<u32>,
    /// Dropped during shutdown, from a thread that is not the reader.
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// Also dropped during shutdown: on Unix it is a dup of the master fd and would keep
    /// the pty alive on its own.
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    exit: Arc<ExitSlot>,
    eof: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    finished: AtomicBool,
    reader: Mutex<Option<JoinHandle<()>>>,
    waiter: Mutex<Option<JoinHandle<()>>>,
    #[cfg(windows)]
    job: teardown::JobObject,
}

impl PtySession {
    /// Open a pty, spawn the session's shell into it, and start draining its output.
    ///
    /// # Errors
    ///
    /// Returns [`SpawnError`] when the shell is unavailable, the pty cannot be opened, the
    /// child cannot be started, or — on Windows — the process tree cannot be confined to a
    /// Job Object, which would leave a later tree-kill unable to do its job.
    pub fn spawn(spec: SessionSpec) -> Result<(Self, PtyOutput), SpawnError> {
        let mut command = spec.profile.command()?;
        if let Some(cwd) = &spec.cwd {
            command.cwd(cwd);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let program = command
            .get_argv()
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default();

        let pair = native_pty_system()
            .openpty(pty_size(spec.size))
            .map_err(|err| SpawnError::OpenPty(err.to_string()))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| SpawnError::Writer(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| SpawnError::Writer(err.to_string()))?;

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| SpawnError::Spawn {
                program,
                reason: err.to_string(),
            })?;
        // The slave exists only to spawn. Dropping it now leaves the master as the sole
        // owner of the pty, which is what makes the shutdown order below deterministic.
        drop(pair.slave);

        let pid = child.process_id();

        #[cfg(windows)]
        let job = {
            let job = teardown::JobObject::new().map_err(SpawnError::Confinement)?;
            if let Some(handle) = child.as_raw_handle() {
                job.assign(handle).map_err(SpawnError::Confinement)?;
            }
            job
        };

        let killer = child.clone_killer();
        let exit = Arc::new(ExitSlot::default());
        let eof = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));

        let (tx, rx) = sync_channel::<Vec<u8>>(spec.output_queue.max(1));
        let reader_thread = spawn_reader(reader, tx, Arc::clone(&eof), Arc::clone(&closing));
        let waiter_thread = spawn_waiter(child, Arc::clone(&exit));

        let session = Self {
            handle: SessionHandle::generate(),
            pid,
            master: Mutex::new(Some(pair.master)),
            writer: Mutex::new(Some(writer)),
            killer: Mutex::new(killer),
            exit,
            eof,
            closing,
            finished: AtomicBool::new(false),
            reader: Mutex::new(Some(reader_thread)),
            waiter: Mutex::new(Some(waiter_thread)),
            #[cfg(windows)]
            job,
        };
        Ok((session, PtyOutput { chunks: rx }))
    }

    /// This session's runtime-scoped identity.
    #[must_use]
    pub fn handle(&self) -> &SessionHandle {
        &self.handle
    }

    /// The child's process id.
    ///
    /// On Unix the child is a session leader, so this is also its process-group id. §3.2's
    /// peer-credential ancestry walk starts from here.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Write input to the child.
    ///
    /// # Errors
    ///
    /// Returns an error once the session has been shut down and the writer released, or
    /// when the underlying write fails.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut guard = self.writer.lock().unwrap_or_else(|err| err.into_inner());
        let writer = guard.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the session is shut down")
        })?;
        writer.write_all(bytes)?;
        writer.flush()
    }

    /// Tell the kernel, and therefore the child, that the window changed size.
    ///
    /// # Errors
    ///
    /// Returns an error once the session has been shut down, or when the resize fails.
    pub fn resize(&self, size: TerminalSize) -> std::io::Result<()> {
        let guard = self.master.lock().unwrap_or_else(|err| err.into_inner());
        let master = guard.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the session is shut down")
        })?;
        master
            .resize(pty_size(size))
            .map_err(|err| std::io::Error::other(err.to_string()))
    }

    /// The child's exit status, if it has exited. Never inferred from EOF.
    #[must_use]
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit.peek()
    }

    /// Wait up to `timeout` for the child to exit.
    #[must_use]
    pub fn wait_for_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        self.exit.wait(timeout)
    }

    /// Whether the master has reached EOF.
    ///
    /// Independent of [`PtySession::exit_status`]: a backgrounded grandchild holding a
    /// slave fd keeps this `false` long after the shell exited.
    #[must_use]
    pub fn is_at_eof(&self) -> bool {
        self.eof.load(Ordering::Acquire)
    }

    /// Kill the session's whole process tree and release the pty.
    ///
    /// Idempotent: a second call is a no-op, which is what lets `Drop` call it
    /// unconditionally. Returns the child's exit status if one was observed.
    pub fn shutdown(&self, grace: Duration) -> Option<ExitStatus> {
        if self.finished.swap(true, Ordering::AcqRel) {
            return self.exit.peek();
        }
        // From here the reader must never block on a full channel: `ClosePseudoConsole`
        // will not return until its flush has been read.
        self.closing.store(true, Ordering::Release);

        self.kill_tree(grace);
        let status = self.exit.wait(grace);

        // Release the pty from *this* thread, not the reader's. This is what runs
        // `ClosePseudoConsole` on Windows, and it must happen while the reader is still
        // looping so that the flush has somewhere to go.
        drop(
            self.writer
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take(),
        );
        drop(
            self.master
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take(),
        );

        join_or_detach(&self.reader);
        join_or_detach(&self.waiter);

        status.or_else(|| self.exit.peek())
    }

    /// Terminate the process tree, platform by platform.
    fn kill_tree(&self, grace: Duration) {
        #[cfg(windows)]
        {
            // ConPTY has no signals, so there is no graceful step: the Job Object is the
            // mechanism, and it terminates. Closing the job's last handle in `Drop` is the
            // backstop if this fails.
            let _ = grace;
            if let Err(err) = self.job.terminate() {
                tracing::warn!(%err, "job-object terminate failed; falling back to the killer");
                let _ = self
                    .killer
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .kill();
            }
        }
        #[cfg(unix)]
        {
            let Some(pgid) = self.pid else {
                let _ = self
                    .killer
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .kill();
                return;
            };
            if let Err(err) = teardown::signal_group(pgid, teardown::SIGTERM) {
                tracing::warn!(%err, pgid, "SIGTERM to the process group failed");
            }
            // Give the shell its exit traps, then stop asking.
            if !teardown::wait_for(grace, || teardown::tree_is_gone(pgid))
                && let Err(err) = teardown::signal_group(pgid, teardown::SIGKILL)
            {
                tracing::warn!(%err, pgid, "SIGKILL to the process group failed");
            }
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.shutdown(DEFAULT_GRACE);
    }
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("handle", &self.handle)
            .field("pid", &self.pid)
            .field("exited", &self.exit.peek().is_some())
            .field("eof", &self.is_at_eof())
            .finish_non_exhaustive()
    }
}

/// Start the one blocking thread that drains the master.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    chunks: SyncSender<Vec<u8>>,
    eof: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("nysia-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = vec![0u8; READ_BUFFER];
            loop {
                match reader.read(&mut buffer) {
                    // A zero-length read is EOF: every slave fd is closed.
                    Ok(0) => break,
                    Ok(read) => {
                        if !send_chunk(&chunks, buffer[..read].to_vec(), &closing) {
                            break;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            eof.store(true, Ordering::Release);
        })
        .unwrap_or_else(|err| panic!("could not start the pty reader thread: {err}"))
}

/// Hand a chunk to the consumer, blocking while the queue is full — unless shutdown has
/// begun, in which case the chunk is dropped so the reader keeps draining.
///
/// Returns whether the reader should keep going.
fn send_chunk(chunks: &SyncSender<Vec<u8>>, chunk: Vec<u8>, closing: &AtomicBool) -> bool {
    let mut chunk = chunk;
    loop {
        match chunks.try_send(chunk) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                if closing.load(Ordering::Acquire) {
                    // Discarding is the point: a reader parked here cannot service the
                    // `ClosePseudoConsole` flush, and that deadlocks shutdown.
                    return true;
                }
                chunk = returned;
                std::thread::sleep(BACKPRESSURE_POLL);
            }
            // Nobody is listening any more.
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

/// Start the thread that records the child's exit status, separately from the reader.
fn spawn_waiter(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    exit: Arc<ExitSlot>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("nysia-pty-waiter".to_owned())
        .spawn(move || {
            let status = child
                .wait()
                .unwrap_or_else(|_| ExitStatus::with_exit_code(u32::MAX));
            exit.set(status);
        })
        .unwrap_or_else(|err| panic!("could not start the pty waiter thread: {err}"))
}

/// Join a thread, or give up on it after [`JOIN_TIMEOUT`] and let it run detached.
///
/// A detached thread holds nothing up. Blocking here forever would, and shutdown is on the
/// path of every test in this module.
fn join_or_detach(slot: &Mutex<Option<JoinHandle<()>>>) {
    let handle = slot.lock().unwrap_or_else(|err| err.into_inner()).take();
    let Some(handle) = handle else {
        return;
    };
    if teardown::wait_for(JOIN_TIMEOUT, || handle.is_finished()) {
        let _ = handle.join();
    } else {
        tracing::warn!("a pty thread did not finish in time; detaching it");
    }
}

/// Convert the crate's grid size into the pty crate's, which also carries pixel dimensions
/// nothing here uses.
fn pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::vt::{ReadMode, TerminalState, VtConfig};

    /// Every wait in these tests is bounded.
    const DEADLINE: Duration = Duration::from_secs(20);

    /// Spawn the platform's default shell, sized for the tests.
    fn shell() -> (PtySession, PtyOutput) {
        PtySession::spawn(
            SessionSpec::new(ShellProfile::platform_default())
                .with_size(TerminalSize::new(120, 30)),
        )
        .expect("the platform shell must spawn")
    }

    /// Pump output into a grid until `predicate` holds or the deadline passes.
    ///
    /// The emulator's replies go back to the child on the way through. They are not
    /// optional: `pwsh` opens with `CSI 6 n` and will not draw its prompt until the cursor
    /// position comes back, so a pump that drops them sees exactly one chunk and then
    /// nothing at all.
    fn pump_until(
        session: &PtySession,
        output: &PtyOutput,
        state: &mut TerminalState,
        predicate: impl Fn(&str) -> bool,
    ) -> bool {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            let chunk = output.recv_timeout(Duration::from_millis(250));
            if let Output::Chunk(chunk) = &chunk {
                state.feed(chunk);
            }
            let replies = state.take_replies();
            if !replies.is_empty() {
                let _ = session.write(&replies);
            }
            if predicate(&state.screen()) {
                return true;
            }
            if chunk == Output::Eof {
                return false;
            }
        }
        false
    }

    fn grid() -> TerminalState {
        TerminalState::new(TerminalSize::new(120, 30), VtConfig::default())
    }

    #[test]
    fn an_echo_reaches_the_rendered_screen() {
        let (session, output) = shell();
        let mut state = grid();
        session
            .write(b"echo nysia-marker-one\r")
            .expect("write to the shell");

        // The command itself echoes, so wait for the line the shell printed *after* it.
        assert!(
            pump_until(&session, &output, &mut state, |screen| {
                screen.matches("nysia-marker-one").count() >= 2
            }),
            "screen was:\n{}",
            state.screen()
        );
        let read = state.read(ReadMode::Screen, 0, usize::MAX);
        assert!(read.text.contains("nysia-marker-one"));
    }

    #[test]
    fn a_resize_reaches_the_child() {
        let (session, output) = shell();
        let mut state = grid();
        // Let the shell finish starting up, so the resize lands on a live prompt.
        assert!(
            pump_until(&session, &output, &mut state, |screen| !screen.is_empty()),
            "the shell never drew anything"
        );

        session
            .resize(TerminalSize::new(40, 10))
            .expect("resize the pty");
        state.resize(TerminalSize::new(40, 10));
        assert_eq!(state.size(), TerminalSize::new(40, 10));

        // The grid follows, and the session keeps working at the new size.
        session.write(b"echo nysia-after-resize\r").expect("write");
        assert!(
            pump_until(&session, &output, &mut state, |screen| {
                screen.matches("nysia-after-resize").count() >= 2
            }),
            "screen was:\n{}",
            state.screen()
        );
        // The rendered screen joins soft-wrapped rows, so it is *not* clipped to the new
        // width — that is what makes a read survive a resize. The grid following the
        // resize is covered by `vt::state`'s own reflow test.
        assert_eq!(state.size(), TerminalSize::new(40, 10));
    }

    #[test]
    fn the_exit_status_is_captured() {
        let (session, output) = shell();
        let mut state = grid();
        let _ = pump_until(&session, &output, &mut state, |screen| !screen.is_empty());

        session.write(b"exit 7\r").expect("write");
        let status = session
            .wait_for_exit(DEADLINE)
            .expect("the child must report an exit status");
        assert_eq!(status.exit_code(), 7, "status was {status:?}");
        assert!(!status.success());
        // And it is still there after the fact, without a second wait.
        assert!(session.exit_status().is_some());
    }

    #[test]
    fn shutdown_is_idempotent_and_bounded() {
        let (session, _output) = shell();
        let start = Instant::now();
        session.shutdown(Duration::from_millis(500));
        session.shutdown(Duration::from_millis(500));
        assert!(
            start.elapsed() < DEADLINE,
            "shutdown took {:?}",
            start.elapsed()
        );

        // Writing to a session that is gone is an error, not a panic.
        assert!(session.write(b"echo late\r").is_err());
        assert!(session.resize(TerminalSize::new(80, 24)).is_err());
    }

    #[test]
    fn a_session_has_a_handle_and_a_pid() {
        let (session, _output) = shell();
        assert!(session.handle().as_str().starts_with("sess_"));
        assert!(session.pid().is_some_and(|pid| pid > 0));
    }

    #[test]
    fn a_profile_that_is_not_available_reports_why_rather_than_spawning() {
        #[cfg(unix)]
        let unavailable = ShellProfile::CommandPrompt;
        #[cfg(windows)]
        let unavailable = ShellProfile::Posix;
        let err = PtySession::spawn(SessionSpec::new(unavailable)).unwrap_err();
        assert!(matches!(err, SpawnError::Profile(_)));
    }

    #[test]
    #[cfg(unix)]
    fn a_child_that_exits_is_reported_even_though_the_master_is_still_open() {
        // Traps register #11. The backgrounded `sleep` keeps a slave fd open, so EOF does
        // not arrive — but the shell's exit status must still be reported promptly.
        let (session, _output) = PtySession::spawn(SessionSpec::new(ShellProfile::Posix))
            .expect("a posix shell must spawn");
        session
            .write(b"sleep 30 </dev/null >/dev/null 2>&1 &\nexit 5\n")
            .expect("write");

        let status = session
            .wait_for_exit(DEADLINE)
            .expect("the shell's exit status must arrive before EOF does");
        assert_eq!(status.exit_code(), 5);
        assert!(
            !session.is_at_eof(),
            "EOF must not be inferred from the child exiting"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_tree_kill_leaves_no_orphan() {
        let (session, output) = PtySession::spawn(SessionSpec::new(ShellProfile::Posix))
            .expect("a posix shell must spawn");
        let mut state = grid();
        session
            .write(b"sleep 300 & echo nysia-orphan-pid=$!\n")
            .expect("write");

        assert!(
            pump_until(&session, &output, &mut state, |screen| screen
                .contains("nysia-orphan-pid=")
                && screen.split("nysia-orphan-pid=").count() > 2),
            "screen was:\n{}",
            state.screen()
        );
        let pid = orphan_pid(&state.screen()).expect("the background pid must be printed");

        session.shutdown(Duration::from_secs(2));

        // `killpg` on the session leader reaches the background job too; `portable-pty`'s
        // own killer would only have reached the shell.
        assert!(
            teardown::wait_for(Duration::from_secs(5), || !pid_is_alive(pid)),
            "pid {pid} survived the tree-kill"
        );
    }

    #[cfg(unix)]
    fn orphan_pid(screen: &str) -> Option<u32> {
        // The command itself echoes, so take the last occurrence: the shell's own output.
        screen.rsplit("nysia-orphan-pid=").find_map(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 delivers nothing and only performs the existence check.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    #[cfg(windows)]
    fn a_tree_kill_leaves_no_orphan() {
        // ConPTY has no signals, so the Job Object is the only tree-kill there is.
        let (session, output) = PtySession::spawn(
            SessionSpec::new(ShellProfile::CommandPrompt).with_size(TerminalSize::new(120, 30)),
        )
        .expect("cmd.exe must spawn");
        let mut state = grid();

        // `Start-Process` detaches the child from the console, which is exactly the kind of
        // process a kill aimed at the direct child would leave behind.
        session
            .write(
                b"powershell -NoProfile -Command \"$p = Start-Process -PassThru -WindowStyle \
                     Hidden ping -ArgumentList '-n','600','127.0.0.1'; 'nysia-orphan-pid=' + \
                     $p.Id\"\r",
            )
            .expect("write");

        assert!(
            pump_until(&session, &output, &mut state, |screen| {
                orphan_pid(screen).is_some_and(|pid| pid > 0)
                    && screen.matches("nysia-orphan-pid=").count() >= 2
            }),
            "screen was:\n{}",
            state.screen()
        );
        let pid = orphan_pid(&state.screen()).expect("the background pid must be printed");

        session.shutdown(Duration::from_secs(2));

        assert!(
            teardown::wait_for(Duration::from_secs(10), || !pid_is_alive(pid)),
            "pid {pid} survived the job-object tree-kill"
        );
    }

    #[cfg(windows)]
    fn orphan_pid(screen: &str) -> Option<u32> {
        screen.rsplit("nysia-orphan-pid=").find_map(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
    }

    #[cfg(windows)]
    fn pid_is_alive(pid: u32) -> bool {
        let output = std::process::Command::new("tasklist")
            .args(["/fi", &format!("PID eq {pid}"), "/nh", "/fo", "csv"])
            .output();
        match output {
            Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()),
            Err(_) => false,
        }
    }
}
