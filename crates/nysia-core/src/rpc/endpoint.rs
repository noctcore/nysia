//! Where the daemon listens, and where the files that describe it live.
//!
//! §3.1 puts the protocol version in the *name*: `nysiad-v<protocol>.sock` on Unix,
//! `\\.\pipe\nysiad-v<protocol>-<user>` on Windows. `nysia-proto` composes those names and does no
//! IO; this module is the other half — it decides which directory on Unix and which account
//! on Windows, and it is the only place in the daemon that touches either.
//!
//! Three files live beside each other in the runtime directory:
//!
//! | File | What it is |
//! |---|---|
//! | `nysiad-v<protocol>.sock` | the Unix socket. On Windows the endpoint is a pipe and has no file. |
//! | `nysiad-v<protocol>.pid.json` | the adoption lease ([`crate::rpc::PidRecordFile`]). |
//! | `nysiad-v<protocol>.lock` | held while a daemon is binding, so two spawners cannot both win. |
//!
//! # Overrides
//!
//! `NYSIA_RUNTIME_DIR` moves all three. It exists for tests and for a second daemon on one
//! machine, and it moves the *Windows* endpoint too: a named pipe lives in a machine-global
//! namespace, so isolating the directory alone would leave two "isolated" daemons fighting
//! over one pipe. The account half of the pipe name therefore carries a short digest of the
//! directory whenever the override is set.
//!
//! `NYSIA_ENDPOINT` replaces the endpoint outright — a full socket path or a full
//! `\\.\pipe\…` name — and is the escape hatch for a layout neither default anticipates.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use nysia_proto::{EndpointError, PROTOCOL_VERSION, ProtocolVersion, endpoint_stem};

/// The environment variable that moves the runtime directory.
pub const RUNTIME_DIR_VAR: &str = "NYSIA_RUNTIME_DIR";

/// The environment variable that replaces the endpoint outright.
pub const ENDPOINT_VAR: &str = "NYSIA_ENDPOINT";

/// macOS caps `sun_path` at 104 *bytes including the terminating NUL*, so 103 is the longest
/// path that fits.
///
/// Worth refusing rather than discovering: a path that overruns it is truncated by some
/// kernels rather than refused, and then the bind succeeds while every client looks somewhere
/// else. Rust's own `SocketAddr::from_pathname` errors at `len >= 104` for the same reason,
/// which is the boundary this matches.
const UNIX_SOCKET_PATH_MAX: usize = 103;

/// Why an endpoint could not be resolved.
#[derive(Debug, thiserror::Error)]
pub enum EndpointResolveError {
    /// No directory could be chosen for the socket, the lease and the lock.
    #[error(
        "could not decide where the nysia runtime directory belongs: {reason}; set \
         NYSIA_RUNTIME_DIR to an absolute path you own"
    )]
    RuntimeDir {
        /// What went wrong.
        reason: String,
    },
    /// The runtime directory could not be created.
    #[error("could not create the nysia runtime directory {}: {source}", path.display())]
    CreateRuntimeDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The composed socket path would be truncated by the kernel.
    #[error(
        "the unix socket path {} is {actual} bytes and the kernel allows 103; set \
         NYSIA_RUNTIME_DIR to a shorter directory",
        path.display()
    )]
    SocketPathTooLong {
        /// The path that would not fit.
        path: PathBuf,
        /// How long it came out.
        actual: usize,
    },
    /// The Windows pipe name could not be composed.
    #[error("could not compose the named pipe: {0}")]
    PipeName(#[from] EndpointError),
    /// `NYSIA_ENDPOINT` held something this platform cannot listen on.
    #[error("NYSIA_ENDPOINT is set to {value:?}, which is not {expected}")]
    EndpointOverride {
        /// What the variable held.
        value: String,
        /// What it should have held.
        expected: &'static str,
    },
}

/// Where the daemon listens.
///
/// One variant per platform rather than a `PathBuf` that means different things: a Windows
/// pipe name is not a filesystem path, and treating it as one is how a `CreateFile` ends up
/// making a file literally called `\\.\pipe\nysiad-v<protocol>-kacpe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listening {
    /// A Unix domain socket at this path.
    UnixSocket(PathBuf),
    /// A Win32 named pipe with this full name, `\\.\pipe\` included.
    NamedPipe(String),
}

