//! The v0.3 §3.1 project on disk: a registered folder, and nothing git can answer for.
//!
//! The contract is `docs/plans/v0.3-delivery-plan.md` §3, which is the single authority.
//! `nysia-proto` owns [`ProjectId`] and the wire's `Project`; this module owns the table
//! they land in, and **defines no second shape** for either.
//!
//! # What is stored, and what is not
//!
//! §3.1's project has four fields — `id`, `name`, `group`, `worktrees` — and only three of
//! them are here. The table holds the registration: the id, the canonical path it was
//! derived from, and the two labels the sidebar shows. `worktrees`, and the sessions inside
//! them, are **composed live** and never written down.
//!
//! That is a deliberate deviation from "persist §3.1's shape", and three facts decide it:
//!
//! - **Nothing about a worktree is unrecoverable from the path.**
//!   [`crate::worktree::Worktree::is_primary`] is "the checkout the repository was
//!   registered from", and [`crate::git::inspect_folder`] derives it by containment from the
//!   folder it was handed. Store the path and every worktree field comes back, including
//!   that one.
//! - **A stored session is a dead row.** [`nysia_proto::SessionSummary`] is keyed by a
//!   [`nysia_proto::SessionHandle`], which is runtime-scoped: it routes to a live PTY in one
//!   daemon process. The acceptance test restarts the daemon, so a persisted session would
//!   be stale in the exact scenario this milestone is measured on.
//! - **A stored worktree list is a cache that lies.** `git worktree add` run in a terminal
//!   outside Nysia — which is most of them — changes the answer without telling the store.
//!   D-6 makes git the authority on branches and D-5 keeps tasks live for the same reason;
//!   a project's worktrees are the same kind of fact.
//!
//! So the store persists the one thing with no other home: that somebody registered this
//! folder. Mapping from [`StoredProject`] to the wire's [`nysia_proto::Project`] is the
//! daemon's (v0.3 wave C1), and it is a join rather than a read.
//!
//! **One consequence, named here rather than left for C1 to find.** If `worktrees` is
//! composed live, `project_list` costs one `git worktree list` **per project**, and a
//! sidebar with ten projects is ten process spawns against the chokepoint's default timeout
//! on a path a person is waiting on. Nothing in this module can fix that — the fix is a
//! cache with an invalidation rule, or a listing that answers without worktrees and fills
//! them in — but the cost belongs to whoever writes the verb, and this is where it is known.
//!
//! # The identity rule, held by the table rather than by a caller
//!
//! §3.2: registering the same path twice is one project. [`ProjectId`] is a pure function of
//! the canonical path, so that is a `UNIQUE` on the id and not a check anybody has to
//! remember. [`Store::register_project`] derives the id itself and takes a
//! [`CanonicalPath`] rather than a `Path`, so there is no way to reach this table with an id
//! that does not belong to the path beside it, and no way to register a path that was never
//! resolved.
//!
//! A second registration is **not an edit**. It writes nothing and hands back the row that
//! was already there, so a rename — which §3.1 promises for later — cannot be undone by
//! somebody adding the same folder again from the dialog. [`Registered`] says which of the
//! two happened, because `alreadyRegistered` is on the wire and the dialog has to say "that
//! project is already in your sidebar" rather than pretend to have added it.
//!
//! # Forgetting removes the registration and nothing else
//!
//! [`Store::forget_project`] deletes one row from this table. There is no cascade, and that
//! is a decision rather than an omission:
//!
//! - **Nothing on disk is touched.** The folder, its worktrees and its branches are not
//!   Nysia's (§3.1), and [`nysia_proto::ProjectForget`] says so on the wire.
//! - **There are no session rows to drop.** Sessions live in the daemon's registry, not in
//!   this database, so "what happens to a project with live sessions in its worktrees" is
//!   not a question the store can answer wrongly: the registration goes, and the daemon
//!   decides what to do with PTYs it still owns. That is C1's decision and it is not
//!   foreclosed here.
//! - **Nothing else may quietly acquire one.** `nothing_cascades_from_forgetting_a_project`
//!   scans every table's foreign keys for a reference to `projects`, so the day a later
//!   migration adds `REFERENCES projects(id) ON DELETE CASCADE`, that is a decision somebody
//!   makes with the test in front of them rather than one inherited from a schema.
//!
//! An id nothing is registered under answers [`Forgotten::Unknown`].
//! [`nysia_proto::ProjectForget`]'s contract is `UnknownProject` rather than a quiet
//! success, and a store that reported `Removed` for a typo would leave the daemon nothing to
//! build that from.
//!
//! # Trap 13/14: this table is where a path is written down
//!
//! A repository path names a person's disk, and `nysia-proto`'s project types carry none —
//! the wire has the id and the daemon has the path. The path has to live somewhere, and this
//! is it, which makes two of [`Store::open`]'s rules load-bearing for this table and not only
//! for `question` payloads: the file is created `0600` before SQLite opens it, and a
//! symlinked path is refused rather than followed.
//!
//! On the way out, no error here names a registered path. [`StoreError::Project`] carries the
//! **database's** path, as every sibling variant does, and its source is a
//! [`nysia_proto::ProjectError`] —
//! whose `ProjectIdShape` echoes a stored id, which is a digest, and whose `PathNotUnicode`
//! carries nothing at all.

