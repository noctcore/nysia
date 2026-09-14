//! Finding a daemon, or starting exactly one — §12 question 5, answered.
//!
//! The question is: *how does a client discover and start the daemon, given that two clients
//! may race to spawn one and that a killed daemon can leave a socket file behind?* Both
//! halves have an answer, and the answers are different on the two platforms because the
//! endpoints are different objects.
//!
//! # Is it alive? Never ask the pid
//!
//! A stale endpoint is **not** detected by checking whether the pid in the lease still
//! exists. Pids are reused, and a client that attached to whatever now holds the old number
//! is exactly the failure the launch nonce exists to prevent. Liveness is three steps and
//! only ever these three:
//!
//! 1. **connect** — on Unix a socket file with no daemon behind it refuses the connection,
//!    which is a far stronger signal than the file's existence. On Windows the question does
//!    not arise: a pipe name *is* the object, and it stops existing when the last handle to it
//!    closes, so there is no such thing as a stale pipe.
//! 2. **`hello`** — which returns the daemon's [`nysia_proto::DaemonIdentity`].
//! 3. **[`nysia_proto::PidRecord::describes`]** — the lease beside the endpoint must describe
//!    that identity. When it does not, something is listening but it is not the daemon the
//!    file was written for, which is reachable by a crash and an immediate restart.
//!
//! # Two clients racing to spawn
//!
//! Whoever binds the endpoint wins, and the kernel decides. On Windows that is
//! `first_pipe_instance`, which refuses the second creator outright. On Unix `bind` fails
//! with `AddrInUse`.
//!
//! A lock file sits in front of that anyway, because losing the race *after* spawning a
//! process is wasteful: the loser's daemon starts, fails to bind, and exits, and in the
//! meantime two processes have been created for one endpoint. The lock is held by the
//! operating system rather than by a flag in a file — `flock` on Unix, an exclusive share
//! mode on Windows — so a spawner that is killed mid-spawn releases it, and there is no such
//! thing as a stale lock to have to reason about.
//!
//! The loser does not fail. It waits for the winner's daemon to answer and uses that one,
//! which is the whole point: there is one daemon per endpoint, and which process started it
//! does not matter to anybody.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nysia_proto::{ClientId, ClientRole, DaemonIdentity, ErrorCode, ErrorEnvelope};

use crate::rpc::client::{Client, ClientError};
use crate::rpc::endpoint::{ENDPOINT_VAR, Endpoint, RUNTIME_DIR_VAR};
use crate::rpc::errors::envelope;
use crate::rpc::lease::PidRecordFile;
use crate::rpc::transport::TransportError;

/// How long to wait for a freshly spawned daemon to start answering.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// How often to re-dial while waiting for one.
const READY_POLL: Duration = Duration::from_millis(50);