impl fmt::Display for Listening {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnixSocket(path) => write!(f, "{}", path.display()),
            Self::NamedPipe(name) => f.write_str(name),
        }
    }
}

/// The endpoint a daemon binds and a client dials, plus the files beside it.
///
/// Resolved identically by both halves — that is the point of a single type. A client that
/// computed the socket path with its own `format!` is one refactor away from dialling a
/// daemon that is listening somewhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    listening: Listening,
    runtime_dir: PathBuf,
    stem: String,
}

impl Endpoint {
    /// Resolve the endpoint for [`PROTOCOL_VERSION`] from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointResolveError`] when no runtime directory can be chosen or created,
    /// when the composed name does not fit what the platform allows, or when
    /// `NYSIA_ENDPOINT` holds something this platform cannot listen on.
    pub fn from_env() -> Result<Self, EndpointResolveError> {
        Self::resolve(PROTOCOL_VERSION, EnvSource::process())
    }

    /// Resolve the endpoint for `version` against an explicit environment.
    ///
    /// Split out from [`Endpoint::from_env`] so the resolution rules can be tested without
    /// mutating the process environment, which no test can do safely while another test
    /// reads it.
    ///
    /// # Errors
    ///
    /// As [`Endpoint::from_env`].
    pub fn resolve(version: ProtocolVersion, env: EnvSource) -> Result<Self, EndpointResolveError> {
        let stem = endpoint_stem(version);
        let runtime_dir = env.runtime_dir()?;
        std::fs::create_dir_all(&runtime_dir).map_err(|source| {
            EndpointResolveError::CreateRuntimeDir {
                path: runtime_dir.clone(),
                source,
            }
        })?;
        restrict_to_owner(&runtime_dir);

        let listening = match env.endpoint_override.as_deref() {
            Some(raw) => explicit_endpoint(raw)?,
            None => default_endpoint(version, &stem, &runtime_dir, env.isolated, &env.account)?,
        };
        if let Listening::UnixSocket(path) = &listening {
            let actual = path.as_os_str().as_encoded_bytes().len();
            if actual > UNIX_SOCKET_PATH_MAX {
                return Err(EndpointResolveError::SocketPathTooLong {
                    path: path.clone(),
                    actual,
                });
            }
        }
        Ok(Self {
            listening,
            runtime_dir,
            stem,
        })
    }

    /// Where the daemon listens.
    #[must_use]
    pub fn listening(&self) -> &Listening {
        &self.listening
    }

    /// The directory holding the lease, the lock and — on Unix — the socket.
    #[must_use]
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// The adoption lease's path: `<runtime dir>/nysiad-v<protocol>.pid.json`.
    #[must_use]
    pub fn pid_record_path(&self) -> PathBuf {
        self.runtime_dir.join(format!("{}.pid.json", self.stem))
    }

    /// The spawn lock's path: `<runtime dir>/nysiad-v<protocol>.lock`.
    ///
    /// Held while a daemon binds, so two clients racing to spawn one cannot both believe
    /// they won (§12 Q5).
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.runtime_dir.join(format!("{}.lock", self.stem))
    }

    /// Where a spawned daemon's diagnostics go.
    ///
    /// A daemon that inherits its spawner's stdio keeps that pipe open forever, so a CLI
    /// that spawned one and then read its own output would never see it close. This file is
    /// where the output goes instead of nowhere.
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.runtime_dir.join(format!("{}.log", self.stem))
    }

    /// Where the *window's* diagnostics go: `<runtime dir>/<stem>.window.log`.
    ///
    /// Beside [`Self::log_path`] rather than inside it, and that is a decision rather than a
    /// convenience. Two processes appending to one file interleave at whatever granularity
    /// the formatter happens to write in, and the resulting file cannot say which process
    /// wrote a line — so the streams are kept apart and correlated by what they both already
    /// carry: a `PaneKey`, a `SessionHandle`, an `Incarnation` and a `StreamId`.
    ///
    /// Under `windows_subsystem = "windows"` a packaged window has no stderr at all, so this
    /// file is the only record its Rust side leaves anywhere.
    ///
    /// Two windows on one daemon share this file. They interleave, and a rotation racing
    /// between them loses the lines written during the other's copy — see
    /// [`crate::rpc::log_file`] for the trade. One window is the ordinary case and the file
    /// is diagnostic, so that is accepted rather than defended against.
    #[must_use]
    pub fn window_log_path(&self) -> PathBuf {
        self.runtime_dir.join(format!("{}.window.log", self.stem))
    }
}