use std::path::{Path, PathBuf};

use nysia_proto::ProjectId;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::git::CanonicalPath;

use super::error::StoreError;
use super::{Store, begin, commit};

/// The four §3.1 columns, in one order, for every `SELECT` in this module.
///
/// One string rather than one per query, for the reason `status`'s own column list gives:
/// the order is what the index positions in [`StoredProject::read`] mean. `group` is quoted
/// because `GROUP` is a SQL keyword; the column is spelled the way the wire field is.
const COLUMNS: &str = r#"id, path, name, "group""#;

/// What a caller asks the store to record.
///
/// Built by the daemon, which is the component that canonicalises a path — `nysia-proto`
/// does no IO — and the component that decides what a project is called. The store persists
/// what it is handed and derives only the one field it must: the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The folder, already resolved. The id is derived from this and from nothing else.
    pub path: CanonicalPath,
    /// What the sidebar shows — §3.1's "the folder's own name by default".
    pub name: String,
    /// The sidebar's section header — [`nysia_proto::Project::DEFAULT_GROUP`] in v0.3.
    pub group: String,
}

/// A registered project as the store holds it.
///
/// Not [`nysia_proto::Project`], and the difference is the point of this module: this one
/// has the path and no worktrees, that one has the worktrees and no path. The daemon joins
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProject {
    /// Derived from [`StoredProject::path`], and stable across restarts because of it.
    pub id: ProjectId,
    /// The canonical path, as it was resolved when the folder was registered.
    ///
    /// A [`PathBuf`] rather than a [`CanonicalPath`] because reading a row must not depend
    /// on the folder still being there. Re-resolving on the way out would make listing
    /// projects fail whenever an external drive was unplugged, which is a sidebar that
    /// empties itself rather than one that shows a project that cannot be opened.
    pub path: PathBuf,
    /// What the sidebar shows.
    pub name: String,
    /// The sidebar's section header.
    pub group: String,
}

/// One row as SQLite hands it over, before the id has been read as one.
///
/// Split from [`StoredProject`] for the reason `status`'s `RawRow` is: a `query_map` closure
/// answers `rusqlite::Result`, so refusing a row this build cannot read has to happen after
/// the row is out rather than inside the closure.
struct RawProject {
    id: String,
    path: String,
    name: String,
    group: String,
}

impl RawProject {
    /// Read the [`COLUMNS`], in their order.
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            path: row.get(1)?,
            name: row.get(2)?,
            group: row.get(3)?,
        })
    }

    /// Turn it into the stored project, refusing an id this build cannot read.
    fn decode(self, path: &Path) -> Result<StoredProject, StoreError> {
        Ok(StoredProject {
            id: self.id.parse().map_err(|source| StoreError::Project {
                action: "read the id of a project in",
                path: path.to_path_buf(),
                source,
            })?,
            path: PathBuf::from(self.path),
            name: self.name,
            group: self.group,
        })
    }
}

/// What a registration turned out to be.
///
/// Carries the stored project either way, because the caller wants the same answer for both:
/// §3.2's `ProjectRegistered` reports the project *and* a flag, not one or the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registered {
    /// This call created the row.
    Created(StoredProject),
    /// The path was already registered, and this is the row that was already there.
    ///
    /// Nothing was written. The stored name and group are whatever they were, which may not
    /// be the ones this call offered.
    AlreadyRegistered(StoredProject),
}

impl Registered {
    /// The project, whichever of the two happened.
    #[must_use]
    pub fn project(&self) -> &StoredProject {
        match self {
            Self::Created(project) | Self::AlreadyRegistered(project) => project,
        }
    }