/// How long to wait for the spawn lock before giving up on the other spawner.
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a daemon could not be found or started.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// Nothing is listening and this caller was told not to start one.
    #[error("no daemon is listening on {endpoint}")]
    Absent {
        /// Where nothing was listening.
        endpoint: String,
    },
    /// The daemon could not be started.
    #[error("could not start a daemon: {0}")]
    Spawn(#[source] std::io::Error),
    /// A daemon was started but never began answering.
    #[error("started a daemon on {endpoint} but it did not answer within {seconds}s")]
    NeverReady {
        /// Where it should have answered.
        endpoint: String,
        /// How long it was given.
        seconds: u64,
    },
    /// The spawn lock could not be taken.
    #[error("another process has been starting a daemon for {seconds}s")]
    LockTimeout {
        /// How long the wait lasted.
        seconds: u64,
    },
    /// Connecting or handshaking failed.
    #[error(transparent)]
    Client(#[from] ClientError),
}

impl DiscoveryError {
    /// The error as an envelope, so a CLI failure carries next steps like every other.
    #[must_use]
    pub fn envelope(&self) -> ErrorEnvelope {
        match self {
            Self::Client(err) => err.envelope(),
            Self::Absent { .. } => envelope(
                ErrorCode::Internal,
                self.to_string(),
                "start one with `nysia --daemon`",
                &["or drop --no-spawn and let the verb start one for you"],
            )
            .retryable(true)
            .with_next_command_args(["nysia", "--daemon"]),
            Self::Spawn(_) => envelope(
                ErrorCode::SpawnFailed,
                self.to_string(),
                "check that the `nysia` binary is on PATH and executable",
                &["`nysia --daemon` run by hand will say why it cannot start"],
            ),
            Self::NeverReady { .. } => envelope(
                ErrorCode::Internal,
                self.to_string(),
                "read the daemon log in the runtime directory to see why it did not bind",
                &["`nysia --daemon` run in the foreground prints the same diagnosis"],
            )
            .retryable(true),
            Self::LockTimeout { .. } => envelope(
                ErrorCode::SessionBusy,
                self.to_string(),
                "retry in a moment",
                &["another client is starting the daemon this one wants"],
            )
            .retryable(true),
        }
    }
}

/// Whether a client may start a daemon it does not find.
#[derive(Debug, Clone)]
pub enum SpawnPolicy {
    /// Do not start one. Nothing listening is an error.
    ///
    /// What `--no-spawn` selects, and what a supervisor wants: it is starting the daemon
    /// itself and a client that quietly started a second one would be a surprise.
    Never,
    /// Start one by running `program --daemon` if nothing answers.
    IfAbsent {
        /// The `nysia` binary to run. Usually `std::env::current_exe()`.
        program: PathBuf,
    },
}

/// A daemon that was found or started, and which of the two it was.
#[derive(Debug)]
pub struct Discovered {
    /// The connected, handshaken client.
    pub client: Client,
    /// Whether this call started the daemon.
    pub spawned: bool,
    /// Whether the lease beside the endpoint describes the daemon that answered.
    ///
    /// `false` means something is listening and it is not what the file says — a crash and an
    /// immediate restart will do it. The connection is still good: the daemon that answered is
    /// the one that owns the endpoint, and the record is the thing that is out of date.
    pub lease_matches: bool,
}

impl From<Ensured<Client>> for Discovered {
    fn from(ensured: Ensured<Client>) -> Self {
        Self {
            client: ensured.connection,
            spawned: ensured.spawned,
            lease_matches: ensured.lease_matches,
        }
    }
}

/// What a caller's probe found when it dialled the endpoint.
///
/// Two answers, never three, and the missing third is the point: "the daemon refused my
/// version" is not one of them. A probe reports a refusal as its own error, because folding
/// it into [`Probed::Absent`] is how a client ends up starting a second daemon next to one
/// that was perfectly willing to talk to somebody else.
#[derive(Debug)]
pub enum Probed<T> {
    /// Something is listening and it completed the handshake.
    Answering {
        /// Whatever the probe wants to keep — a connected client, a socket, or `()`.
        connection: T,
        /// Who answered, which is what the lease is checked against.
        identity: DaemonIdentity,
    },
    /// Nothing is listening.
    Absent,
}

/// A daemon [`ensure_daemon`] found or started, and whatever its probe kept.
#[derive(Debug)]
pub struct Ensured<T> {
    /// What the probe handed back from the dial that answered.
    pub connection: T,
    /// Whether this call started the daemon.
    pub spawned: bool,
    /// Whether the lease beside the endpoint describes the daemon that answered.
    pub lease_matches: bool,
}

/// Why [`ensure_daemon`] could not hand back a daemon.
///
/// Generic over the probe's own error rather than flattening it into a string: the caller
/// supplied the dial, so the caller is the only party that can say what went wrong with it.
/// The window's transport phrases a refused handshake with next steps of its own, and a seam
/// that turned that into [`DiscoveryError::Client`] would throw them away on the one path
/// where a person is reading the message.
#[derive(Debug, thiserror::Error)]
pub enum EnsureError<E: std::error::Error + 'static> {
    /// Finding or starting the daemon failed.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// The probe failed for a reason that is not "nothing is listening".
    #[error(transparent)]
    Probe(E),
    /// The caller could not say what it may start, asked after nothing answered.
    ///
    /// Its own variant rather than folded into [`Self::Probe`] because of *when* it is
    /// reached: only after step 1 found nothing listening. A caller whose policy is expensive
    /// or fallible — the window's, which has to find the runtime it ships beside itself — pays
    /// for it on the cold path alone, and a window with no runtime beside it still attaches to
    /// a daemon that is already there.
    #[error(transparent)]
    Policy(E),
}

impl From<EnsureError<DiscoveryError>> for DiscoveryError {
    fn from(error: EnsureError<DiscoveryError>) -> Self {
        match error {
            EnsureError::Discovery(err) | EnsureError::Probe(err) | EnsureError::Policy(err) => err,
        }
    }
}

