//! Why a store operation failed.
//!
//! One error type for the whole module, because a caller that has to match on four of them
//! to answer "did the status get written" is a caller that will match on none.
//!
//! Every variant carries the path, or the value, or both. A bare `rusqlite::Error` says
//! `database is locked` and nothing about which database or what was being attempted, and
//! the daemon runs more than one store path in tests and exactly one in production — so the
//! message that reaches a log has to say which.

use std::path::PathBuf;

use nysia_proto::AgentError;

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database file, or the directory holding it, could not be prepared.
    #[error("could not {action} the store at {}: {source}", path.display())]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// Which path.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// SQLite refused an operation.
    #[error("could not {action} in the store at {}: {source}", path.display())]
    Sqlite {
        /// What was being attempted.
        action: &'static str,
        /// Which database.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: rusqlite::Error,
    },
    /// The database opened, but not in WAL mode.
    ///
    /// Its own variant rather than an `Io`, because the cause is almost always the
    /// filesystem rather than the file: WAL needs shared memory, which a network share and
    /// some container mounts do not provide, and SQLite's answer to that is to leave
    /// `journal_mode` on `delete` and carry on rather than to fail. D-12 asks for WAL
    /// specifically, so a store that silently is not in it is a store that will serialise
    /// every reader against the daemon's writes.
    #[error("the store at {} is in {mode:?} mode, not WAL", path.display())]
    NotWal {
        /// Which database.
        path: PathBuf,
        /// What `PRAGMA journal_mode` actually reported.
        mode: String,
    },
    /// A migration failed to apply, and was rolled back.
    #[error("migration {version} ({name}) failed on the store at {}: {source}", path.display())]
    Migration {
        /// Which migration.
        version: u32,
        /// Its name, as the migration list spells it.
        name: &'static str,
        /// Which database.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: rusqlite::Error,
    },
    /// The migration list has a gap, or does not start at 1.
    ///
    /// A programming error rather than a runtime one — the list is a `const` — but it is
    /// checked rather than assumed, because the failure it prevents is a migration that
    /// never runs on an existing database while passing on every fresh one.
    #[error("migration {version} ({name}) does not follow {previous}")]
    MigrationOrder {
        /// The migration that is out of order.
        version: u32,
        /// Its name.
        name: &'static str,
        /// The version it was expected to follow.
        previous: u32,
    },
    /// The database was written by a newer build of Nysia.
    ///
    /// Refused rather than opened. Migrations only run forward, so this build has no way to
    /// read a schema it does not know — and an older daemon writing into a newer schema is
    /// how a downgrade turns into data loss rather than into an error message.
    #[error(
        "the store at {} is at schema version {found}, and this build knows {known}",
        path.display()
    )]
    SchemaAhead {
        /// Which database.
        path: PathBuf,
        /// The version on disk.
        found: u32,
        /// The newest version this build can apply.
        known: u32,
    },
    /// A stored agent state was not one of the four wire spellings.
    #[error("the store at {} holds {value:?}, which is not an agent state: {source}", path.display())]
    State {
        /// Which database.
        path: PathBuf,
        /// What was on disk.
        value: String,
        /// What `nysia-proto` made of it.
        #[source]
        source: AgentError,
    },
    /// A stored pane key was not `<tabId>:<leafId>`.
    #[error("the store at {} holds {value:?}, which is not a pane key: {source}", path.display())]
    Pane {
        /// Which database.
        path: PathBuf,
        /// What was on disk.
        value: String,
        /// What `nysia-proto` made of it.
        #[source]
        source: nysia_proto::IdentityError,
    },
    /// A question payload could not be encoded to, or decoded from, its stored JSON.
    #[error("could not {action} a question payload for the store at {}: {source}", path.display())]
    Question {
        /// Whether it was on the way in or the way out.
        action: &'static str,
        /// Which database.
        path: PathBuf,
        /// What serde made of it.
        #[source]
        source: serde_json::Error,
    },
    /// A timestamp did not fit the signed 64-bit integer SQLite stores.
    ///
    /// [`nysia_proto::UnixMillis`] is a `u64` and SQLite's `INTEGER` is an `i64`, so the top
    /// half of the range has nowhere to go. Unreachable with a real clock — the boundary is
    /// in the year 292 277 026 596 — and an error rather than an `as` cast anyway, because
    /// the cast's failure mode is a negative timestamp that reads back as a row from before
    /// 1970 and is therefore permanently stale.
    #[error("a timestamp of {millis} does not fit SQLite's signed 64-bit INTEGER")]
    Timestamp {
        /// The offending value.
        millis: u64,
    },
    /// A stored timestamp was negative, so it was not written by this code.
    #[error("the store at {} holds a negative timestamp, {millis}", path.display())]
    NegativeTimestamp {
        /// Which database.
        path: PathBuf,
        /// The offending value.
        millis: i64,
    },
    /// The database path is a symbolic link.
    ///
    /// Refused rather than followed. `create_owner_only` uses `create_new`, which fails with
    /// `AlreadyExists` on an existing symlink and is mapped to "the file is already there" —
    /// after which `chmod` follows the link and narrows its **target**, and SQLite opens that
    /// target as the database. So a link planted at the path redirects a file trap 14 says is
    /// owner-only, and hands `0600` to whatever it points at.
    ///
    /// Planting one needs write access to the daemon's state directory, which is itself
    /// owner-only, so this is a second line rather than the only one.
    #[error("the store path {} is a symbolic link, which this will not follow", path.display())]
    Symlink {
        /// The link.
        path: PathBuf,
    },
    /// Another thread panicked while holding the connection.
    ///
    /// Surfaced rather than recovered. `rusqlite` rolls a dropped [`rusqlite::Transaction`]
    /// back, so the database itself is consistent — but a panic inside the store is a bug
    /// this module has no business papering over, and a daemon that keeps writing status
    /// after one is a daemon whose first symptom has already been swallowed.
    #[error("the store connection was poisoned by a panic in another thread")]
    Poisoned,
}