    /// The project, whichever of the two happened, by value.
    #[must_use]
    pub fn into_project(self) -> StoredProject {
        match self {
            Self::Created(project) | Self::AlreadyRegistered(project) => project,
        }
    }

    /// `nysia_proto::ProjectRegistered::already_registered`, for the daemon's answer.
    #[must_use]
    pub const fn already_registered(&self) -> bool {
        matches!(self, Self::AlreadyRegistered(_))
    }
}

/// What forgetting a project turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forgotten {
    /// The registration was there, and is not any more.
    Removed,
    /// Nothing was registered under that id, so nothing was removed.
    Unknown,
}

impl Store {
    /// Register a folder, or report that it already was.
    ///
    /// Idempotent by §3.2: the id is derived from `registration.path` and the table is
    /// unique on it, so registering the same folder twice is one project. The second call
    /// **writes nothing** and answers [`Registered::AlreadyRegistered`] with the row that is
    /// already there — a registration is not an edit, and the name a person set is not
    /// something re-adding the folder may quietly replace.
    ///
    /// Two processes racing the same path are serialised by the write lock, so exactly one
    /// of them sees [`Registered::Created`].
    ///
    /// # Errors
    ///
    /// - [`StoreError::Project`] if the path is not valid Unicode, which leaves it with no
    ///   one spelling to derive a stable id from.
    /// - [`StoreError::Sqlite`] if the write fails, in which case nothing was written.
    pub fn register_project(&self, registration: &Registration) -> Result<Registered, StoreError> {
        let id = ProjectId::from_canonical_path(registration.path.as_path()).map_err(|source| {
            StoreError::Project {
                action: "derive the id of a project for",
                path: self.path.clone(),
                source,
            }
        })?;
        let text = match registration.path.as_path().to_str() {
            Some(text) => text,
            // `from_canonical_path` asks this same question and has already refused a path
            // that cannot answer it, so there is nothing to reach here. Spelled out rather
            // than `expect`ed: CLAUDE.md §6 denies both, and this records why instead of
            // asserting that it cannot happen.
            None => unreachable!("ProjectId::from_canonical_path refuses a non-Unicode path"),
        };

        let mut conn = self.conn()?;
        let tx = begin(&mut conn, &self.path, "begin a project registration")?;
        // Read and then write, under the write lock the whole way. A deferred transaction
        // would let the other process commit in between, which turns the losing side of a
        // race into a constraint failure instead of an answer.
        if let Some(existing) = project_on(&tx, &self.path, &id)? {
            return Ok(Registered::AlreadyRegistered(existing));
        }
        tx.execute(
            r#"INSERT INTO projects (id, path, name, "group") VALUES (?1, ?2, ?3, ?4)"#,
            rusqlite::params![id.as_str(), text, registration.name, registration.group],
        )
        .map_err(|source| StoreError::Sqlite {
            action: "insert a project row",
            path: self.path.clone(),
            source,
        })?;
        commit(tx, &self.path, "commit a project registration")?;

        Ok(Registered::Created(StoredProject {
            id,
            path: registration.path.as_path().to_path_buf(),
            name: registration.name.clone(),
            group: registration.group.clone(),
        }))
    }