/// Connect through `probe`, starting **one** daemon if `policy` allows and none is there.
///
/// Synchronous, and that is the whole reason it exists. The window's transport is blocking by
/// construction — every Tauri command is `spawn_blocking`, and a runtime inside one is how a
/// panic becomes `abort()` (traps register #2) — so it cannot call [`discover`], which is
/// `async` and hands back a [`Client`] built on this crate's tokio transport. What the window
/// needs is not a second spawn-if-absent; it is *this* one with its own dial substituted for
/// the one it cannot use. So the policy lives here, once:
///
/// 1. probe — if something answers, that is the daemon, and no lock is taken;
/// 2. **resolve `policy`**, which is the first moment anything to start has to exist;
/// 3. take the spawn lock, which the operating system holds and releases;
/// 4. **probe again**, because somebody may have won it while this caller waited;
/// 5. run `program --daemon`, detached;
/// 6. probe until it answers or [`READY_TIMEOUT`] passes.
///
/// [`discover`] runs exactly these steps, on a blocking thread, with an async dial bridged
/// into `probe`. There is one implementation of the race and both callers are inside it.
///
/// # Why `policy` is a closure
///
/// Because step 2 is where it belongs, and a value would have been resolved before step 1.
/// **Spawning needs a program; attaching does not.** The window finds the runtime it ships
/// beside itself, and that lookup fails — permanently, with "reinstall" — when the file is
/// not there; resolved eagerly it refused a daemon that was already listening, which is
/// step 1 of this very contract denied by its own caller. A `FnOnce` puts the cost and the
/// failure on the only path that can use either.
///
/// # The contract on `probe`
///
/// It must answer [`Probed::Absent`] **only** for "nothing is listening", and report every
/// other failure as `Err`. Step 6 calls it in a loop, so it must be cheap when the answer is
/// no, and it must leave no connection behind when it answers `Absent`.
///
/// # Errors
///
/// [`DiscoveryError::Absent`] when nothing is listening and `policy` forbids starting one,
/// [`DiscoveryError::LockTimeout`] when another spawner held the lock throughout,
/// [`DiscoveryError::Spawn`] when the program could not be run, [`DiscoveryError::NeverReady`]
/// when it ran and never answered, [`EnsureError::Policy`] for a policy that could not be
/// resolved, and [`EnsureError::Probe`] for anything the probe refused.
pub fn ensure_daemon<T, E>(
    endpoint: &Endpoint,
    policy: impl FnOnce() -> Result<SpawnPolicy, E>,
    mut probe: impl FnMut() -> Result<Probed<T>, E>,
) -> Result<Ensured<T>, EnsureError<E>>
where
    E: std::error::Error + 'static,
{
    if let Probed::Answering {
        connection,
        identity,
    } = probe().map_err(EnsureError::Probe)?
    {
        return Ok(settle(endpoint, connection, &identity, false));
    }

    // Asked for only now, with nothing listening established. Everything above this line
    // runs on a window that has no runtime beside it at all.
    let program = match policy().map_err(EnsureError::Policy)? {
        SpawnPolicy::Never => {
            return Err(DiscoveryError::Absent {
                endpoint: endpoint.listening().to_string(),
            }
            .into());
        }
        SpawnPolicy::IfAbsent { program } => program,
    };

    let _lock = SpawnLock::acquire(endpoint)?;
    // Somebody may have won while this caller was waiting for the lock. Re-probing before
    // spawning is what makes the lock worth taking: without it the loser starts a process that
    // can only fail to bind and exit.
    if let Probed::Answering {
        connection,
        identity,
    } = probe().map_err(EnsureError::Probe)?
    {
        return Ok(settle(endpoint, connection, &identity, false));
    }

    spawn_daemon(&program, endpoint).map_err(DiscoveryError::Spawn)?;

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Probed::Answering {
            connection,
            identity,
        } = probe().map_err(EnsureError::Probe)?
        {
            return Ok(settle(endpoint, connection, &identity, true));
        }
        if Instant::now() >= deadline {
            return Err(DiscoveryError::NeverReady {
                endpoint: endpoint.listening().to_string(),
                seconds: READY_TIMEOUT.as_secs(),
            }
            .into());
        }
        std::thread::sleep(READY_POLL);
    }
}

