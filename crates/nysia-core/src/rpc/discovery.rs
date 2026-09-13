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

use nysia_proto::{ClientId, ClientRole, ErrorCode, ErrorEnvelope};

use crate::rpc::client::{Client, ClientError};
use crate::rpc::endpoint::Endpoint;
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

/// Connect to the daemon, starting one if `policy` allows and none is there.
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
        return Ok(finish(endpoint, client, false));
    }
    let program = match policy {
        SpawnPolicy::Never => {
            return Err(DiscoveryError::Absent {
                endpoint: endpoint.listening().to_string(),
            });
        }
        SpawnPolicy::IfAbsent { program } => program.clone(),
    };

    let _lock = SpawnLock::acquire(endpoint).await?;
    // Somebody may have won while this client was waiting for the lock. Re-probing before
    // spawning is what makes the lock worth taking: without it the loser starts a process that
    // can only fail to bind and exit.
    if let Some(client) = try_connect(endpoint, client_id, role).await? {
        return Ok(finish(endpoint, client, false));
    }

    spawn_daemon(&program, endpoint).map_err(DiscoveryError::Spawn)?;

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(client) = try_connect(endpoint, client_id, role).await? {
            return Ok(finish(endpoint, client, true));
        }
        if Instant::now() >= deadline {
            return Err(DiscoveryError::NeverReady {
                endpoint: endpoint.listening().to_string(),
                seconds: READY_TIMEOUT.as_secs(),
            });
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// Connect and check the lease, without starting anything.
fn finish(endpoint: &Endpoint, client: Client, spawned: bool) -> Discovered {
    // The third step of the liveness check. The connection already proved something is there;
    // this proves it is the something the file describes.
    let lease_matches = PidRecordFile::at(endpoint.pid_record_path())
        .read()
        .ok()
        .flatten()
        .is_some_and(|record| record.describes(client.identity()));
    if !lease_matches {
        tracing::warn!(
            endpoint = %endpoint.listening(),
            "the daemon answering does not match the lease beside it; the record is stale"
        );
    }
    Discovered {
        client,
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

    let child = command.spawn()?;
    // The handle is dropped on purpose. Waiting on the daemon is precisely what a client must
    // not do; on Unix that leaves a zombie until this process exits, which is a few hundred
    // bytes of process table for the life of one CLI invocation.
    drop(child);
    Ok(())
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
    async fn acquire(endpoint: &Endpoint) -> Result<Self, DiscoveryError> {
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
            tokio::time::sleep(READY_POLL).await;
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
    use crate::rpc::endpoint::EnvSource;
    use nysia_proto::PROTOCOL_VERSION;

    fn endpoint(tag: &str) -> Endpoint {
        let dir = std::env::temp_dir().join(format!(
            "nysia-discovery-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Endpoint::resolve(
            PROTOCOL_VERSION,
            EnvSource {
                runtime_dir_override: Some(dir),
                endpoint_override: None,
                home: None,
                xdg_runtime_dir: None,
                account: "nysia-test".to_owned(),
                isolated: true,
            },
        )
        .expect("an endpoint resolves")
    }

    fn client_id() -> ClientId {
        "nysia-test".parse().expect("a well-formed client id")
    }

    #[tokio::test]
    async fn nothing_listening_and_no_permission_to_spawn_says_how_to_start_one() {
        let endpoint = endpoint("absent");
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

    #[tokio::test]
    async fn only_one_spawner_holds_the_lock_and_it_is_released_when_dropped() {
        let endpoint = endpoint("lock");
        let held = SpawnLock::acquire(&endpoint).await.expect("takes the lock");
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
        let endpoint = endpoint("stale");
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
}
