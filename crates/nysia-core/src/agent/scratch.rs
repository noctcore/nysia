//! A throwaway directory for tests, removed when the guard drops.
//!
//! Unique per process and per call. The pty tests next door use fixed names, which is fine
//! while they run one at a time and stops being fine the moment two of them, or two cargo
//! invocations, overlap — and the hook tests here write files whose whole point is their
//! exact bytes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes two scratch directories made in the same process in the same nanosecond.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory that deletes itself.
pub(crate) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Make one, named after `tag` so a leaked directory says which test leaked it.
    pub(crate) fn new(tag: &str) -> Self {
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nysia-{tag}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Self { path }
    }

    /// Where it is.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// A path inside it.
    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A failed cleanup must not mask the assertion that is already failing.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