/// Connect to the daemon, starting one if `policy` allows and none is there.
///
/// The async half of [`ensure_daemon`], and a *caller* of it rather than a second copy of it.
/// The first dial happens here because the overwhelmingly common case is a daemon that is
/// already listening and it costs one round trip; everything after that is the seam.
///
/// # Errors
///
/// Returns [`DiscoveryError::Absent`] when nothing is listening and the policy forbids
/// starting one, [`DiscoveryError::Spawn`] or [`DiscoveryError::NeverReady`] when starting one
/// does not work, and [`DiscoveryError::Client`] when the handshake is refused.
pub async fn discover(
    endpoint: &Endpoint,
    client_id: &ClientId,
    role: ClientRole,
    policy: &SpawnPolicy,
) -> Result<Discovered, DiscoveryError> {
    if let Some(client) = try_connect(endpoint, client_id, role).await? {
        let identity = client.identity().clone();
        return Ok(settle(endpoint, client, &identity, false).into());
    }

    // Nothing is listening. Whether that is an error or the start of a spawn is the seam's
    // decision, not this function's — including under `SpawnPolicy::Never`, which costs one
    // extra dial to reach. Short-circuiting it here would put the answer to "may this client
    // start a daemon" in two places, which is the shape of the bug this whole module avoids.
    //
    // `Handle::block_on` from a thread of the blocking pool, which is the supported bridge in
    // that direction: this thread drives no tasks, so parking it parks nothing else, and the
    // runtime's own threads go on servicing the IO the dial below waits for.
    let handle = tokio::runtime::Handle::current();
    let endpoint = endpoint.clone();
    let client_id = client_id.clone();
    let policy = policy.clone();
    let ensured = tokio::task::spawn_blocking(move || {
        // Already resolved, because a CLI's program is `current_exe` and costs nothing to
        // name. The seam asks late; this caller has nothing to defer.
        ensure_daemon(
            &endpoint,
            move || Ok(policy),
            || match handle.block_on(try_connect(&endpoint, &client_id, role))? {
                Some(client) => {
                    let identity = client.identity().clone();
                    Ok(Probed::Answering {
                        connection: client,
                        identity,
                    })
                }
                None => Ok(Probed::Absent),
            },
        )
    })
    .await
    // A `JoinError` means the blocking thread panicked or the runtime is shutting down.
    // Neither is a daemon fault and both leave the caller with no daemon, so it is reported as
    // the spawn failing rather than quietly becoming "nothing is listening".
    .map_err(|error| DiscoveryError::Spawn(std::io::Error::other(error)))??;
    Ok(ensured.into())
}

/// Record what the probe reached, after checking the lease beside the endpoint.
///
/// The third step of the liveness check. The connection already proved something is there;
/// this proves it is the something the file describes.
fn settle<T>(
    endpoint: &Endpoint,
    connection: T,
    identity: &DaemonIdentity,
    spawned: bool,
) -> Ensured<T> {
    let lease_matches = PidRecordFile::at(endpoint.pid_record_path())
        .read()
        .ok()
        .flatten()
        .is_some_and(|record| record.describes(identity));
    if !lease_matches {
        tracing::warn!(
            endpoint = %endpoint.listening(),
            "the daemon answering does not match the lease beside it; the record is stale"
        );
    }
    Ensured {
        connection,
        spawned,
        lease_matches,
    }
}

/// Dial, returning `None` when nothing is listening.
///
/// Every other failure is an error: "the daemon refused my version" and "there is no daemon"
/// want opposite responses, and folding them together is how a client ends up starting a
/// second daemon next to one that was perfectly willing to talk to somebody else.
async fn try_connect(
    endpoint: &Endpoint,
    client_id: &ClientId,
    role: ClientRole,
) -> Result<Option<Client>, DiscoveryError> {
    match Client::connect(endpoint, client_id, role).await {
        Ok(client) => Ok(Some(client)),
        Err(ClientError::Transport(TransportError::NotListening { .. })) => Ok(None),
        Err(err) => Err(DiscoveryError::Client(err)),
    }
}

