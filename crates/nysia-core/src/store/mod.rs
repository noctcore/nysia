//! Persistent state: SQLite for anything queried, JSON for anything hand-edited.
//!
//! D-12. Sessions, worktrees, orchestration, agent status and the scrollback index go into
//! a WAL-mode SQLite database, because orchestration needs indexes and transactions.
//! Settings and the project list stay JSON, written atomically, because a human is
//! expected to open them in an editor.
//!
//! There is no local task domain model: tasks are GitHub Issues, queried live (D-5).
//!
//! # What is here
//!
//! The first table, and the runner that will carry the rest:
//!
//! | Module | What it owns |
//! |---|---|
//! | [`migrate`] | the versioned schema, applied forward once, in a transaction |
//! | [`status`] | the §2.2 agent-status row, its history cap and its rehydration |
//! | [`error`] | one error type, every variant carrying the path it failed on |
//!
//! Scrollback, worktrees and orchestration are later waves and later migrations. Nothing
//! here reads or writes the disk spool: that is `nysia hook`'s and the daemon's (W4). The
//! store persists what the drain hands it, through [`Store::restore_status`].
//!
//! # It is synchronous, and that is deliberate
//!
//! Every method here blocks. The daemon calls them from `spawn_blocking`, which is what
//! `rusqlite` wants — it is a C library with a blocking API, and wrapping it in an async
//! façade would buy an `await` in front of a call that still occupies a thread. [`Store`] is
//! `Send + Sync`, so an `Arc<Store>` is the shape a caller wants; a const assertion at the
//! bottom of this file holds that, because discovering it in W4 is discovering it too late.
//!
//! # Trap 14: the database file is owner-only
//!
//! A `waiting` row carries a tool's input verbatim, and a tool's input can be a secret the
//! same way scrollback can. (The architecture doc's register numbers this 14; `CLAUDE.md`
//! §5 numbers the same trap 13.) Two things follow, and both are load-bearing:
//!
//! - The file is created `0600` **before SQLite opens it**, so the `-wal` and `-shm`
//!   sidecars — which SQLite creates with the mode of the main database — are owner-only
//!   too. Creating it afterwards would leave the WAL, which holds the most recent rows,
//!   wider than the database it belongs to.
//! - The path is refused if it is a **symbolic link**. `create_new` cannot tell a planted
//!   link from an existing database, and following one would chmod `0600` onto the link's
//!   target and open that target as the store. See [`refuse_symlink`], which also says what
//!   the check does not cover.
//! - `question` reaches the disk **only on a `waiting` row**. `nysia-proto` already confines
//!   `tool_input` to `waiting` on the wire; [`status`] applies the same rule on the way in,
//!   on the one private path both entry points share, so a row built in process rather than
//!   received over the socket cannot widen it.
//!
//! # Proving the durability rules trip
//!
//! Traps register #13 — every gate ships a proof that it trips. Each rule below has a named
//! test, and each was **run against the mutation beside it** rather than asserted to be
//! capable of failing. To repeat one: apply the edit, run
//! `cargo test -p nysia-core <module>::tests::<name>` from the crate directory, revert. The
//! module is `store::status::tests` for the rows naming `status::`, and `store::tests` for the
//! rest — including the `migrate::apply` row, whose test lives with the other `Store::open`
//! tests rather than beside the code it mutates.
//!
//! | Mutate | To | Turns red |
//! |---|---|---|
//! | `status::trim`'s cap parameter | `i64::from(AGENT_STATUS_HISTORY_CAP) + 1` | `the_history_stops_at_the_cap` |
//! | `status::insert`'s timestamp | `row.observed_at.get() / 1000` | `a_row_at_the_staleness_boundary_reads_back_exact` |
//! | `Store::restore_status`'s provenance | `Provenance::Live` | `a_restored_row_is_unconfirmed_whatever_the_caller_said` |
//! | `status::insert`'s question filter | dropped | `only_a_waiting_row_keeps_its_question` |
//! | `status::live_row_at_least_as_recent`'s `restored_unconfirmed = 0` | dropped | `an_out_of_order_drain_still_lands` |
//! | `migrate::apply`'s `migration.version <= found` guard | dropped | `opening_a_current_store_takes_no_write_lock` |
//! | `Store::open`'s `refuse_symlink` call | dropped | `a_symlinked_database_path_is_refused` (unix) |
//!
//! The tests assert **literal** numbers — twenty rows, `1_800_000` milliseconds — rather
//! than recomputing them from the constants they are checking. A test that derives its
//! expectation from the value under test passes whatever that value becomes, which is the
//! shape of a gate that cannot trip. `the_cap_is_the_plans_twenty` and
//! `the_staleness_threshold_is_the_plans_thirty_minutes` hold those literals against
//! `nysia-proto`'s constants in the other direction, so the two cannot drift apart quietly.

