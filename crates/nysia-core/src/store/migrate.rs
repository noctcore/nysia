//! The migration runner: a versioned schema applied forward, once, in a transaction.
//!
//! Scrollback, worktrees and orchestration all land in this database later, so the shape
//! that matters is not this one table — it is that **adding a migration is one entry in one
//! list**, and that running the list twice does nothing the second time.
//!
//! # Adding a migration
//!
//! Append a [`Migration`] to [`MIGRATIONS`] with the next `version`, and raise
//! [`SCHEMA_VERSION`] to match. Never edit an applied one: a database in the wild is already
//! at that version and will skip it, so an edit is a schema that exists on fresh machines
//! and nowhere else. `the_migration_list_is_contiguous` and
//! `the_schema_version_is_the_last_migration` hold both halves of that.
//!
//! # Why `user_version`
//!
//! SQLite keeps a caller-owned 32-bit integer in the database header, and writing it is
//! transactional along with everything else in the transaction. So the version and the
//! schema it describes commit together, and there is no window in which one is ahead of the
//! other — which a separate `schema_migrations` table cannot promise without being written
//! in the same transaction anyway, at the cost of a table that is itself a migration.
//!
//! # Why every step is `BEGIN IMMEDIATE`
//!
//! Two daemons can open a fresh database at the same moment — the discovery race in
//! `crate::rpc::discovery` narrows that to one daemon, but the store is a library and does
//! not get to assume it. A deferred transaction starts as a reader and only takes the write
//! lock at the first write, so both would read `user_version = 0`, both would decide to
//! create the table, and the second would fail on `CREATE TABLE`. `IMMEDIATE` takes the
//! write lock **before** the version is read, so the second daemon reads the version the
//! first one committed and applies nothing. `two_daemons_opening_a_fresh_store_both_succeed`
//! is that race.
//!
//! Every step that has **work to do**, that is. A step the recorded version already covers is
//! skipped before any transaction opens, so the common case — an open of a database already at
//! the newest schema — takes no lock at all. Leaving that to the re-read inside the
//! transaction would make every open of a current database queue behind whatever else is
//! writing, and then fail once it ran out of busy timeout.
//! `opening_a_current_store_takes_no_write_lock` is that open.

use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::error::StoreError;

/// One forward step of the schema.
///
/// `sql` is run with `execute_batch`, so a step may contain several statements.
#[derive(Debug, Clone, Copy)]
pub(super) struct Migration {
    /// Which step this is. The list is contiguous and starts at 1.
    pub(super) version: u32,
    /// What it does, for the error message when it does not.
    pub(super) name: &'static str,
    /// The statements to run.
    pub(super) sql: &'static str,
}

/// The newest schema this build can produce.
///
/// Held equal to the last entry of [`MIGRATIONS`] by
/// `the_schema_version_is_the_last_migration`, because a stated total that can drift from the
/// list beneath it is a number nobody can trust.
pub(super) const SCHEMA_VERSION: u32 = 2;