/// Compose the platform's default endpoint.
fn default_endpoint(
    version: ProtocolVersion,
    stem: &str,
    runtime_dir: &Path,
    isolated: bool,
    account: &str,
) -> Result<Listening, EndpointResolveError> {
    if cfg!(windows) {
        // A named pipe lives in a machine-global namespace. `NYSIA_RUNTIME_DIR` therefore
        // has to reach the *name*, or two daemons pointed at different directories would
        // still collide on one pipe — which is exactly what happens when two integration
        // tests run in parallel.
        let account = if isolated {
            format!("{account}-{}", short_digest(runtime_dir.as_os_str()))
        } else {
            account.to_owned()
        };
        Ok(Listening::NamedPipe(nysia_proto::windows_pipe_name(
            version, &account,
        )?))
    } else {
        Ok(Listening::UnixSocket(
            runtime_dir.join(format!("{stem}.sock")),
        ))
    }
}

/// Read `NYSIA_ENDPOINT` as this platform's endpoint.
fn explicit_endpoint(raw: &str) -> Result<Listening, EndpointResolveError> {
    let refuse = |expected| EndpointResolveError::EndpointOverride {
        value: raw.to_owned(),
        expected,
    };
    if cfg!(windows) {
        if !raw.starts_with(r"\\.\pipe\") {
            return Err(refuse(r"a named pipe beginning with \\.\pipe\"));
        }
        Ok(Listening::NamedPipe(raw.to_owned()))
    } else {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(refuse("an absolute unix socket path"));
        }
        Ok(Listening::UnixSocket(path))
    }
}

/// The environment the resolution rules read.
///
/// A struct rather than direct `env::var` calls inside the rules, because the rules are
/// worth testing and `env::set_var` is unsafe to call while another thread reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSource {
    /// `NYSIA_RUNTIME_DIR`, when set.
    pub runtime_dir_override: Option<PathBuf>,
    /// `NYSIA_ENDPOINT`, when set.
    pub endpoint_override: Option<String>,
    /// The home directory the default layout hangs off.
    pub home: Option<PathBuf>,
    /// `XDG_RUNTIME_DIR`, when set. Ignored on Windows.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// The account name the Windows pipe is named for.
    pub account: String,
    /// Whether the runtime directory was overridden, which is what makes the Windows pipe
    /// name isolated too.
    pub isolated: bool,
}

impl EnvSource {
    /// Read the current process environment.
    #[must_use]
    pub fn process() -> Self {
        let runtime_dir_override = non_empty(RUNTIME_DIR_VAR).map(PathBuf::from);
        Self {
            isolated: runtime_dir_override.is_some(),
            runtime_dir_override,
            endpoint_override: non_empty(ENDPOINT_VAR),
            home: non_empty("HOME")
                .or_else(|| non_empty("USERPROFILE"))
                .or_else(|| non_empty("LOCALAPPDATA"))
                .map(PathBuf::from),
            xdg_runtime_dir: non_empty("XDG_RUNTIME_DIR").map(PathBuf::from),
            account: sanitize_account(
                &non_empty("USERNAME")
                    .or_else(|| non_empty("USER"))
                    .unwrap_or_default(),
            ),
        }
    }

