//! Fixtures the store's tests share.
//!
//! One home rather than one copy per module, because a temporary directory that two tests
//! disagree about is two tests that pass alone and fail together.

use std::path::PathBuf;

use nysia_proto::{AgentState, AgentStatusRow, PaneKey, UnixMillis};

use super::Registration;
use crate::git::CanonicalPath;

/// A directory of this test's own, and the database path inside it.
///
/// Returns the directory too, so the caller can remove it: the `-wal` and `-shm` live beside
/// the database and a test that deletes only the file it named leaves two behind.
///
/// `tag` must be unique per test. Tests in one crate share a process, so the pid alone does
/// not separate them — `std::env::temp_dir` plus a tag is the same shape `rpc::endpoint`'s
/// tests use, and the reason it is not `tempfile` is that this crate takes no dependency it
/// does not otherwise need.
pub(super) fn temp_db(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("nysia-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Created here rather than left to `Store::open`, because the migration tests open a
    // bare `rusqlite::Connection` on the path and SQLite will not create a missing parent.
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let path = dir.join("nysia.sqlite3");
    (dir, path)
}

/// A real directory `name` inside `dir`, resolved the way a registration resolves one.
///
/// A [`CanonicalPath`] and not a `PathBuf` because that is what [`Registration`] takes, and
/// it has to exist for the same reason: the type's whole guarantee is that the folder was
/// there when it was resolved. Not a git repository — deciding that a folder *is* one
/// belongs to `crate::git`, and the store is handed the answer rather than asking.
pub(super) fn folder(dir: &std::path::Path, name: &str) -> CanonicalPath {
    let at = dir.join(name);
    std::fs::create_dir_all(&at).expect("create the folder");
    CanonicalPath::of(&at).expect("resolve the folder")
}

/// A registration of `at`, under the name and group a caller would supply.
pub(super) fn registration(at: &CanonicalPath, name: &str, group: &str) -> Registration {
    Registration {
        path: at.clone(),
        name: name.to_owned(),
        group: group.to_owned(),
    }
}

/// The pane every fixture row belongs to.
pub(super) fn pane() -> PaneKey {
    PaneKey::new("tab1", "leaf1").expect("a valid pane key")
}

/// A second pane, for the tests that prove one pane's cap is not another's.
pub(super) fn other_pane() -> PaneKey {
    PaneKey::new("tab2", "leaf1").expect("a valid pane key")
}

/// A live `working` row for the lead agent in `pane`, observed at `observed_at`.
pub(super) fn row(pane: PaneKey, observed_at: u64) -> AgentStatusRow {
    AgentStatusRow {
        pane,
        state: AgentState::Working,
        question: None,
        is_interrupt: false,
        session_boundary: false,
        agent_id: None,
        observed_at: UnixMillis(observed_at),
        restored_unconfirmed: false,
    }
}

/// A `waiting` row carrying `question`, which is the only state that keeps one.
pub(super) fn waiting(
    pane: PaneKey,
    observed_at: u64,
    question: serde_json::Value,
) -> AgentStatusRow {
    AgentStatusRow {
        state: AgentState::Waiting,
        question: Some(question),
        ..row(pane, observed_at)
    }
}

/// A row belonging to the subagent `agent_id` rather than to the lead.
pub(super) fn subagent(pane: PaneKey, observed_at: u64, agent_id: &str) -> AgentStatusRow {
    AgentStatusRow {
        agent_id: Some(agent_id.to_owned()),
        ..row(pane, observed_at)
    }
}