/// Every migration, in the order they apply.
///
/// # 1 — `agent_status`
///
/// The §2.2 status table. **Eight columns, spelled as §2.2 spells them**, plus one key:
///
/// - `seq` is the row's position in the history, and it is what "latest" and the history cap
///   are computed from. `observed_at` cannot do that job: two hooks in the same millisecond
///   are ordinary — a `PreToolUse` and its `PostToolUse` around a fast tool — and a "latest"
///   that ties is a dot that flickers between two states depending on which row the query
///   planner reached first.
/// - It is declared rather than left as the implicit `rowid`, which would also be monotonic
///   here. `VACUUM` renumbers the rowids of a table with no `INTEGER PRIMARY KEY`, and a
///   maintenance command silently reordering a pane's history is the kind of defect that is
///   found a year later. Declaring it makes the column an alias for the rowid and pins it.
///
/// `agent_id` is `NULL` on the lead's row, which is what makes one table serve §2.2's "the
/// same shape for the lead and the roster". Every query therefore says `agent_id IS ?`
/// rather than `=`, because `NULL = NULL` is `NULL` in SQL and a lead's history queried with
/// `=` comes back empty.
///
/// `question` holds the payload as its **serialised JSON text**, not as a value mapped onto
/// SQLite's types. A tool's input is arbitrary JSON and SQLite has five storage classes: a
/// `u64` past `i64::MAX` has no `INTEGER` to land in, `1.0` and `1` are the same `REAL`, and
/// an object has nowhere to go at all. Text round-trips every one of them exactly, and
/// `serde_json` escapes `U+0000` as `\u0000` on the way out, so the stored text never
/// carries the interior NUL that truncates a C string.
///
/// The index covers the one access path there is: newest-first within a `(pane, agent_id)`.
///
/// # 2 — `projects`
///
/// v0.3 §3.1's registered folder. **Four columns and one key**, and the four are the
/// registration: everything else §3.1 lists is queried rather than stored, which
/// [`crate::store::project`] says why at length.
///
/// - `id` is the identity, and it carries the `UNIQUE` because of it. §3.2's idempotency is
///   "registering the same path twice is one project", the path's identity is
///   [`nysia_proto::ProjectId`], and a constraint on `name` instead would refuse two real
///   folders that happen to be called `nysia`. There is deliberately no `UNIQUE` on `path`:
///   the id is a function of the path, so one already implies the other, and a second
///   constraint would only differ from the first on a digest collision — where the useful
///   report is the one naming the identity that collided.
/// - `path` is the canonical path the id was derived from, spelled as
///   [`crate::git::CanonicalPath`] resolved it. The daemon needs it to ask git anything at
///   all after a restart, and this table is the **only** place Nysia writes a repository
///   path down — which is why the database file is owner-only (trap 14) and why no error
///   variant in this module echoes one.
/// - `"group"` is quoted because `GROUP` is a SQL keyword. Quoting it is cheaper than a
///   second spelling: `Project::group` is the field name on the wire, and a column called
///   `group_name` would be one more mapping for a reader to hold.
/// - `seq` is registration order, which is the order the sidebar lists in. Declared rather
///   than left as the implicit `rowid` for the reason migration 1 gives.
pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "agent_status",
        sql: "
        CREATE TABLE agent_status (
            seq                  INTEGER PRIMARY KEY,
            pane                 TEXT    NOT NULL,
            state                TEXT    NOT NULL,
            question             TEXT,
            is_interrupt         INTEGER NOT NULL,
            session_boundary     INTEGER NOT NULL,
            agent_id             TEXT,
            observed_at          INTEGER NOT NULL,
            restored_unconfirmed INTEGER NOT NULL
        );
        CREATE INDEX agent_status_by_agent
            ON agent_status (pane, agent_id, seq DESC);
    ",
    },
    Migration {
        version: 2,
        name: "projects",
        sql: r#"
        CREATE TABLE projects (
            seq     INTEGER PRIMARY KEY,
            id      TEXT    NOT NULL UNIQUE,
            path    TEXT    NOT NULL,
            name    TEXT    NOT NULL,
            "group" TEXT    NOT NULL
        );
    "#,
    },
];

