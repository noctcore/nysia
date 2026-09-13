//! The adoption lease: the pid record written beside the socket, and what reading it proves.
//!
//! §3.1 asks for `pid` + `startedAtMs` + `launchNonce` in a file beside the socket, "so a
//! restarted app can verify the daemon it found is the one it thinks it is". `nysia-proto`
//! owns the [`PidRecord`] shape; this module owns the file.
//!
//! # What the lease is not
//!
//! It is **not** a liveness check. `kill(pid, 0)` says a process with that number exists,
//! which after a reboot or a busy hour is very often a different process — pid reuse is the
//! failure the launch nonce exists to prevent, and re-introducing it by checking the pid
//! first would defeat the nonce entirely.
//!
//! Liveness is therefore always the same three steps, in this order, and
//! [`crate::rpc::discovery`] is the only place that performs them:
//!
//! 1. **connect** to the endpoint;
//! 2. **`hello`**, and read back the daemon's [`DaemonIdentity`];
//! 3. **[`PidRecord::describes`]** that identity.
//!
//! A connect that fails means the daemon is gone whatever the file says. A connect that
//! succeeds but whose identity the record does not describe means the file is stale and a
//! *different* daemon now owns the endpoint — which is a real state, reachable by a crash
//! and an immediate restart, and the one where trusting the file silently attaches a client
//! to the wrong runtime.

use std::io::Write;
use std::path::{Path, PathBuf};

use nysia_proto::{DaemonIdentity, PidRecord};

/// Why the lease could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The file could not be read or written.
    #[error("could not {action} the daemon lease at {}: {source}", path.display())]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// Which file.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The file existed but did not hold a readable record.
    #[error("the daemon lease at {} is not a readable pid record: {source}", path.display())]
    Corrupt {
        /// Which file.
        path: PathBuf,
        /// What serde made of it.
        #[source]
        source: serde_json::Error,
    },
}

/// The lease file beside the endpoint.
#[derive(Debug, Clone)]
pub struct PidRecordFile {
    path: PathBuf,
}

impl PidRecordFile {
    /// The lease stored at `path`.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the lease lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the record, or `None` when there is no file.
    ///
    /// A **corrupt** file is an error rather than a `None`. The two want opposite responses:
    /// nothing there means "no daemon has run here, start one", while half a JSON object
    /// means something wrote it and was interrupted, and quietly overwriting that is how a
    /// second daemon gets started on top of a live one.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Io`] when the file exists but cannot be read, and
    /// [`LeaseError::Corrupt`] when it is not a readable record.
    pub fn read(&self) -> Result<Option<PidRecord>, LeaseError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(LeaseError::Io {
                    action: "read",
                    path: self.path.clone(),
                    source,
                });
            }
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| LeaseError::Corrupt {
                path: self.path.clone(),
                source,
            })
    }

    /// Write the record for `identity`, atomically and owner-only.
    ///
    /// Written to a sibling temporary file and renamed, so a reader never sees a half-file:
    /// a truncate-then-write leaves exactly the corrupt state [`PidRecordFile::read`] has to
    /// treat as fatal, and leaves it during the one moment a racing client is most likely to
    /// look.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Io`] when the file cannot be written or renamed into place.
    pub fn write(&self, identity: &DaemonIdentity) -> Result<(), LeaseError> {
        let io = |action: &'static str| {
            let path = self.path.clone();
            move |source| LeaseError::Io {
                action,
                path: path.clone(),
                source,
            }
        };
        let record = identity.pid_record();
        let mut text = serde_json::to_string_pretty(&record).map_err(|source| LeaseError::Io {
            action: "encode",
            path: self.path.clone(),
            source: std::io::Error::other(source),
        })?;
        text.push('\n');

        // The temporary name carries this process's pid, so two daemons writing leases in
        // one directory — which the endpoint override makes possible — cannot truncate each
        // other's half-written file.
        let temp = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        let mut file = open_owner_only(&temp).map_err(io("write"))?;
        file.write_all(text.as_bytes()).map_err(io("write"))?;
        file.sync_all().map_err(io("write"))?;
        drop(file);

        // Windows `rename` refuses to replace an existing file, so the old lease goes first.
        // The window this opens is harmless: a reader that finds nothing retries, and a
        // reader that finds the old record still has to connect and compare before trusting
        // it.
        #[cfg(windows)]
        let _ = std::fs::remove_file(&self.path);

        std::fs::rename(&temp, &self.path).map_err(io("rename"))
    }

    /// Remove the lease, ignoring an absent file.
    ///
    /// Called on a clean shutdown. A daemon that crashed leaves the lease behind, which is
    /// exactly why nothing may treat its presence as proof of life.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Create `path` for writing, readable and writable by its owner alone.
///
/// Scrollback-adjacent files are owner-only (trap 14), and the lease is the file that says
/// which pipe to talk to — a world-readable one is an invitation to attach.
fn open_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nysia_proto::LaunchNonce;

    fn identity(pid: u32) -> DaemonIdentity {
        DaemonIdentity {
            pid,
            started_at_ms: 1_757_721_600_000,
            launch_nonce: LaunchNonce::generate(),
            app_version: "0.1.0".to_owned(),
        }
    }

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nysia-lease-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("nysiad-v1.pid.json")
    }

    #[test]
    fn an_absent_lease_reads_as_nothing_rather_than_as_an_error() {
        let lease = PidRecordFile::at(temp_path("absent"));
        assert!(lease.read().expect("absent is not a failure").is_none());
    }

    #[test]
    fn a_written_lease_describes_the_daemon_that_wrote_it_and_nothing_else() {
        let path = temp_path("describes");
        let lease = PidRecordFile::at(&path);
        let mine = identity(4242);
        lease.write(&mine).expect("writes");

        let record = lease.read().expect("reads").expect("is there");
        assert!(record.describes(&mine));

        // Same pid, same start time, different daemon: the nonce is what catches it, and it
        // is the case pid reuse produces.
        let impostor = DaemonIdentity {
            launch_nonce: LaunchNonce::generate(),
            ..mine.clone()
        };
        assert!(!record.describes(&impostor));

        // Same nonce, recycled pid: all three fields have to agree.
        let renumbered = DaemonIdentity { pid: 4243, ..mine };
        assert!(!record.describes(&renumbered));
        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));
    }

    #[test]
    fn a_corrupt_lease_is_an_error_and_not_an_absent_one() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{\"pid\": 12").expect("writes half a record");
        let lease = PidRecordFile::at(&path);
        assert!(matches!(lease.read(), Err(LeaseError::Corrupt { .. })));
        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));
    }

    #[test]
    fn a_rewritten_lease_replaces_the_previous_one_whole() {
        let path = temp_path("rewrite");
        let lease = PidRecordFile::at(&path);
        let first = identity(1);
        let second = identity(2);
        lease.write(&first).expect("writes");
        lease.write(&second).expect("rewrites");

        let record = lease.read().expect("reads").expect("is there");
        assert!(record.describes(&second));
        assert!(!record.describes(&first));

        lease.remove();
        assert!(lease.read().expect("reads").is_none());
        // Removing twice is a no-op, which is what lets shutdown be unconditional.
        lease.remove();
        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));
    }
}