/// Start `program --daemon`, detached, with its output in the runtime directory.
///
/// Detaching is not tidiness. A daemon that inherits its spawner's stdout holds that pipe
/// open for as long as it lives, so a CLI that spawned one and then read its own output would
/// never see the pipe close — the command would appear to hang for the daemon's entire
/// lifetime, which under D-1 is days.
fn spawn_daemon(program: &std::path::Path, endpoint: &Endpoint) -> std::io::Result<()> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(endpoint.log_path())?;
    let mut command = std::process::Command::new(program);
    command
        .arg("--daemon")
        // **Named, not inherited.** The child resolves its endpoint from the environment, so a
        // spawner that pinned one — an interop test, a second daemon on one machine, the
        // window's own proof — would otherwise start a daemon that binds somewhere else and
        // then wait out `READY_TIMEOUT` for an answer that was never coming to this endpoint.
        // Passing both is deliberate: the override names the endpoint, and the runtime
        // directory is still where the lease, the lock and the log belong.
        .env(RUNTIME_DIR_VAR, endpoint.runtime_dir())
        .env(ENDPOINT_VAR, endpoint.listening().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS so it gets no console of its own, and CREATE_NEW_PROCESS_GROUP so
        // a Ctrl-C in the terminal that started it does not also stop the daemon — which
        // would make the runtime exactly as disposable as the UI.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, for the same reason: a Ctrl-C aimed at the shell that
        // spawned it must not reach the daemon.
        command.process_group(0);
    }

    // Windows inherits *every* inheritable handle when any stdio is redirected, not only the
    // three that were named. That includes the write end of whatever pipe this process's own
    // stdout happens to be — so a daemon spawned from inside `H=$(nysia session create)` holds
    // that pipe open for its whole life, and the shell waits for an answer it already has.
    // Detaching the parent's own handles first is what stops it; on Unix the redirections
    // replace fds 0, 1 and 2 outright and nothing else is inherited, so there is nothing to do.
    #[cfg(windows)]
    detach_parent_stdio();

    let child = command.spawn()?;
    // The handle is dropped on purpose. Waiting on the daemon is precisely what a client must
    // not do; on Unix that leaves a zombie until this process exits, which is a few hundred
    // bytes of process table for the life of one CLI invocation.
    drop(child);
    Ok(())
}

/// Stop this process's stdio handles from being inherited by anything it spawns.
///
/// Best effort, and harmless to this process: clearing the inherit flag does not affect the
/// current process's own use of the handle, only whether a child receives a copy.
#[cfg(windows)]
fn detach_parent_stdio() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };

    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    for handle in handles {
        if handle.is_null() {
            continue;
        }
        // SAFETY: the handle is this process's own standard handle, borrowed for the call.
        // `SetHandleInformation` only changes the flag bits named by the mask and writes
        // nothing through a pointer.
        let _ =
            unsafe { SetHandleInformation(HANDLE(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
    }
}

/// The spawn lock, held by the operating system for as long as the process lives.
///
/// A flag written into a file would need a staleness rule, and a staleness rule is the same
/// problem as the stale socket one level down. `flock` and an exclusive Windows share mode
/// are released by the kernel when the holder dies, so a spawner killed mid-spawn leaves
/// nothing behind to reason about.
#[derive(Debug)]
struct SpawnLock {
    #[allow(
        dead_code,
        reason = "the lock is the open file; dropping it is what releases it"
    )]
    file: std::fs::File,
}