/// Bring `conn` up to the newest version in `migrations`, and report where it landed.
///
/// Idempotent: a database already at the newest version has nothing applied to it, and no
/// write transaction is opened for a step that is already in place.
///
/// `migrations` is a parameter rather than a reference to [`MIGRATIONS`] so that the runner
/// can be tested with more than one step while the real list has one. A runner exercised only
/// by the list it ships with is a runner whose forward-application is untested until the day
/// it matters.
///
/// # Errors
///
/// - [`StoreError::MigrationOrder`] if `migrations` is not `1, 2, 3, …`.
/// - [`StoreError::SchemaAhead`] if the database is newer than `migrations` can produce.
/// - [`StoreError::Migration`] if a step fails; that step is rolled back and no later step
///   runs, so the database is left at the last version that did apply.
pub(super) fn apply(
    conn: &mut Connection,
    migrations: &[Migration],
    path: &Path,
) -> Result<u32, StoreError> {
    check_order(migrations)?;

    let newest = migrations.last().map_or(0, |m| m.version);
    let found = user_version(conn, path)?;
    if found > newest {
        return Err(StoreError::SchemaAhead {
            path: path.to_path_buf(),
            found,
            known: newest,
        });
    }

    for migration in migrations {
        // The step is already in place, so there is nothing to take the write lock for. Not
        // an optimisation: without it an already-current open queues behind any other writer
        // for `BUSY_TIMEOUT` and then fails, which is exactly the daemon restarting to drain
        // the spool. `opening_a_current_store_takes_no_write_lock` is that open.
        if migration.version <= found {
            continue;
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| StoreError::Sqlite {
                action: "begin a migration",
                path: path.to_path_buf(),
                source,
            })?;
        // Re-read under the write lock. Another process may have applied this step between
        // the check above and this line, which is the whole reason the read is in here.
        if user_version(&tx, path)? >= migration.version {
            continue;
        }
        apply_in(&tx, migration, path)?;
        tx.commit().map_err(|source| StoreError::Migration {
            version: migration.version,
            name: migration.name,
            path: path.to_path_buf(),
            source,
        })?;
    }

    user_version(conn, path)
}

/// Run one step inside an open transaction, and set the version it produces.
///
/// Split out so a test can hold the transaction open and ask what a second connection sees
/// while it is — see `a_reader_never_sees_a_half_applied_migration`. Nothing else calls it
/// directly.
pub(super) fn apply_in(
    tx: &Transaction<'_>,
    migration: &Migration,
    path: &Path,
) -> Result<(), StoreError> {
    let failed = |source| StoreError::Migration {
        version: migration.version,
        name: migration.name,
        path: path.to_path_buf(),
        source,
    };
    tx.execute_batch(migration.sql).map_err(failed)?;
    // `PRAGMA user_version` takes no bound parameter, so the value is formatted in. It is a
    // `u32` from a `const` list and cannot carry anything but digits.
    tx.execute_batch(&format!("PRAGMA user_version = {}", migration.version))
        .map_err(failed)
}

/// The schema version recorded in the database header.
pub(super) fn user_version(conn: &Connection, path: &Path) -> Result<u32, StoreError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| StoreError::Sqlite {
            action: "read the schema version",
            path: path.to_path_buf(),
            source,
        })?;
    // Negative or past `u32` means something other than this runner wrote the header; treat
    // it as a schema this build cannot produce rather than wrapping it into a plausible one.
    u32::try_from(version).map_err(|_| StoreError::SchemaAhead {
        path: path.to_path_buf(),
        found: u32::MAX,
        known: SCHEMA_VERSION,
    })
}

