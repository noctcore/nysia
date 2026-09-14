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

use super::env;
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
    /// Environment applied **before** the scrub.
    ///
    /// Anything here is layered onto the inherited block and then §7.1's scrub runs over
    /// the result, so this cannot reintroduce a scrubbed variable. That is deliberate: see
    /// [`SessionSpec::with_env`].
    pub env: Vec<(OsString, OsString)>,
    /// Environment applied **after** the scrub, and so the only way to set one of the
    /// variables the scrub removes.
    ///
    /// Private, unlike every other field here, and that asymmetry is the guarantee rather
    /// than an oversight. A `pub` field is reachable by a struct literal or a `push`, which
    /// would have made [`SessionSpec::with_env_overriding_the_scrub`] a convention instead
    /// of the only door — and a security default an ordinary caller can undo is a
    /// suggestion, not a default. Privacy closes both routes, and it takes struct literals
    /// of `SessionSpec` with it: outside this module [`SessionSpec::new`] is now the only
    /// way to build one.
    ///
    /// [`SessionSpec::env`] stays public because reaching it directly changes nothing —
    /// the scrub still runs over whatever is there.
    env_overriding_the_scrub: Vec<(OsString, OsString)>,
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
            env_overriding_the_scrub: Vec::new(),
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

    /// Set an environment variable for the session.
    ///
    /// The scrub runs *after* this, so a variable in [`env::SCRUBBED_VARS`] set here does
    /// not survive. That is the point rather than a limitation: those seven are not a
    /// default to be overridden, they are a guard. Three of them decide whether a launched
    /// `claude` is treated as somebody's child session, and two are a credential and an
    /// endpoint — so a layer that forwards environment it got from settings or from a
    /// GitHub issue must not be able to reroute an agent or hand it a key just by naming
    /// the variable.
    ///
    /// If a caller genuinely has to set one, that is
    /// [`SessionSpec::with_env_overriding_the_scrub`], which is separate so the call site
    /// is greppable and obvious in review.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Set an environment variable *after* the scrub has run, defeating it for that name.
    ///
    /// The only way to set one of [`env::SCRUBBED_VARS`], and that is literal rather than a
    /// figure of speech: the list this appends to is private, so no struct literal and no
    /// direct `push` reaches around this method. Nothing in v1 calls it; it exists so that
    /// if something ever must, the decision is visible at the call site and findable with
    /// one grep, instead of being an emergent property of the order two loops happen to run
    /// in.
    ///
    /// Reach for [`SessionSpec::with_env`] instead unless the variable is one the scrub
    /// removes and you have a reason that survives being read aloud.
    #[must_use]
    pub fn with_env_overriding_the_scrub(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.env_overriding_the_scrub
            .push((key.into(), value.into()));
        self
    }

    /// Build the command this spec spawns, with the scrub applied in the right place.
    ///
    /// Split out from [`PtySession::spawn`] so the ordering can be tested without starting
    /// a process: the ordering *is* the guarantee, and a guarantee that can only be checked
    /// by spawning a shell and reading its screen is one nobody checks.
    fn build_command(&self) -> Result<portable_pty::CommandBuilder, SpawnError> {
        let mut command = self.profile.command()?;
        if let Some(cwd) = &self.cwd {
            command.cwd(cwd);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        // §7.1's scrub runs last, so that layering cannot undo it. `ShellProfile::command`
        // also scrubs; that one is defence in depth for a caller building a command
        // directly, and this one is the authority.
        env::sanitize(&mut command);
        for (key, value) in &self.env_overriding_the_scrub {
            command.env(key, value);
        }
        Ok(command)
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
    /// One of the session's two threads could not be started.
    #[error("could not start a session thread: {0}")]
    Thread(#[source] std::io::Error),
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
        let command = spec.build_command()?;
        let program = command
            .get_argv()
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default();

        // The Job Object is created *before* the child exists. Creating it afterwards put a
        // fallible step inside the window where an error unwinds through `pair.master`, and
        // dropping the master runs `ClosePseudoConsole`, which blocks until the client exits
        // — with no reader thread in existence to drain its flush (traps register #6).
        #[cfg(windows)]
        let job = teardown::JobObject::new().map_err(SpawnError::Confinement)?;

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
        let killer = child.clone_killer();
        // Past this point the child is running, so every remaining failure kills it before
        // unwinding. A bare `?` here would drop `pair.master` with no reader thread yet,
        // which is the deadlock above, and would leave the child behind as a certain orphan.
        let mut unwind = child.clone_killer();

        // The reader starts first, before anything else that can fail. Every later failure
        // then unwinds through a `pair.master` that has a live reader draining it, which is
        // what `ClosePseudoConsole` needs; only this call itself is left without one.
        let exit = Arc::new(ExitSlot::default());
        let eof = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));

        let (tx, rx) = sync_channel::<Vec<u8>>(spec.output_queue.max(1));
        let reader_thread = match spawn_reader(reader, tx, Arc::clone(&eof), Arc::clone(&closing)) {
            Ok(handle) => handle,
            Err(err) => {
                let _ = unwind.kill();
                return Err(SpawnError::Thread(err));
            }
        };

        // Assignment can only follow the spawn — `portable-pty`'s attribute list does not
        // expose `PROC_THREAD_ATTRIBUTE_JOB_LIST` — so this one step stays in the window.
        #[cfg(windows)]
        {
            let assigned = child
                .as_raw_handle()
                .ok_or_else(|| {
                    SpawnError::Confinement(std::io::Error::other(
                        "the spawned child exposed no process handle",
                    ))
                })
                .and_then(|handle| job.assign(handle).map_err(SpawnError::Confinement));
            if let Err(err) = assigned {
                let _ = unwind.kill();
                return Err(err);
            }
        }

        let waiter_thread = match spawn_waiter(child, Arc::clone(&exit)) {
            Ok(handle) => handle,
            Err(err) => {
                let _ = unwind.kill();
                return Err(SpawnError::Thread(err));
            }
        };

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
            let Some(leader) = self.pid else {
                let _ = self
                    .killer
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .kill();
                return;
            };
            // Capture the tree while the shell is still holding it together: once the leader
            // dies its children reparent away and there is nothing left to enumerate.
            let tree = teardown::collect_tree(leader);

            if let Err(err) = teardown::signal_group(leader, teardown::SIGTERM) {
                tracing::warn!(%err, leader, "SIGTERM to the process group failed");
            }
            teardown::signal_pids(&tree, teardown::SIGTERM);

            // The whole tree is polled, **leader included**. Polling only the jobs made the
            // SIGKILL phase unreachable for the leader — with no background job there is
            // nothing in the list and `all_gone` is trivially true — and a leader that
            // ignores SIGTERM is precisely what the phase exists for: an interactive `bash`
            // ignores it, and one with `IGNOREEOF` set survives the master drop too.
            //
            // Including it costs nothing. This session's waiter thread is already blocked in
            // `wait()`, so the leader is reaped the instant it dies and stops answering
            // `kill(pid, 0)`; and `shutdown` waits out the same grace on the exit slot
            // either way.
            if !teardown::wait_for(grace, || teardown::all_gone(&tree)) {
                if let Err(err) = teardown::signal_group(leader, teardown::SIGKILL) {
                    tracing::warn!(%err, leader, "SIGKILL to the process group failed");
                }
                // Re-collect as well as re-signalling: the grace period is long enough for
                // the shell to have started something new while it was ignoring SIGTERM.
                let late = teardown::collect_tree(leader);
                let swept = teardown::signal_pids(&tree, teardown::SIGKILL)
                    + teardown::signal_pids(&late, teardown::SIGKILL);
                if swept > 0 {
                    tracing::debug!(swept, leader, "swept the session for surviving jobs");
                }
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
///
/// # Errors
///
/// Returns the OS error if the thread cannot be created.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    chunks: SyncSender<Vec<u8>>,
    eof: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nysia-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = vec![0u8; READ_BUFFER];
            // Set once the consumer has gone away. The reader does *not* stop then: an
            // undrained master leaves the child blocked in `write`, and on Windows leaves
            // `ClosePseudoConsole` with nowhere to put its flush.
            let mut discarding = false;
            loop {
                match reader.read(&mut buffer) {
                    // A zero-length read is EOF: every slave fd is closed.
                    Ok(0) => break,
                    Ok(read) => {
                        if !discarding && !send_chunk(&chunks, buffer[..read].to_vec(), &closing) {
                            discarding = true;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            eof.store(true, Ordering::Release);
        })
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
///
/// # Errors
///
/// Returns the OS error if the thread cannot be created.
fn spawn_waiter(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    exit: Arc<ExitSlot>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nysia-pty-waiter".to_owned())
        .spawn(move || {
            let status = child
                .wait()
                .unwrap_or_else(|_| ExitStatus::with_exit_code(u32::MAX));
            exit.set(status);
        })
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

    /// A spec for the platform's default shell, sized for the tests.
    fn shell_spec() -> SessionSpec {
        SessionSpec::new(ShellProfile::platform_default()).with_size(TerminalSize::new(120, 30))
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

    /// The token a ready shell prints, which its own echo cannot contain.
    const READY: &str = "nysia-ready-42";

    /// A command whose *output* is [`READY`] but whose typed form is not.
    ///
    /// Matching on a token that appears in the typed line as well means kernel echo plus a
    /// readline redisplay satisfies the count on its own, with the shell never having run
    /// anything. Making the shell compute the token is what turns the wait into a real
    /// signal: the arithmetic is only resolved by a shell that reached its prompt, parsed a
    /// line and executed it.
    fn ready_probe(profile: &ShellProfile) -> String {
        match profile.id() {
            // PowerShell expands a subexpression in an unquoted argument.
            "pwsh" => "echo nysia-ready-$(6*7)".to_owned(),
            // `cmd` has no arithmetic in `echo`, but a zero-length substring of a variable
            // that always exists splits the token just as well.
            "cmd" => "echo nysia-ready-%CD:~0,0%42".to_owned(),
            // Every POSIX shell, including Git Bash and WSL.
            _ => "echo nysia-ready-$((6*7))".to_owned(),
        }
    }

    /// Spawn `spec` and return once its shell is drawing a prompt and running commands.
    ///
    /// Spawning and waiting are one step on purpose. The probe below has to be written in
    /// the language of the shell that was actually started, and when the two were chosen
    /// separately they drifted: the Windows tree-kill test spawns `cmd` explicitly while the
    /// probe came from `platform_default`, which on a runner with PowerShell installed is
    /// `pwsh` — so a PowerShell probe was typed at `cmd`, which echoed it back verbatim and
    /// the wait timed out. Taking both from one `spec` makes that mismatch unrepresentable.
    fn spawn_ready(spec: SessionSpec) -> (PtySession, PtyOutput, TerminalState) {
        let profile = spec.profile.clone();
        let size = spec.size;
        let (session, output) = PtySession::spawn(spec).expect("the session must spawn");
        let mut state = TerminalState::new(size, VtConfig::default());
        wait_until_ready(&session, &output, &mut state, &profile);
        (session, output, state)
    }

    /// Block until the shell is drawing a prompt and consuming input.
    ///
    /// Writing to a session the moment it is spawned races the shell's own startup, and the
    /// loser is the command: a macOS runner prints the "default interactive shell is now
    /// zsh" notice between the two halves of whatever was typed, and what reaches the
    /// parser is spliced nonsense. Waiting for the shell to *run* something is the only
    /// proof that the next write will be read whole.
    fn wait_until_ready(
        session: &PtySession,
        output: &PtyOutput,
        state: &mut TerminalState,
        profile: &ShellProfile,
    ) {
        let probe = ready_probe(profile);
        session
            .write(format!("{probe}{}", line_end()).as_bytes())
            .expect("write the readiness probe");
        assert!(
            pump_until(session, output, state, |screen| screen.contains(READY)),
            "the shell never ran the readiness probe; screen was:\n{}",
            state.screen()
        );
    }

    /// What ends a typed line for the platform's shell.
    fn line_end() -> &'static str {
        if cfg!(windows) { "\r" } else { "\n" }
    }

    #[test]
    fn an_echo_reaches_the_rendered_screen() {
        let (session, output, mut state) = spawn_ready(shell_spec());
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
        let (session, output, mut state) = spawn_ready(shell_spec());

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
        let (session, _output, _state) = spawn_ready(shell_spec());

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
        let (session, _output) = PtySession::spawn(shell_spec()).expect("spawn");
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
        let (session, _output) = PtySession::spawn(shell_spec()).expect("spawn");
        assert!(session.handle().as_str().starts_with("sess_"));
        assert!(session.pid().is_some_and(|pid| pid > 0));
    }

    #[test]
    fn each_shell_gets_a_readiness_probe_written_in_its_own_language() {
        // The three are genuinely different languages, and sending one shell another's
        // probe is what broke the Windows leg: `cmd` echoed a PowerShell subexpression back
        // verbatim and the wait timed out. Both forms are verified against a live shell —
        // the `cmd` one on the development machine, the `pwsh` one on CI, which is where
        // PowerShell is installed.
        let cmd = ready_probe(&ShellProfile::CommandPrompt);
        let pwsh = ready_probe(&ShellProfile::PowerShell7);
        let posix = ready_probe(&ShellProfile::Posix);
        assert_ne!(cmd, pwsh);
        assert_ne!(pwsh, posix);
        for probe in [&cmd, &pwsh, &posix] {
            assert!(
                !probe.contains(READY),
                "{probe:?} already contains {READY}, so an echo of it would satisfy the wait"
            );
        }
    }

    #[test]
    fn a_session_is_shareable_across_threads() {
        // W4 puts one of these behind an `Arc` in the daemon, so this is a real constraint
        // rather than a formality. The output half is `Send` but not `Sync`: an
        // `mpsc::Receiver` has one consumer by construction.
        const fn assert_send_sync<T: Send + Sync>() {}
        const fn assert_send<T: Send>() {}
        assert_send_sync::<PtySession>();
        assert_send::<PtyOutput>();
        // The grid goes behind the daemon's mutex alongside the session, so it has to be
        // `Send` for that mutex to be `Sync`.
        assert_send::<TerminalState>();
    }

    #[test]
    fn dropping_the_consumer_does_not_wedge_shutdown() {
        // With nobody receiving, a reader that stopped reading would leave the child
        // blocked in `write` and, on Windows, leave `ClosePseudoConsole` with nowhere to
        // put its flush. The reader keeps draining and discards instead.
        let (session, output) = PtySession::spawn(shell_spec()).expect("spawn");
        drop(output);
        let _ = session.write(b"echo nysia-nobody-is-listening\r");

        let start = Instant::now();
        session.shutdown(Duration::from_secs(2));
        assert!(
            start.elapsed() < DEADLINE,
            "shutdown took {:?}",
            start.elapsed()
        );
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
    fn eof_and_exit_are_reported_independently() {
        // Traps register #11, expressed as what this crate guarantees rather than as what a
        // kernel happens to do. Which of the two arrives first is not portable, and both
        // directions were measured on CI rather than assumed:
        //
        // - Linux keeps the master readable while *any* slave fd is open, so a backgrounded
        //   grandchild can hold EOF off long after the shell has exited.
        // - Darwin does not. Closing every slave fd from a child that keeps running
        //   produced no EOF at all, and EOF instead arrived when the child exited — so on
        //   macOS the two effectively coincide and no test can separate them by timing.
        //
        // Asserting either ordering therefore pins a kernel, not this code. What is Nysia's
        // to promise is that neither fact is *derived* from the other: the exit status comes
        // from `wait()` on the waiter thread and EOF from the read loop on the reader
        // thread, and that is what this pins.
        let (session, _output, _state) = spawn_ready(shell_spec());

        // Output has flowed and the reader has gone quiet again. Neither is an exit, and an
        // implementation that inferred one from an idle reader would fail here.
        assert!(
            session.exit_status().is_none(),
            "a live child has no exit status"
        );
        assert!(
            !session.is_at_eof(),
            "a live child's master has not reached EOF"
        );

        session
            .write(format!("exit 7{}", line_end()).as_bytes())
            .expect("write");

        // A real `wait()` result: EOF carries no exit code, so a 7 here cannot have been
        // synthesised from the reader ending.
        let status = session
            .wait_for_exit(DEADLINE)
            .expect("the child must report an exit status");
        assert_eq!(status.exit_code(), 7);

        // The other direction, asserted where it is deterministic. ConPTY keeps the master
        // readable until the pseudoconsole is closed, so the reader is still blocked in
        // `read` and an implementation that set EOF from the waiter would trip here.
        #[cfg(windows)]
        assert!(
            !session.is_at_eof(),
            "EOF must not be inferred from the child exiting"
        );

        // On Darwin the kernel releases the slave with the process, so EOF legitimately
        // arrives; what must hold there is that it does not rewrite what `wait()` reported.
        // Only this branch waits, so Windows does not pay five seconds for a fact it
        // already knows.
        #[cfg(unix)]
        let _ = teardown::wait_for(Duration::from_secs(5), || session.is_at_eof());

        assert_eq!(
            session.exit_status().map(|status| status.exit_code()),
            Some(7)
        );
    }

    #[test]
    fn the_scrub_wins_over_a_caller_that_tries_to_reintroduce_it() {
        // The ordering *is* the guarantee, so it is checked without spawning anything:
        // swap the two loops in `build_command` and every one of these comes back set.
        let mut spec = SessionSpec::new(ShellProfile::platform_default());
        for name in env::SCRUBBED_VARS {
            spec = spec.with_env(*name, "nysia-attacker-value");
        }
        let command = spec.build_command().expect("the default shell must build");

        for name in env::SCRUBBED_VARS {
            assert!(
                command.get_env(name).is_none(),
                "{name} survived the scrub and can be set by any caller"
            );
        }
        // The forced terminal description wins over a caller too.
        assert_eq!(command.get_env("TERM"), Some(env::FORCED_TERM.as_ref()));
    }

    #[test]
    fn an_ordinary_variable_still_reaches_the_session() {
        // The scrub is seven names, not a blanket refusal: everything else a caller sets
        // has to survive, or `with_env` would be useless.
        let spec = SessionSpec::new(ShellProfile::platform_default())
            .with_env("NYSIA_PANE_KEY", "tab:leaf");
        let command = spec.build_command().expect("the default shell must build");
        assert_eq!(command.get_env("NYSIA_PANE_KEY"), Some("tab:leaf".as_ref()));
    }

    #[test]
    fn the_named_override_is_the_only_way_past_the_scrub() {
        // The escape hatch exists, and it is the *only* hatch. Nothing in v1 calls it.
        let spec = SessionSpec::new(ShellProfile::platform_default())
            .with_env("ANTHROPIC_BASE_URL", "https://ignored.invalid")
            .with_env_overriding_the_scrub("ANTHROPIC_BASE_URL", "https://deliberate.invalid");
        let command = spec.build_command().expect("the default shell must build");
        assert_eq!(
            command.get_env("ANTHROPIC_BASE_URL"),
            Some("https://deliberate.invalid".as_ref())
        );
    }

    #[test]
    fn a_scrubbed_variable_cannot_be_smuggled_into_a_live_session() {
        // The end-to-end form of the same guarantee, because this was reported as reachable
        // on ConPTY: a session built with an attacker-controlled endpoint and a child-session
        // marker printed both on the rendered screen.
        let (session, output, mut state) = spawn_ready(
            shell_spec()
                .with_env("CLAUDECODE", "1")
                .with_env("ANTHROPIC_BASE_URL", "https://nysia-attacker.invalid"),
        );

        #[cfg(windows)]
        let probe = "echo scrubbed=[%CLAUDECODE%][%ANTHROPIC_BASE_URL%]";
        #[cfg(unix)]
        let probe = "echo scrubbed=[$CLAUDECODE][$ANTHROPIC_BASE_URL]";
        session
            .write(format!("{probe}{}", line_end()).as_bytes())
            .expect("write");

        assert!(
            pump_until(&session, &output, &mut state, |screen| {
                screen.matches("scrubbed=[").count() >= 2
            }),
            "screen was:\n{}",
            state.screen()
        );
        let screen = state.screen();
        assert!(
            !screen.contains("nysia-attacker.invalid"),
            "the endpoint reached the session; screen was:\n{screen}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_leader_that_ignores_sigterm_is_still_killed() {
        // The SIGKILL phase exists for exactly this, and nothing covered it: the orphan test
        // asserts only the job's pid, and a leader that ignores SIGTERM was left alive when
        // the poll stopped including it. Restore the `jobs`-only poll and this goes red.
        //
        // Both `exec`s are load-bearing, for different reasons.
        //
        // The outer one makes the leader itself the process that ignores SIGTERM, and one
        // that never reads stdin — otherwise the newline and VEOF that `portable-pty` writes
        // when it drops the master half would end the shell on their own and the kill would
        // not be what proved anything.
        //
        // The inner one leaves the leader with **no children**, which is what makes this a
        // guard rather than a coin flip. The jobs-only poll fails when the list it polls is
        // empty, so the leader has to be the only thing in the tree. A `while :; do sleep 1;
        // done` loop here instead — the shape this test used to have — puts a child in that
        // list, and `trap "" TERM` is SIG_IGN, which survives `exec`, so the child ignores
        // the signal too and only its own timeout ends it. Whether that lands inside the
        // 500ms grace is a race the test does not control: against the jobs-only poll the
        // loop form is reported in #21 as failing about half the time on the macOS runner,
        // and measured on Linux it never failed at all — 14 runs, 14 passes, a guard for a
        // bug present in every one. Lengthening the loop's sleep makes that worse, because
        // the child then reliably outlives the grace and the SIGKILL phase always runs. One
        // long `exec sleep`, with no child to poll, is the shape that fails every time.
        //
        // #21 proposed exactly that remedy — a longer sleep inside the loop, to make this
        // guard deterministic — and it is backwards, so nobody should try it again. The
        // longer sleep does not remove the race, it settles it on the wrong side: with the
        // loop form the child reliably outlives the grace, the SIGKILL phase reliably runs,
        // and the false pass becomes permanent rather than occasional. A guard that lies
        // every time is worse than one that lies half the time.
        //
        // Measured in a container, for the record:
        //
        // - shipped code, with this guard as it stands: 10 passes in 10;
        // - the kill phase reverted, with this guard as it stands: 20 failures in 20, each
        //   taking an identical 15.51s;
        // - the kill phase reverted, with this guard back in the loop form described
        //   above: 10 passes in 10. That last row is the bug this guard exists to catch,
        //   passing ten times out of ten.
        let (session, output, mut state) = spawn_ready(
            SessionSpec::new(ShellProfile::Posix).with_size(TerminalSize::new(120, 30)),
        );
        let leader = session.pid().expect("a spawned session has a pid");

        session
            .write(b"exec sh -c 'trap \"\" TERM; echo nysia-notrap-$((6*7)); exec sleep 300'\n")
            .expect("write");
        assert!(
            pump_until(&session, &output, &mut state, |screen| screen
                .contains("nysia-notrap-42")),
            "the leader never reported that it was ignoring SIGTERM; screen was:\n{}",
            state.screen()
        );

        session.shutdown(Duration::from_millis(500));

        assert!(
            teardown::wait_for(Duration::from_secs(5), || !pid_is_alive(leader)),
            "leader {leader} ignored SIGTERM and was never SIGKILLed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_tree_kill_leaves_no_orphan() {
        // A backgrounded job is the case `killpg` alone misses: an interactive shell with
        // job control puts it in a process group of its own.
        //
        // The job reports its own pid rather than the shell reporting `$!`, because the
        // macOS runner's login shell is an interactive `bash` with history expansion on,
        // where `$!.end` is read as the history event `!.end` and the whole line is
        // rejected with "event not found". `sh -c` gets a pid from `$$` with no `!` in
        // sight, and `exec` keeps that pid for the `sleep` that replaces it.
        let (session, output, mut state) = spawn_ready(
            SessionSpec::new(ShellProfile::Posix).with_size(TerminalSize::new(120, 30)),
        );

        session
            .write(b"sh -c 'echo nysia-orphan-pid=$$.end; exec sleep 300' &\n")
            .expect("write");

        assert!(
            pump_until(&session, &output, &mut state, |screen| orphan_pid(screen)
                .is_some()),
            "screen was:\n{}",
            state.screen()
        );
        let pid = orphan_pid(&state.screen()).expect("the background pid must be printed");

        session.shutdown(Duration::from_secs(2));

        // The sweep reaches the job through the process table; `portable-pty`'s own killer
        // would have reached the shell and nothing else.
        assert!(
            teardown::wait_for(Duration::from_secs(5), || !pid_is_alive(pid)),
            "pid {pid} survived the tree-kill"
        );
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 delivers nothing and only performs the existence check.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// The pid the shell printed, accepted only once the whole number has arrived.
    ///
    /// The marker is `nysia-orphan-pid=<digits>.end`, and the `.end` is load-bearing: a
    /// read of the master can split the line mid-number, and taking whatever digits have
    /// turned up so far yields `410` from `41023` — a pid that is very likely some
    /// unrelated live process, which turns "the sweep did not work" and "the read was
    /// split" into the same failure. Requiring the terminator makes the test wait instead.
    ///
    /// The last occurrence is the shell's output; the earlier one is the command echoing,
    /// and it never parses because `$!` and `$p.Id` are not digits.
    fn orphan_pid(screen: &str) -> Option<u32> {
        let (_, tail) = screen.rsplit_once("nysia-orphan-pid=")?;
        let (digits, _) = tail.split_once(".end")?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }

    #[test]
    #[cfg(windows)]
    fn a_tree_kill_leaves_no_orphan() {
        // ConPTY has no signals, so the Job Object is the only tree-kill there is.
        let (session, output, mut state) = spawn_ready(
            SessionSpec::new(ShellProfile::CommandPrompt).with_size(TerminalSize::new(120, 30)),
        );

        // `Start-Process` detaches the child from the console, which is exactly the kind of
        // process a kill aimed at the direct child would leave behind.
        session
            .write(
                b"powershell -NoProfile -Command \"$p = Start-Process -PassThru -WindowStyle \
                     Hidden ping -ArgumentList '-n','600','127.0.0.1'; 'nysia-orphan-pid=' + \
                     $p.Id + '.end'\"\r",
            )
            .expect("write");

        assert!(
            pump_until(&session, &output, &mut state, |screen| orphan_pid(screen)
                .is_some()),
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