impl SpawnLock {
    /// Take the lock, waiting for whoever holds it.
    ///
    /// Synchronous, like everything else [`ensure_daemon`] does: the window has no runtime to
    /// wait on, and the wait is bounded by [`LOCK_TIMEOUT`] whoever is calling.
    fn acquire(endpoint: &Endpoint) -> Result<Self, DiscoveryError> {
        let path = endpoint.lock_path();
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match Self::try_acquire(&path) {
                Ok(Some(lock)) => return Ok(lock),
                Ok(None) => {}
                Err(err) => return Err(DiscoveryError::Spawn(err)),
            }
            if Instant::now() >= deadline {
                return Err(DiscoveryError::LockTimeout {
                    seconds: LOCK_TIMEOUT.as_secs(),
                });
            }
            std::thread::sleep(READY_POLL);
        }
    }

    /// One attempt. `Ok(None)` means somebody else holds it.
    #[cfg(unix)]
    fn try_acquire(path: &std::path::Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        // SAFETY: `file` owns a live descriptor for the duration of the call, and `flock` only
        // reads it. `LOCK_NB` makes the call return rather than block, which is what lets the
        // wait above be bounded.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked == 0 {
            return Ok(Some(Self { file }));
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
            _ => Err(err),
        }
    }

    /// One attempt. `Ok(None)` means somebody else holds it.
    #[cfg(windows)]
    fn try_acquire(path: &std::path::Path) -> std::io::Result<Option<Self>> {
        use std::os::windows::fs::OpenOptionsExt;

        // `share_mode(0)` is the lock: no other process may open the file at all until this
        // handle closes, and the kernel closes it when this process ends however it ends.
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .share_mode(0)
            .open(path)
        {
            Ok(file) => Ok(Some(Self { file })),
            Err(err)
                if err.kind() == std::io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(32) =>
            {
                // ERROR_SHARING_VIOLATION: somebody else is starting a daemon right now.
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_id() -> ClientId {
        "nysia-test".parse().expect("a well-formed client id")
    }

    #[tokio::test]
    async fn nothing_listening_and_no_permission_to_spawn_says_how_to_start_one() {
        let endpoint = crate::rpc::endpoint::scratch("absent");
        let err = discover(
            &endpoint,
            &client_id(),
            ClientRole::Control,
            &SpawnPolicy::Never,
        )
        .await
        .expect_err("nothing is listening");
        assert!(matches!(err, DiscoveryError::Absent { .. }));

        let envelope = err.envelope();
        assert!(envelope.is_retryable(), "starting one would fix it");
        assert_eq!(
            envelope.next_command_args(),
            Some(["nysia".to_owned(), "--daemon".to_owned()].as_slice()),
            "the error should carry the argv that fixes it, not just prose"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn only_one_spawner_holds_the_lock_and_it_is_released_when_dropped() {
        let endpoint = crate::rpc::endpoint::scratch("lock");
        let held = SpawnLock::acquire(&endpoint).expect("takes the lock");
        assert!(
            SpawnLock::try_acquire(&endpoint.lock_path())
                .expect("the attempt itself works")
                .is_none(),
            "a second spawner must not also believe it won"
        );

        drop(held);
        assert!(
            SpawnLock::try_acquire(&endpoint.lock_path())
                .expect("the attempt itself works")
                .is_some(),
            "releasing the lock must let the next spawner in"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_lease_that_describes_nothing_listening_is_not_taken_for_a_live_daemon() {
        // The stale case. The record is there, the pid in it may even exist — and nothing is
        // listening, which is the only question that matters.
        let endpoint = crate::rpc::endpoint::scratch("stale");
        let lease = PidRecordFile::at(endpoint.pid_record_path());
        lease
            .write(&nysia_proto::DaemonIdentity {
                pid: std::process::id(),
                started_at_ms: 1,
                launch_nonce: nysia_proto::LaunchNonce::generate(),
                app_version: "0.0.0".to_owned(),
            })
            .expect("writes a lease for a daemon that is not there");

        let err = discover(
            &endpoint,
            &client_id(),
            ClientRole::Control,
            &SpawnPolicy::Never,
        )
        .await
        .expect_err("a lease is not a daemon");
        assert!(matches!(err, DiscoveryError::Absent { .. }));
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// A probe that answers from a script, and counts how often it was asked.
    ///
    /// The point of the counter is step 3: the re-probe after the lock is what stops the
    /// loser of a race from starting a process that can only fail to bind and exit, and a
    /// seam that dropped it would pass every test that only looked at the outcome.
    #[derive(Debug, Default)]
    struct Scripted {
        answers: std::cell::RefCell<std::collections::VecDeque<Probed<&'static str>>>,
        asked: std::cell::Cell<usize>,
    }

    impl Scripted {
        fn of(answers: impl IntoIterator<Item = Probed<&'static str>>) -> Self {
            Self {
                answers: std::cell::RefCell::new(answers.into_iter().collect()),
                asked: std::cell::Cell::new(0),
            }
        }

        fn probe(&self) -> Result<Probed<&'static str>, DiscoveryError> {
            self.asked.set(self.asked.get() + 1);
            Ok(self
                .answers
                .borrow_mut()
                .pop_front()
                .unwrap_or(Probed::Absent))
        }
    }

    fn answering(tag: &'static str) -> Probed<&'static str> {
        Probed::Answering {
            connection: tag,
            identity: DaemonIdentity {
                pid: std::process::id(),
                started_at_ms: 1,
                launch_nonce: nysia_proto::LaunchNonce::generate(),
                app_version: "0.1.0".to_owned(),
            },
        }
    }

    /// A program that exists on both platforms and does nothing useful with `--daemon`.
    ///
    /// The tests below never want the thing they spawn to *become* a daemon — that is what
    /// the desktop crate's proof does, against the real binary. What they want is a spawn
    /// that succeeds, so the step after it can be observed.
    fn harmless() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("cmd.exe")
        } else {
            PathBuf::from("/bin/echo")
        }
    }

    #[test]
    fn a_daemon_that_is_already_there_is_used_and_no_lock_is_taken() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-present");
        let probe = Scripted::of([answering("live")]);

        let ensured = ensure_daemon(&endpoint, || Ok(SpawnPolicy::Never), || probe.probe())
            .expect("something answered");
        assert_eq!(ensured.connection, "live");
        assert!(!ensured.spawned, "nothing was started");
        assert_eq!(probe.asked.get(), 1, "one dial, and no lock");
        assert!(
            SpawnLock::try_acquire(&endpoint.lock_path())
                .expect("the attempt itself works")
                .is_some(),
            "the fast path took the spawn lock, which serialises every client's first connect"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn nothing_listening_and_no_permission_to_spawn_is_absent_through_the_seam_too() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-absent");
        let probe = Scripted::of([]);
        let err = ensure_daemon(&endpoint, || Ok(SpawnPolicy::Never), || probe.probe())
            .expect_err("nothing is listening");
        assert!(matches!(
            err,
            EnsureError::Discovery(DiscoveryError::Absent { .. })
        ));
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// Step 2 happens **after** step 1, and a daemon that is already there never reaches it.
    ///
    /// The defect: the window resolves the runtime it ships beside itself, and that lookup
    /// fails permanently — "reinstall Nysia" — when the file is not there. Resolved before the
    /// probe, a window with no sidecar refused a daemon that was listening and answering, and
    /// stopped for the rest of its life. The seam's own contract says the opposite in step 1,
    /// so this asserts the order rather than trusting the numbering.
    #[test]
    fn the_policy_is_never_asked_for_when_something_already_answers() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-policy-late");
        let probe = Scripted::of([answering("live")]);
        let asked = std::cell::Cell::new(0_usize);

        let ensured = ensure_daemon(
            &endpoint,
            || {
                asked.set(asked.get() + 1);
                // What a window with no runtime beside it produces. Reaching this at all is
                // the bug; answering it with a failure is how the test says so.
                Err(DiscoveryError::Spawn(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the runtime is not installed beside the app",
                )))
            },
            || probe.probe(),
        )
        .expect("something answered, so nothing had to be started");

        assert_eq!(ensured.connection, "live");
        assert_eq!(
            asked.get(),
            0,
            "the policy was resolved for a daemon that was already listening"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// And when nothing answers it **is** asked, exactly once, after the probe.
    ///
    /// The other half of the same claim: deferring the policy must not quietly turn
    /// spawn-if-absent into never-spawn. The probe count read from inside the closure is what
    /// pins the order — a resolver called first would see zero dials.
    #[test]
    fn nothing_listening_resolves_the_policy_once_and_only_after_the_probe() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-policy-once");
        let probe = Scripted::of([Probed::Absent, answering("the one it started")]);
        let asked = std::cell::Cell::new(0_usize);
        let dials_when_asked = std::cell::Cell::new(0_usize);

        let ensured = ensure_daemon(
            &endpoint,
            || {
                asked.set(asked.get() + 1);
                dials_when_asked.set(probe.asked.get());
                Ok(SpawnPolicy::IfAbsent {
                    program: harmless(),
                })
            },
            || probe.probe(),
        )
        .expect("the re-probe under the lock answered");

        assert_eq!(ensured.connection, "the one it started");
        assert_eq!(
            asked.get(),
            1,
            "the policy was resolved {} times",
            asked.get()
        );
        assert!(
            dials_when_asked.get() >= 1,
            "the policy was resolved before anything had been dialled"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// A policy that cannot be resolved is the caller's own failure, handed back untouched.
    ///
    /// Untouched because the caller is the only party that can phrase it: the window's
    /// "reinstall Nysia, then reopen it" is worth more to a person than anything this module
    /// could compose, and a seam that wrapped it in [`DiscoveryError::Spawn`] would throw the
    /// next steps away on the one path where somebody is reading them.
    #[test]
    fn a_policy_that_cannot_be_resolved_stops_before_the_lock_and_keeps_its_own_words() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-policy-fails");
        let probe = Scripted::of([]);

        let err = ensure_daemon(
            &endpoint,
            || {
                Err(DiscoveryError::Spawn(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "reinstall Nysia",
                )))
            },
            || probe.probe(),
        )
        .expect_err("there is nothing to start");

        match &err {
            EnsureError::Policy(cause) => assert!(
                cause.to_string().contains("reinstall Nysia"),
                "the caller's sentence did not survive: {cause}"
            ),
            other => panic!("got {other:?}"),
        }
        assert!(
            SpawnLock::try_acquire(&endpoint.lock_path())
                .expect("the attempt itself works")
                .is_some(),
            "the lock was taken for a spawn that could never happen"
        );
        assert!(
            !endpoint.log_path().exists(),
            "something was started with no program to start"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn a_probe_that_fails_for_any_other_reason_never_starts_a_daemon() {
        // The whole reason `Probed` has two variants and not three. A refusal — a daemon that
        // speaks another protocol version, a handshake the peer would not accept — means
        // something *is* listening, and starting a second daemon beside it is the failure this
        // arm exists to prevent.
        let endpoint = crate::rpc::endpoint::scratch("ensure-refused");
        let refused = ensure_daemon(
            &endpoint,
            || {
                Ok(SpawnPolicy::IfAbsent {
                    program: harmless(),
                })
            },
            || Err::<Probed<&'static str>, _>(DiscoveryError::LockTimeout { seconds: 1 }),
        )
        .expect_err("the probe refused");
        assert!(
            matches!(
                refused,
                EnsureError::Probe(DiscoveryError::LockTimeout { .. })
            ),
            "the probe's own error must survive, got {refused:?}"
        );
        assert!(
            !endpoint.log_path().exists(),
            "a refusal started a daemon, which is the one thing it must not do"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn the_loser_of_the_race_re_probes_under_the_lock_rather_than_spawning() {
        // Step 3, and the reason the lock is worth taking at all: whoever was holding it has
        // just finished starting a daemon, so the loser's own spawn could only produce a
        // process that fails to bind and exits.
        let endpoint = crate::rpc::endpoint::scratch("ensure-loser");
        let probe = Scripted::of([Probed::Absent, answering("the winner's")]);

        let ensured = ensure_daemon(
            &endpoint,
            || {
                Ok(SpawnPolicy::IfAbsent {
                    program: harmless(),
                })
            },
            || probe.probe(),
        )
        .expect("the winner's daemon answered");
        assert_eq!(ensured.connection, "the winner's");
        assert!(
            !ensured.spawned,
            "the loser started a second daemon for one endpoint"
        );
        assert_eq!(
            probe.asked.get(),
            2,
            "the re-probe under the lock is missing"
        );
        assert!(
            !endpoint.log_path().exists(),
            "nothing should have been spawned, so there is no daemon log to write"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn a_spawn_waits_for_the_daemon_to_answer_before_handing_it_back() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-spawn");
        let probe = Scripted::of([
            Probed::Absent,
            Probed::Absent,
            Probed::Absent,
            answering("the one it started"),
        ]);

        let ensured = ensure_daemon(
            &endpoint,
            || {
                Ok(SpawnPolicy::IfAbsent {
                    program: harmless(),
                })
            },
            || probe.probe(),
        )
        .expect("it answered on the third poll");
        assert_eq!(ensured.connection, "the one it started");
        assert!(ensured.spawned, "this call is the one that started it");
        assert!(
            probe.asked.get() >= 4,
            "the readiness wait polled {} times, so it returned before anything answered",
            probe.asked.get()
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn a_program_that_cannot_be_run_is_a_spawn_failure_naming_it() {
        let endpoint = crate::rpc::endpoint::scratch("ensure-missing");
        let probe = Scripted::of([]);
        let err = ensure_daemon(
            &endpoint,
            || {
                Ok(SpawnPolicy::IfAbsent {
                    program: endpoint.runtime_dir().join("no-such-nysia-binary"),
                })
            },
            || probe.probe(),
        )
        .expect_err("there is no such program");
        assert!(
            matches!(err, EnsureError::Discovery(DiscoveryError::Spawn(_))),
            "got {err:?}"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn the_lease_beside_a_daemon_that_answered_is_still_checked() {
        // The third liveness step, which the seam owns rather than leaving to each caller —
        // otherwise the window and the CLI would disagree about what a stale record means.
        let endpoint = crate::rpc::endpoint::scratch("ensure-lease");
        let identity = DaemonIdentity {
            pid: std::process::id(),
            started_at_ms: 7,
            launch_nonce: nysia_proto::LaunchNonce::generate(),
            app_version: "0.1.0".to_owned(),
        };
        PidRecordFile::at(endpoint.pid_record_path())
            .write(&identity)
            .expect("writes a lease");

        let matching = ensure_daemon(
            &endpoint,
            || Ok(SpawnPolicy::Never),
            || {
                Ok::<_, DiscoveryError>(Probed::Answering {
                    connection: "live",
                    identity: identity.clone(),
                })
            },
        )
        .expect("something answered");
        assert!(matching.lease_matches);

        let impostor = ensure_daemon(
            &endpoint,
            || Ok(SpawnPolicy::Never),
            || Ok::<_, DiscoveryError>(answering("live")),
        )
        .expect("something answered");
        assert!(
            !impostor.lease_matches,
            "a daemon the lease does not describe was reported as the one it describes"
        );
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }
}