mod error;
mod migrate;
mod project;
mod status;

#[cfg(test)]
mod tests_support;

pub use error::StoreError;
pub use project::{Forgotten, Registered, Registration, StoredProject};
pub use status::Restored;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

/// How long a writer waits for another writer before giving up.
///
/// WAL admits any number of readers alongside one writer, but writers still serialise, and
/// SQLite's default on contention is to fail the statement immediately with
/// `SQLITE_BUSY` — not to wait. A daemon and a `nysia` CLI process writing status in the
/// same millisecond is ordinary, so without this the loser's status is simply **lost**.
/// Five seconds is far longer than any statement here, which means hitting it is a deadlock
/// to investigate rather than a slow disk to wait out.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times to try switching a fresh database into WAL, and how long to wait between.
///
/// Together they bound the wait at half a second, which is the same order as [`BUSY_TIMEOUT`]
/// and far longer than the exclusive lock a WAL switch actually holds. See [`ensure_wal`] for
/// why this is a loop rather than a busy timeout.
const WAL_ATTEMPTS: u32 = 25;
const WAL_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// The daemon's SQLite database.
///
/// One connection behind a mutex. `rusqlite::Connection` is `Send` and not `Sync`, and the
/// alternative — a pool — would buy parallel writers that SQLite serialises anyway. Readers
/// that genuinely need to run alongside a write open their **own** [`Store`] on the same
/// path, which is what WAL is for and what `two_writers_racing_the_cap_leave_exactly_the_cap`
/// exercises.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    /// Open, or create, the database at `path`, and bring its schema up to date.
    ///
    /// Creates the parent directory if it is missing. Safe to call concurrently on the same
    /// path from more than one process: for a step that is not yet in place the migration
    /// runner takes the write lock before it reads the version, so the loser applies nothing
    /// rather than failing. A database already at the newest schema has no such step, and so
    /// takes no write lock and never waits for one.
    ///
    /// # Errors
    ///
    /// - [`StoreError::Io`] if the directory or the file cannot be created.
    /// - [`StoreError::Symlink`] if `path` is a symbolic link, which is refused rather than
    ///   followed.
    /// - [`StoreError::NotWal`] if the filesystem will not give SQLite a WAL.
    /// - [`StoreError::SchemaAhead`] if a newer build of Nysia wrote this database.
    /// - [`StoreError::Migration`] if a migration fails; it is rolled back.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let io = |action: &'static str, path: &Path| {
            let path = path.to_path_buf();
            move |source| StoreError::Io {
                action,
                path,
                source,
            }
        };

        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            // Only narrow a directory this call brought into existence. Tightening one the
            // person already had — which may be a home directory — is not a store's business.
            let ours = !dir.exists();
            std::fs::create_dir_all(dir).map_err(io("create the directory for", dir))?;
            if ours {
                restrict_dir_to_owner(dir);
            }
        }

        // Before `Connection::open`, so the `-wal` and `-shm` SQLite is about to create
        // inherit the mode rather than a default one.
        create_owner_only(&path).map_err(io("create", &path))?;
        refuse_symlink(&path)?;
        restrict_to_owner(&path);

        let mut conn = Connection::open(&path).map_err(|source| StoreError::Sqlite {
            action: "open the database",
            path: path.clone(),
            source,
        })?;
        configure(&conn, &path)?;
        migrate::apply(&mut conn, migrate::MIGRATIONS, &path)?;

        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    /// Where this database lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The schema version on disk.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the header cannot be read.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let conn = self.conn()?;
        migrate::user_version(&conn, &self.path)
    }

    /// The connection, or [`StoreError::Poisoned`] if a panic left it locked.
    fn conn(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }
}