/// Refuse a list that is not `1, 2, 3, …`.
///
/// A gap would leave a database at the version before it forever: the loop applies a step
/// only when the recorded version is below it, and nothing ever raises the version past a
/// hole.
fn check_order(migrations: &[Migration]) -> Result<(), StoreError> {
    let mut previous = 0;
    for migration in migrations {
        if migration.version != previous + 1 {
            return Err(StoreError::MigrationOrder {
                version: migration.version,
                name: migration.name,
                previous,
            });
        }
        previous = migration.version;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests_support::temp_db;

    // `temp_db` hands back the directory so the test can remove it, and the path inside it.

    /// Two synthetic steps, so the runner is exercised as a runner rather than as the one
    /// migration it currently ships. The second alters what the first created, which is the
    /// case a "fresh equals stepped" comparison has to cover.
    const STEP_ONE: Migration = Migration {
        version: 1,
        name: "first",
        sql: "CREATE TABLE widget (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    };
    const STEP_TWO: Migration = Migration {
        version: 2,
        name: "second",
        sql: "ALTER TABLE widget ADD COLUMN colour TEXT;
              CREATE INDEX widget_by_name ON widget (name);",
    };

    /// Everything `sqlite_master` knows, plus the version: the whole schema as a comparable
    /// value. Ordered explicitly, because `sqlite_master` is in creation order and two
    /// databases that created the same objects in a different order are still identical
    /// schemas.
    fn schema_of(conn: &Connection) -> (u32, Vec<String>) {
        let mut statement = conn
            .prepare(
                "SELECT type, name, tbl_name, COALESCE(sql, '')
                   FROM sqlite_master
                  ORDER BY type, name",
            )
            .expect("prepare");
        let objects = statement
            .query_map([], |row| {
                Ok(format!(
                    "{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        let version = user_version(conn, Path::new("<test>")).expect("version");
        (version, objects)
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .expect("count")
            > 0
    }

    #[test]
    fn the_migration_list_is_contiguous() {
        check_order(MIGRATIONS).expect("the shipped list applies in order");
    }

    #[test]
    fn the_schema_version_is_the_last_migration() {
        // The stated total against the list beneath it: raising one without the other is the
        // drift this holds shut.
        assert_eq!(
            SCHEMA_VERSION,
            MIGRATIONS.last().map_or(0, |m| m.version),
            "SCHEMA_VERSION must name the last migration"
        );
        assert_eq!(
            MIGRATIONS.len(),
            SCHEMA_VERSION as usize,
            "a contiguous list from 1 has exactly SCHEMA_VERSION entries"
        );
    }

    #[test]
    fn a_gap_in_the_migration_list_is_refused() {
        let gapped = &[
            STEP_ONE,
            Migration {
                version: 3,
                name: "third",
                sql: "SELECT 1;",
            },
        ];
        let error = check_order(gapped).expect_err("a gap is refused");
        assert!(
            matches!(
                error,
                StoreError::MigrationOrder {
                    version: 3,
                    previous: 1,
                    ..
                }
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_stepped_database_matches_a_fresh_one() {
        // "Fresh" means the same list applied in one run — not an independently written
        // consolidated schema. That is deliberate and it is the whole property a forward-only
        // runner has: there is no hand-maintained "current schema" DDL for the list to drift
        // from, so the only thing worth holding is that arriving at version N one step at a
        // time is indistinguishable from arriving in one go. What it would catch is a step
        // that is not self-contained — one whose `ALTER` depends on state an earlier step left
        // behind rather than on the schema that step declares — which is why `STEP_TWO` alters
        // what `STEP_ONE` created rather than adding an unrelated table.
        let (dir, stepped_path) = temp_db("migrate-stepped");
        let fresh_path = dir.join("fresh.sqlite3");

        // Stepped: version 1 first, then the full list on a second open.
        let mut stepped = Connection::open(&stepped_path).expect("open stepped");
        apply(&mut stepped, &[STEP_ONE], &stepped_path).expect("apply step one");
        assert_eq!(schema_of(&stepped).0, 1);
        apply(&mut stepped, &[STEP_ONE, STEP_TWO], &stepped_path).expect("apply step two");

        // Fresh: both steps at once.
        let mut fresh = Connection::open(&fresh_path).expect("open fresh");
        apply(&mut fresh, &[STEP_ONE, STEP_TWO], &fresh_path).expect("apply both");

        assert_eq!(
            schema_of(&stepped),
            schema_of(&fresh),
            "a database migrated in steps must be indistinguishable from a fresh one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v0_2_database_arrives_at_the_fresh_schema_with_its_rows() {
        // The same property as `a_stepped_database_matches_a_fresh_one`, asked of the two
        // migrations this build actually ships rather than of two synthetic steps. v0.2 noted
        // the property was trivial with one migration in the list; it is not trivial now, and
        // this is the pair a daemon updating in the field will really apply.
        //
        // The row is the half a schema comparison cannot see. `schema_of` would be satisfied
        // by a migration that dropped and recreated `agent_status`, and a person whose agent
        // history vanished on upgrade would have no way to tell that from a schema that
        // matched.
        let (dir, upgraded_path) = temp_db("migrate-v0-2");
        let fresh_path = dir.join("fresh.sqlite3");

        // A v0.2 database: migration 1, and a status row somebody's history depends on.
        let mut upgraded = Connection::open(&upgraded_path).expect("open the v0.2 database");
        apply(&mut upgraded, &MIGRATIONS[..1], &upgraded_path).expect("apply v0.2's migration");
        assert_eq!(schema_of(&upgraded).0, 1, "a v0.2 database is at version 1");
        assert!(!table_exists(&upgraded, "projects"));
        upgraded
            .execute(
                "INSERT INTO agent_status
                     (pane, state, question, is_interrupt, session_boundary, agent_id,
                      observed_at, restored_unconfirmed)
                 VALUES ('tab1:leaf1', 'working', NULL, 0, 0, NULL, 1000, 0)",
                [],
            )
            .expect("write a v0.2 row");

        // The upgrade a daemon performs on its first start after this build lands.
        apply(&mut upgraded, MIGRATIONS, &upgraded_path).expect("apply v0.3's migration");

        let mut fresh = Connection::open(&fresh_path).expect("open the fresh database");
        apply(&mut fresh, MIGRATIONS, &fresh_path).expect("apply both at once");

        assert_eq!(
            schema_of(&upgraded),
            schema_of(&fresh),
            "a v0.2 database brought forward must be indistinguishable from a fresh one"
        );
        assert!(table_exists(&upgraded, "projects"));
        let kept: i64 = upgraded
            .query_row("SELECT COUNT(*) FROM agent_status", [], |row| row.get(0))
            .expect("count");
        assert_eq!(kept, 1, "the upgrade must not cost a person their history");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn applying_the_list_twice_changes_nothing() {
        let (dir, path) = temp_db("migrate-twice");
        let mut conn = Connection::open(&path).expect("open");
        let first = apply(&mut conn, MIGRATIONS, &path).expect("first run");
        let before = schema_of(&conn);
        let second = apply(&mut conn, MIGRATIONS, &path).expect("second run");
        assert_eq!(first, SCHEMA_VERSION);
        assert_eq!(second, SCHEMA_VERSION);
        assert_eq!(before, schema_of(&conn), "a second run is a no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let (dir, path) = temp_db("migrate-ahead");
        let mut conn = Connection::open(&path).expect("open");
        apply(&mut conn, &[STEP_ONE, STEP_TWO], &path).expect("apply both");

        // An older build, which knows only the first step, meets it.
        let error = apply(&mut conn, &[STEP_ONE], &path).expect_err("a newer schema is refused");
        assert!(
            matches!(
                error,
                StoreError::SchemaAhead {
                    found: 2,
                    known: 1,
                    ..
                }
            ),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reader_never_sees_a_half_applied_migration() {
        let (dir, path) = temp_db("migrate-midway");
        let mut writer = Connection::open(&path).expect("open writer");
        // WAL before the transaction opens: the point of the test is that the observer reads
        // while a write is in flight, and only WAL lets it.
        let mode: String = writer
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .expect("wal");
        assert_eq!(mode, "wal");

        let tx = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin");
        apply_in(&tx, &MIGRATIONS[0], &path).expect("apply inside the transaction");
        // Inside, it has happened.
        assert!(table_exists(&tx, "agent_status"));

        // Outside, none of it has. Not the table, and not the version that describes it —
        // which is the half a non-transactional runner would leave visible.
        let observer = Connection::open(&path).expect("open observer");
        assert_eq!(user_version(&observer, &path).expect("version"), 0);
        assert!(!table_exists(&observer, "agent_status"));

        tx.commit().expect("commit");

        // And after the commit, both halves arrive together.
        assert_eq!(user_version(&observer, &path).expect("version"), 1);
        assert!(table_exists(&observer, "agent_status"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
