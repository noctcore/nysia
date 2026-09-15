//! Fixtures the store's tests share.
//!
//! One home rather than one copy per module, because a temporary directory that two tests
//! disagree about is two tests that pass alone and fail together.

use std::path::PathBuf;

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