/// Open a write transaction that takes the write lock immediately.
///
/// `IMMEDIATE` on every writer in this module, and it is not a precaution. A deferred
/// transaction starts as a reader and only takes the write lock at its first write, so a
/// read-then-write — which every entry point here is — fails with `SQLITE_BUSY_SNAPSHOT` if
/// another writer committed in between. The busy timeout does not retry that one, because no
/// amount of waiting refreshes a snapshot that is already stale.
///
/// `action` is what the error says was being attempted. It is a parameter rather than one
/// wording for every table because the path in the message names the **database**, and a
/// message that stops there tells a reader which file failed and nothing about what was
/// being written to it.
pub(super) fn begin<'a>(
    conn: &'a mut Connection,
    path: &Path,
    action: &'static str,
) -> Result<rusqlite::Transaction<'a>, StoreError> {
    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|source| StoreError::Sqlite {
            action,
            path: path.to_path_buf(),
            source,
        })
}

/// Commit, reporting the path if it fails.
pub(super) fn commit(
    tx: rusqlite::Transaction<'_>,
    path: &Path,
    action: &'static str,
) -> Result<(), StoreError> {
    tx.commit().map_err(|source| StoreError::Sqlite {
        action,
        path: path.to_path_buf(),
        source,
    })
}

/// Put the connection into the mode D-12 asks for, and verify it got there.
fn configure(conn: &Connection, path: &Path) -> Result<(), StoreError> {
    let sqlite = |action: &'static str| {
        let path = path.to_path_buf();
        move |source| StoreError::Sqlite {
            action,
            path,
            source,
        }
    };

    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(sqlite("set the busy timeout"))?;
    ensure_wal(conn, path)?;

    // `NORMAL` is the setting WAL is designed around: a commit is durable against the
    // process dying, and only a power cut or a kernel panic can cost the most recent
    // transactions. `FULL` would fsync on every status row — one per tool call, on the
    // agent's critical path — to protect against a case §2.3 already answers, since a status
    // lost to a power cut is exactly what a `restored_unconfirmed` row is for.
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(sqlite("set synchronous"))?;

    // No foreign keys today. Set anyway, because SQLite defaults it *off* per connection,
    // and a later migration adding a reference would otherwise declare a constraint that is
    // never enforced — which looks exactly like one that is.
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(sqlite("enable foreign keys"))?;

    Ok(())
}

/// Put the database into WAL, retrying while another process is doing the same.
///
/// Two things make this more than one `PRAGMA`.
///
/// **The mode is read first.** `journal_mode` lives in the database header and persists, so
/// every open after the first finds WAL already set. Reading is a shared operation; assigning
/// is not, so skipping the assignment is skipping the contention.
///
/// **The assignment is retried.** Switching *into* WAL needs a brief exclusive lock, and
/// SQLite answers `SQLITE_BUSY` for it **without consulting the busy handler** — so
/// [`BUSY_TIMEOUT`] does not cover this one statement, and two daemons opening the same fresh
/// database lose a coin toss rather than queue. `two_daemons_opening_a_fresh_store_both_succeed`
/// is that race, and it failed before this loop existed.
fn ensure_wal(conn: &Connection, path: &Path) -> Result<(), StoreError> {
    let sqlite = |action: &'static str| {
        let path = path.to_path_buf();
        move |source| StoreError::Sqlite {
            action,
            path,
            source,
        }
    };
    let in_wal = |conn: &Connection| -> Result<bool, StoreError> {
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(sqlite("read the journal mode"))?;
        Ok(mode.eq_ignore_ascii_case("wal"))
    };

    for _ in 0..WAL_ATTEMPTS {
        if in_wal(conn)? {
            return Ok(());
        }
        // The pragma answers with the mode it ended up in, and when WAL is unavailable it
        // answers with the old one rather than failing. Reading the answer is the difference
        // between requesting WAL and being in it.
        match conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            // Not busy, not WAL: the filesystem cannot give SQLite the shared memory a WAL
            // needs. Retrying that forever would be a hang, so it is reported.
            Ok(mode) => {
                return Err(StoreError::NotWal {
                    path: path.to_path_buf(),
                    mode,
                });
            }
            Err(error) if is_busy(&error) => std::thread::sleep(WAL_RETRY_INTERVAL),
            Err(error) => return Err(sqlite("request WAL mode")(error)),
        }
    }

    // Out of attempts. Report the mode it is actually in, which is the useful half.
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(sqlite("read the journal mode"))?;
    if mode.eq_ignore_ascii_case("wal") {
        return Ok(());
    }
    Err(StoreError::NotWal {
        path: path.to_path_buf(),
        mode,
    })
}