    /// Every registered project, in the order they were registered.
    ///
    /// Registration order, not alphabetical: the sidebar's order is the person's, and a list
    /// that resorts itself when a project is renamed is one they did not arrange. Forgetting
    /// a project and registering it again therefore lands it at the end, which is where the
    /// dialog just put it.
    ///
    /// # Errors
    ///
    /// - [`StoreError::Project`] if a stored id is not one this build can read.
    /// - [`StoreError::Sqlite`] if the read fails.
    pub fn projects(&self) -> Result<Vec<StoredProject>, StoreError> {
        let conn = self.conn()?;
        let sql = format!("SELECT {COLUMNS} FROM projects ORDER BY seq");
        let mut statement = conn.prepare(&sql).map_err(|source| StoreError::Sqlite {
            action: "prepare the project list",
            path: self.path.clone(),
            source,
        })?;
        let rows = statement
            .query_map([], RawProject::read)
            .map_err(|source| StoreError::Sqlite {
                action: "read the project list",
                path: self.path.clone(),
                source,
            })?;
        let mut projects = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StoreError::Sqlite {
                action: "read a project row",
                path: self.path.clone(),
                source,
            })?;
            projects.push(raw.decode(&self.path)?);
        }
        Ok(projects)
    }

    /// One registered project, or `None` when nothing is registered under `id`.
    ///
    /// # Errors
    ///
    /// As [`Store::projects`].
    pub fn project(&self, id: &ProjectId) -> Result<Option<StoredProject>, StoreError> {
        let conn = self.conn()?;
        project_on(&conn, &self.path, id)
    }

    /// Forget a project: remove its registration, and nothing else.
    ///
    /// Nothing on disk is touched and nothing cascades — the module docs say why that is a
    /// decision rather than an omission.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the delete fails, in which case nothing was
    /// removed.
    pub fn forget_project(&self, id: &ProjectId) -> Result<Forgotten, StoreError> {
        let conn = self.conn()?;
        // One statement, so autocommit is already the transaction. An explicit one would
        // take the write lock in order to decide whether there is anything to write.
        let removed = conn
            .execute("DELETE FROM projects WHERE id = ?1", [id.as_str()])
            .map_err(|source| StoreError::Sqlite {
                action: "forget a project in",
                path: self.path.clone(),
                source,
            })?;
        Ok(if removed == 0 {
            Forgotten::Unknown
        } else {
            Forgotten::Removed
        })
    }
}

/// One project on an open connection or transaction, so the caller chooses the snapshot.
fn project_on(
    conn: &Connection,
    path: &Path,
    id: &ProjectId,
) -> Result<Option<StoredProject>, StoreError> {
    let sql = format!("SELECT {COLUMNS} FROM projects WHERE id = ?1");
    conn.query_row(&sql, [id.as_str()], RawProject::read)
        .optional()
        .map_err(|source| StoreError::Sqlite {
            action: "read a project in",
            path: path.to_path_buf(),
            source,
        })?
        .map(|raw| raw.decode(path))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests_support::{folder, registration, temp_db};

    #[test]
    fn a_project_reads_back_under_the_id_its_path_derives() {
        let (dir, path) = temp_db("project-roundtrip");
        let store = Store::open(&path).expect("open");
        let at = folder(&dir, "nysia");

        let registered = store
            .register_project(&registration(&at, "nysia", "Dev"))
            .expect("register");
        assert!(matches!(registered, Registered::Created(_)));
        assert!(!registered.already_registered());

        // The identity rule from the other end: the id is the one `nysia-proto` derives for
        // this canonical path, not one the store minted and then kept.
        let expected =
            ProjectId::from_canonical_path(at.as_path()).expect("a temp path is Unicode");
        let stored = registered.into_project();
        assert_eq!(stored.id, expected);
        assert_eq!(stored.path, at.as_path());
        assert_eq!(stored.name, "nysia");
        assert_eq!(stored.group, "Dev");

        assert_eq!(store.projects().expect("list"), vec![stored.clone()]);
        assert_eq!(store.project(&expected).expect("get"), Some(stored));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_registration_outlives_the_store_that_made_it() {
        // §3.3 at this module's own boundary. `crates/nysia/tests/projects.rs` is the same
        // property across a daemon restart; this is the half that can be checked from here.
        let (dir, path) = temp_db("project-reopen");
        let at = folder(&dir, "nysia");
        let first = Store::open(&path).expect("open");
        let before = first
            .register_project(&registration(&at, "nysia", "Dev"))
            .expect("register")
            .into_project();
        drop(first);

        let second = Store::open(&path).expect("reopen");
        assert_eq!(second.projects().expect("list"), vec![before]);
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registering_the_same_folder_twice_is_one_project() {
        let (dir, path) = temp_db("project-idempotent");
        let store = Store::open(&path).expect("open");
        let at = folder(&dir, "nysia");

        let first = store
            .register_project(&registration(&at, "the name it was given", "Dev"))
            .expect("register");
        assert!(matches!(first, Registered::Created(_)));

        // A different name and a different group, so what follows cannot pass on an
        // `INSERT OR REPLACE` that happened to write the same values back.
        let again = store
            .register_project(&registration(&at, "a later name", "Elsewhere"))
            .expect("register again");
        assert!(
            again.already_registered(),
            "a second registration of one folder must say so: {again:?}"
        );
        assert_eq!(
            again.project().name,
            "the name it was given",
            "a second registration is not an edit, and must not undo a rename"
        );
        assert_eq!(again.project().group, "Dev");
        assert_eq!(
            store.projects().expect("list").len(),
            1,
            "registering one folder twice must leave one project"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
