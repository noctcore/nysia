//! Repositories on disk, for the tests that need a real one.
//!
//! Every fixture is built through the chokepoint itself rather than through a bare
//! `std::process::Command`, so a test runs against the same hardened invocation production
//! does — and so a fixture cannot be quietly shaped by whatever is in the machine's global
//! git config.
//!
//! Three things here are deliberate and would otherwise cost a red CI leg each:
//!
//! - **The branch is pinned** to [`Scratch::BRANCH`], and it is neither `main` nor `master`.
//!   `init.defaultBranch` differs between a developer's machine and a runner, so a test that
//!   asserts either name passes in one place and fails in the other; a third name asserts
//!   that the branch came from the fixture.
//! - **Identity and signing are pinned per invocation.** A committer with no `user.email`
//!   fails, and a machine with `commit.gpgsign = true` turns `git commit` into a wait for a
//!   passphrase — which on a CI runner is the test timing out for a reason nobody will guess.
//! - **Cleanup clears the read-only bit first.** git writes loose objects read-only, and
//!   `remove_dir_all` on Windows refuses a read-only file, so a fixture that committed
//!   anything leaves its directory behind without it.

use std::path::{Path, PathBuf};

use super::command::{Git, GitCommand};
use super::path::CanonicalPath;

/// A resolved git, or `None` on a machine that has none.
///
/// A test that needs a repository cannot make one without git, and skipping is the honest
/// answer: failing would report "this code is broken" for a machine that never had git. Both
/// CI legs have it, so the skip is not how these tests normally end — and
/// `command::tests::a_missing_git_is_reported_before_anything_is_spawned` is what covers the
/// missing-git behaviour itself.
pub(crate) fn git_or_skip() -> Option<Git> {
    Git::locate().ok()
}

/// A temporary directory that cleans itself up, and the fixtures built inside it.
pub(crate) struct Scratch {
    /// The directory everything is built in.
    root: PathBuf,
    /// The git used to build fixtures.
    git: Git,
}

impl Scratch {
    /// The branch every fixture repository is on.
    ///
    /// Not `main` and not `master`: either would also be the runner's `init.defaultBranch`
    /// on some machines, so asserting it would not prove the fixture put it there.
    pub(crate) const BRANCH: &'static str = "trunk";

    /// A new scratch directory, named for the test using it.
    ///
    /// The pid and thread id are in the name because `cargo test` runs tests in parallel
    /// threads of one process: a shared name is a fixture two tests delete from under each
    /// other, which passes alone and fails in a suite.
    pub(crate) fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "nysia-git-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        remove_tree(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self {
            root,
            git: Git::locate().expect("git, which the caller checked for with `git_or_skip`"),
        }
    }

    /// An empty folder at `relative`, creating its parents.
    pub(crate) fn folder(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(&path).expect("a folder");
        path
    }

    /// A repository at `relative` with one commit on [`Scratch::BRANCH`].
    pub(crate) fn repository(&self, relative: &str) -> PathBuf {
        let path = self.empty_repository(relative);
        std::fs::write(path.join("a.txt"), "hello\n").expect("a file to commit");
        self.run(&path, ["add", "a.txt"]);
        self.run(
            &path,
            [
                // Per invocation rather than per repository, so a fixture is one command
                // fewer and the settings cannot be missed on a repository somebody adds.
                "-c",
                "user.name=Nysia Test",
                "-c",
                "user.email=test@nysia.invalid",
                // A machine with signing on globally would otherwise wait for a passphrase.
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ],
        );
        path
    }

    /// A repository at `relative` with no commits: `git init` and nothing else.
    pub(crate) fn empty_repository(&self, relative: &str) -> PathBuf {
        let path = self.folder(relative);
        self.run(&path, ["init", "--initial-branch", Self::BRANCH]);
        path
    }

    /// Run git in `at`, panicking with git's own message if it fails.
    fn run<I, S>(&self, at: &Path, args: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        let command = GitCommand::new(args);
        let at = CanonicalPath::of(at).expect("the fixture directory exists");
        self.git
            .run(&command, &at)
            .unwrap_or_else(|err| panic!("building a fixture failed: {err}"));
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove_tree(&self.root);
    }
}

/// Delete a tree, clearing the read-only bit git puts on loose objects first.
///
/// `remove_dir_all` refuses a read-only file on Windows, and every repository with a commit
/// in it has a directory full of them under `.git/objects`. Without this the first fixture
/// that commits leaves its scratch directory on disk for good.
fn remove_tree(root: &Path) {
    if !root.exists() {
        return;
    }
    clear_read_only(root);
    // A best-effort delete: a file a virus scanner still has open is not a test failure, and
    // the directory name carries a pid so the next run will not collide with it.
    std::fs::remove_dir_all(root).ok();
}

/// Clear the read-only bit on every file in a tree.
fn clear_read_only(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => clear_read_only(&path),
            Ok(_) => {
                if let Ok(metadata) = std::fs::metadata(&path) {
                    let mut permissions = metadata.permissions();
                    #[expect(
                        clippy::permissions_set_readonly_false,
                        reason = "clearing the bit is the point: git writes loose objects read-only and remove_dir_all refuses them on Windows"
                    )]
                    permissions.set_readonly(false);
                    std::fs::set_permissions(&path, permissions).ok();
                }
            }
            Err(_) => {}
        }
    }
}