/// Whether SQLite refused because someone else held the lock.
fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
                || failure.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

/// Create `path` readable and writable by its owner alone, leaving an existing file alone.
///
/// `create_new` rather than `create`: `create` pairs with `truncate` in most uses and the
/// one thing this must never do is empty a database that is already there.
fn create_owner_only(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Refuse a database path that is a symbolic link.
///
/// [`create_owner_only`] cannot catch this on its own: `create_new` answers `AlreadyExists`
/// for an existing symlink exactly as it does for an existing file, and that answer is mapped
/// to success because the one thing it must never do is truncate a real database. What
/// follows would then treat the link as the file — [`restrict_to_owner`] chmods through it, so
/// the `0600` trap 14 asks for lands on the **target**, and `Connection::open` opens that
/// target as the database.
///
/// Checked unconditionally rather than under `cfg(unix)`: Windows has symbolic links too, and
/// a check that exists on one leg is a check whose error variant the other leg never builds.
///
/// Not a complete defence, and not claimed as one. It is a check-then-use, so a link swapped
/// in between [`create_owner_only`] and this line is still followed; closing that needs
/// `O_NOFOLLOW` on the open, which is not portable and which SQLite does not expose. Both the
/// race and the plant need write access to the daemon's state directory, which is owner-only,
/// so this narrows an already-narrow window rather than being the thing that holds the door.
fn refuse_symlink(path: &Path) -> Result<(), StoreError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| StoreError::Io {
        action: "read the file type of",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(StoreError::Symlink {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Narrow a file to its owner, best effort.
///
/// Applied to a database that already existed as well as to one just created, because a file
/// that got wider — restored from a backup, copied with `cp` — is exactly the one worth
/// narrowing. Best effort for the reason `rpc::endpoint` gives: a daemon that refuses to
/// start because `chmod` failed on a filesystem with no modes is worse than one that starts.
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        // Not enforced. A new file inherits its parent directory's ACL, and the daemon's own
        // state directory lives under `%LOCALAPPDATA%`, which is owner-only — so the default
        // location is covered by inheritance rather than by anything this function does.
        // `Store::open` accepts any path, though, and a caller that passes one somewhere else
        // gets that directory's ACL, whatever it is. Narrowing it properly means building a
        // DACL through `SetNamedSecurityInfo`, which is a `windows-sys` dependency and a
        // manifest change; until that is worth taking, this is a disclosed gap and not a
        // silent one. It is the same call `rpc::endpoint` makes.
        let _ = path;
    }
}

/// Narrow a directory this process created to its owner, best effort.
fn restrict_dir_to_owner(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// W4 will hold this in an `Arc` and reach it from several tasks. Checked at compile time so
/// that is a fact rather than a hope.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Store>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests_support::{self, temp_db};

    #[test]
    fn a_new_store_is_in_wal_at_the_current_schema() {
        let (dir, path) = temp_db("open-wal");
        let store = Store::open(&path).expect("open");
        assert_eq!(store.path(), path);
        assert_eq!(
            store.schema_version().expect("version"),
            migrate::SCHEMA_VERSION
        );

        let conn = store.conn().expect("lock");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("mode");
        assert_eq!(mode, "wal");
        drop(conn);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_an_existing_store_keeps_its_rows() {
        let (dir, path) = temp_db("open-again");
        let first = Store::open(&path).expect("open");
        first
            .record_status(&tests_support::row(tests_support::pane(), 1_000))
            .expect("record");
        drop(first);

        let second = Store::open(&path).expect("reopen");
        assert_eq!(
            second
                .status_history(&tests_support::pane(), None)
                .expect("history")
                .len(),
            1,
            "reopening must not truncate the database"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let (dir, _) = temp_db("open-nested");
        let nested = dir.join("a").join("b").join("nysia.sqlite3");
        let store = Store::open(&nested).expect("open");
        assert!(nested.exists());
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_a_current_store_takes_no_write_lock() {
        // The lock a daemon must not need. `Store::open` on an already-current schema has no
        // migration to run, so it has no reason to want the write lock — and the path that
        // needs this most is a daemon restarting to drain the spool (§2.3), which would
        // otherwise fail to open the store whenever anything else held the lock.
        //
        // `two_writers_racing_the_cap_leave_exactly_the_cap` cannot catch it: both stores are
        // opened before any write begins, so neither open is ever contended.
        let (dir, path) = temp_db("open-contended");
        Store::open(&path).expect("bring the schema up to date");

        // Someone else holds the write lock for longer than the busy timeout would wait.
        let mut holder = Connection::open(&path).expect("open the holder");
        let held = holder
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("take the write lock");

        let started = std::time::Instant::now();
        let opened = Store::open(&path);
        let elapsed = started.elapsed();

        // Release before asserting, so a failure does not leave the lock held for the rest of
        // the test binary.
        drop(held);
        drop(holder);

        opened.expect("an already-current open must not need the write lock");
        assert!(
            elapsed < BUSY_TIMEOUT,
            "opening waited {elapsed:?}, so it queued for a lock it had no work for"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_daemons_opening_a_fresh_store_both_succeed() {
        // The migration race: both read `user_version`, both decide to create the table.
        // `BEGIN IMMEDIATE` is what stops the loser failing on `CREATE TABLE`.
        let (dir, path) = temp_db("open-race");
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| Store::open(&path).map(|store| store.path().to_path_buf())))
                .collect();
            for handle in handles {
                let opened = handle.join().expect("thread");
                opened.expect("a concurrent open must succeed");
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Trap 14 again: the path itself, not its mode. `cfg(unix)` because creating a symbolic
    /// link on Windows needs a privilege this machine does not grant and the Windows leg
    /// cannot be relied on to, so a cross-platform version would be a test that silently does
    /// nothing wherever it is missing. The code it checks is not `cfg`-gated.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_database_path_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, path) = temp_db("open-symlink");
        // The file an attacker wants narrowed, or replaced, or read.
        let decoy = dir.join("decoy");
        std::fs::write(&decoy, b"not a database").expect("write the decoy");
        // Sampled rather than assumed to be 0o644: under a 077 umask the decoy is born 0o600,
        // and a test that asserts "not 0o600" would then fail with the check working. The
        // property is that the target is left as it was, so that is what is recorded.
        let mode_before = std::fs::metadata(&decoy)
            .expect("stat the decoy")
            .permissions()
            .mode();
        std::os::unix::fs::symlink(&decoy, &path).expect("plant the link");

        let error = Store::open(&path).expect_err("a symlinked store path is refused");
        assert!(
            matches!(&error, StoreError::Symlink { path: reported } if reported == &path),
            "unexpected error: {error}"
        );

        // And the target was left exactly as it was: same bytes and same mode — which is both
        // halves of what following the link would have done to it.
        assert_eq!(
            std::fs::read(&decoy).expect("read the decoy"),
            b"not a database",
            "the link's target was opened as a database"
        );
        assert_eq!(
            std::fs::metadata(&decoy)
                .expect("stat the decoy")
                .permissions()
                .mode(),
            mode_before,
            "the link's target was chmodded through the link"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Trap 14, on the leg that can express it. Windows inherits the profile ACL and has no
    /// mode bits to assert on, so this is a macOS-CI proof by construction.
    #[cfg(unix)]
    #[test]
    fn the_database_and_its_wal_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, path) = temp_db("open-modes");
        let store = Store::open(&path).expect("open");
        // A write, so the `-wal` and `-shm` exist to be checked. They live beside the
        // database for as long as a connection is open.
        store
            .record_status(&tests_support::row(tests_support::pane(), 1_000))
            .expect("record");

        for suffix in ["", "-wal", "-shm"] {
            let mut sidecar = path.clone().into_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            let mode = std::fs::metadata(&sidecar)
                .unwrap_or_else(|error| panic!("{} is missing: {error}", sidecar.display()))
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode,
                0o600,
                "{} is {mode:o}, and a question payload can be a secret",
                sidecar.display()
            );
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