    /// Where the lease, the lock and the socket belong.
    fn runtime_dir(&self) -> Result<PathBuf, EndpointResolveError> {
        if let Some(explicit) = &self.runtime_dir_override {
            return Ok(explicit.clone());
        }
        // `XDG_RUNTIME_DIR` first where it exists, because it is already per-user, already
        // 0700 and already cleaned at logout. macOS does not set it, and a socket under
        // `$TMPDIR` there is a long path against a 104-byte ceiling — so `~/.nysia/run` is
        // the fallback rather than the temporary directory.
        if !cfg!(windows)
            && let Some(xdg) = &self.xdg_runtime_dir
        {
            return Ok(xdg.join("nysia"));
        }
        let home = self
            .home
            .as_ref()
            .ok_or_else(|| EndpointResolveError::RuntimeDir {
                reason: "neither HOME nor USERPROFILE nor LOCALAPPDATA is set".to_owned(),
            })?;
        Ok(home.join(".nysia").join("run"))
    }
}

/// An environment variable, treating empty as unset.
fn non_empty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

/// Reduce an account name to something `nysia_proto::windows_pipe_name` accepts.
///
/// `GetUserNameEx` and `USERNAME` can both hand back a `DOMAIN\user` string, and splicing
/// that into a pipe name would silently name a pipe in a different namespace — which proto
/// refuses outright. Taking the account half and replacing anything else keeps a pipe name
/// composable for every login this machine has, rather than failing on the ones with a
/// space in them.
fn sanitize_account(raw: &str) -> String {
    let account = raw.rsplit(['\\', '/']).next().unwrap_or(raw);
    let cleaned: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "nysia".to_owned()
    } else {
        cleaned
    }
}

/// A short, stable, name-safe digest of `value`.
///
/// FNV-1a rather than a hash crate: this is a name suffix that keeps two isolated daemons
/// apart, not a security boundary, and the workspace dependency table is coordinator-owned.
fn short_digest(value: &OsStr) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Narrow a directory to its owner where the platform has a cheap way to say so.
///
/// Best effort by design: the socket, the lease and the log can all carry scrollback-shaped
/// information (trap 14), and a directory one mode bit wider than it should be is worth
/// fixing — but a daemon that refuses to start because a `chmod` failed on a filesystem with
/// no modes is worse than one that starts.
fn restrict_to_owner(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the profile directory's ACL, which is already owner-only for
        // everything under `%USERPROFILE%` and `%LOCALAPPDATA%`.
        let _ = dir;
    }
}

/// An isolated endpoint for a test, on a path short enough for every platform.
///
/// One helper rather than one per module, because the constraint is easy to violate by
/// accident and expensive to discover: macOS caps a socket path at 103 bytes and its `TMPDIR`
/// is already ~49 of them, so `temp_dir().join("nysia-<module>-<tag>-<pid>-<thread>")` plus
/// `/nysiad-v<protocol>.sock` overruns the cap and every test in the module fails to resolve an
/// endpoint at all.
///
/// The name is therefore a short digest rather than anything readable. On Unix it hangs off
/// `/tmp` — which is where the shortest writable path is, and on macOS is a symlink to
/// `/private/tmp` that the kernel resolves for us.
#[cfg(test)]
#[must_use]
pub(crate) fn scratch(tag: &str) -> Endpoint {
    let name = format!(
        "nys-{}",
        short_digest(std::ffi::OsStr::new(&format!(
            "{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )))
    );
    let base = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let runtime_dir = base.join(name);
    let _ = std::fs::remove_dir_all(&runtime_dir);
    let source = EnvSource {
        runtime_dir_override: Some(runtime_dir),
        endpoint_override: None,
        home: None,
        xdg_runtime_dir: None,
        account: "nysia-test".to_owned(),
        isolated: true,
    };
    match Endpoint::resolve(PROTOCOL_VERSION, source) {
        Ok(endpoint) => endpoint,
        Err(err) => panic!("a scratch endpoint should always resolve: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(dir: &Path) -> EnvSource {
        EnvSource {
            runtime_dir_override: Some(dir.to_path_buf()),
            endpoint_override: None,
            home: Some(PathBuf::from("/home/kacpe")),
            xdg_runtime_dir: None,
            account: "kacpe".to_owned(),
            isolated: true,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nysia-endpoint-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_runtime_directory_holds_the_lease_the_lock_and_the_log() {
        let dir = temp_dir("layout");
        let endpoint = Endpoint::resolve(PROTOCOL_VERSION, source(&dir)).expect("resolves");
        assert_eq!(endpoint.runtime_dir(), dir);
        // Derived from the version rather than written out, because §3.1's whole mechanism is
        // that these names *move* when the protocol does — a literal here asserts that this
        // build is v1, which is a different and much less useful claim.
        let stem = nysia_proto::endpoint_stem(PROTOCOL_VERSION);
        assert_eq!(
            endpoint.pid_record_path(),
            dir.join(format!("{stem}.pid.json"))
        );
        assert_eq!(endpoint.lock_path(), dir.join(format!("{stem}.lock")));
        assert_eq!(endpoint.log_path(), dir.join(format!("{stem}.log")));
        assert_eq!(
            endpoint.window_log_path(),
            dir.join(format!("{stem}.window.log"))
        );
        assert_ne!(
            endpoint.window_log_path(),
            endpoint.log_path(),
            "the window and the daemon would interleave into one file"
        );
        // And they are versioned at all, which is the property the names carry.
        assert!(stem.starts_with("nysiad-v"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_isolated_runtime_directory_isolates_the_windows_pipe_too() {
        // A named pipe is machine-global, so two directories that are isolated from each
        // other must not resolve to one pipe. On Unix the directory *is* the isolation.
        let one = temp_dir("iso-one");
        let two = temp_dir("iso-two");
        let first = Endpoint::resolve(PROTOCOL_VERSION, source(&one)).expect("resolves");
        let second = Endpoint::resolve(PROTOCOL_VERSION, source(&two)).expect("resolves");
        assert_ne!(first.listening(), second.listening());
        let _ = std::fs::remove_dir_all(&one);
        let _ = std::fs::remove_dir_all(&two);
    }

    #[test]
    fn a_domain_qualified_account_is_reduced_to_its_account_half() {
        assert_eq!(sanitize_account(r"CORP\kacpe"), "kacpe");
        assert_eq!(sanitize_account("kac pe"), "kac_pe");
        assert_eq!(sanitize_account(""), "nysia");
        // Whatever comes out has to be something proto will actually compose.
        for raw in [r"CORP\kac pe", "", "ünïcode", "a/b\\c"] {
            assert!(
                nysia_proto::windows_pipe_name(PROTOCOL_VERSION, &sanitize_account(raw)).is_ok(),
                "{raw:?} should sanitise to a composable account"
            );
        }
    }

    #[test]
    fn an_endpoint_override_must_be_shaped_for_this_platform() {
        let dir = temp_dir("override");
        let wrong = EnvSource {
            endpoint_override: Some("not-an-endpoint".to_owned()),
            ..source(&dir)
        };
        assert!(matches!(
            Endpoint::resolve(PROTOCOL_VERSION, wrong),
            Err(EndpointResolveError::EndpointOverride { .. })
        ));

        let right = EnvSource {
            endpoint_override: Some(if cfg!(windows) {
                r"\\.\pipe\nysiad-v1-test".to_owned()
            } else {
                "/tmp/nysiad-v1-test.sock".to_owned()
            }),
            ..source(&dir)
        };
        let endpoint = Endpoint::resolve(PROTOCOL_VERSION, right).expect("resolves");
        assert!(endpoint.listening().to_string().contains("test"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scratch_endpoint_fits_on_every_platform() {
        // The constraint that is easy to violate by accident: macOS caps a socket path at 103
        // bytes and its TMPDIR is already about half of that.
        let endpoint = scratch("a-tag-long-enough-to-have-been-a-problem");
        if let Listening::UnixSocket(path) = endpoint.listening() {
            let bytes = path.as_os_str().as_encoded_bytes().len();
            assert!(
                bytes <= UNIX_SOCKET_PATH_MAX,
                "{} is {bytes} bytes",
                path.display()
            );
        }
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[test]
    fn a_socket_path_the_kernel_would_truncate_is_refused() {
        if cfg!(windows) {
            return;
        }
        let long = std::env::temp_dir().join("n".repeat(UNIX_SOCKET_PATH_MAX));
        assert!(matches!(
            Endpoint::resolve(PROTOCOL_VERSION, source(&long)),
            Err(EndpointResolveError::SocketPathTooLong { .. })
                | Err(EndpointResolveError::CreateRuntimeDir { .. })
        ));
        let _ = std::fs::remove_dir_all(&long);
    }
}
