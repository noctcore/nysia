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

/// A resolved gh, or `None` on a machine that has none.
///
/// [`git_or_skip`]'s reasoning, with one difference worth stating: **gh is not assumed to be
/// on either CI leg**, so a test using this may legitimately never run there. That makes it a
/// weaker gate than the git ones, and it is why the reading of every ending gh can produce is
/// covered by `gh::tests` as pure functions over measured fixtures — those run everywhere.
/// What this adds on top is the one thing a fixture cannot prove: that the argument vector
/// and the environment policy really do produce that ending from a real gh.
pub(crate) fn gh_or_skip() -> Option<super::gh::Gh> {
    super::gh::Gh::locate().ok()
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
    /// See [`unique_name`] for why the name is built rather than taken from the thread id
    /// directly: one half of that is parallelism, and the other half cost an afternoon.
    pub(crate) fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(unique_name(tag));
        remove_tree(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self {
            root,
            git: Git::locate().expect("git, which the caller checked for with `git_or_skip`"),
        }
    }

    /// The scratch directory itself.
    pub(crate) fn root(&self) -> &Path {
        &self.root
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

    /// A bare repository at `relative`.
    pub(crate) fn bare_repository(&self, relative: &str) -> PathBuf {
        let path = self.folder(relative);
        self.run(&path, ["init", "--bare", "--initial-branch", Self::BRANCH]);
        path
    }

    /// A git directory at `path`, not attached to any working tree.
    ///
    /// What a submodule's `.git` file points at, under the superproject's `.git/modules`.
    pub(crate) fn init_git_dir(&self, path: &Path) {
        std::fs::create_dir_all(path).expect("a folder for the git directory");
        self.run(path, ["init", "--bare", "--initial-branch", Self::BRANCH]);
    }

    /// A program that touches a marker when it runs, and prints nothing.
    ///
    /// Returns the program and the marker it will touch. **Each caller gets its own pair**,
    /// which is not tidiness: three programs sharing one marker makes a failing neutraliser
    /// indistinguishable from a passing one, because whichever program still runs touches the
    /// file every assertion is reading. Removing `core.fsmonitor` from the list and watching
    /// the `core.hooksPath` assertion fail is how that was found.
    ///
    /// A `/bin/sh` script on both platforms. Git for Windows runs hooks and config-named
    /// programs through its own bundled `sh`, so one spelling covers both CI legs — which is
    /// also why the marker path is written with forward slashes and quoted, since a backslash
    /// is an escape inside a shell script and a runner's temp directory is not this fixture's
    /// to promise free of spaces.
    pub(crate) fn marker_program(&self, name: &str) -> (PathBuf, PathBuf) {
        let program = self.root.join(format!("{name}.sh"));
        let marker = self.root.join(format!("{name}.fired"));
        std::fs::write(
            &program,
            format!("#!/bin/sh\ntouch \"{}\"\n", forward_slashes(&marker)),
        )
        .expect("write the marker program");
        make_executable(&program);
        (program, marker)
    }

    /// Write settings into a repository's **own** `.git/config`.
    ///
    /// Appended to the file rather than set through `git config`, because that is the shape of
    /// the threat the neutralisers exist for: an agent working in a checkout can write this
    /// file, and `-c` is what has to outrank it. Repeating a section header is legal in a git
    /// config, so one entry per setting needs no grouping.
    ///
    /// Values are written with forward slashes. A backslash is an escape character in a git
    /// config value, so `C:\Users` would be read as an invalid escape rather than a path.
    pub(crate) fn poison_config(&self, repo: &Path, settings: &[(&str, &str, &Path)]) {
        let config = repo.join(".git").join("config");
        let mut text = std::fs::read_to_string(&config).expect("the fixture has a .git/config");
        for (section, key, value) in settings {
            let value = forward_slashes(value);
            text.push_str(&format!("\n[{section}]\n\t{key} = {value}\n"));
        }
        std::fs::write(&config, text).expect("write .git/config");
    }

    /// Add a linked worktree of `repo` at `path`, on a new branch.
    pub(crate) fn add_worktree(&self, repo: &Path, path: &Path, branch: &str) {
        let command = GitCommand::new(["worktree", "add", "-b", branch]).operand_path(path);
        let at = CanonicalPath::of(repo).expect("the repository is a folder");
        self.git
            .run(&command, &at)
            .unwrap_or_else(|err| panic!("git worktree add failed: {err}"));
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

/// A directory name unique to this test, and safe for a path a shell will read.
///
/// Two separate requirements, and the second is not obvious.
///
/// **Unique**, because `cargo test` runs tests in parallel threads of one process: a shared
/// name is a fixture two tests delete from under each other, which passes alone and fails in a
/// suite. The pid and the thread id together give that.
///
/// **Free of shell metacharacters**, because this crate's whole subject is spawning programs.
/// The obvious spelling of a thread id is `format!("{:?}")`, which produces `ThreadId(191)` —
/// and git runs a program named by `core.fsmonitor` or `diff.external` *through a shell*, so
/// a fixture living under a directory with parentheses in it silently fails to launch
/// anything. That is exactly how `the_neutralisers_stop_programs_the_repository_config_names`
/// first failed: its control did not fire, and the neutraliser it was testing was fine. Only
/// the digits are kept.
pub(crate) fn unique_name(tag: &str) -> String {
    let thread = format!("{:?}", std::thread::current().id());
    let digits: String = thread.chars().filter(|c| c.is_ascii_digit()).collect();
    format!("nysia-git-{tag}-{}-t{digits}", std::process::id())
}

/// A path spelled the way a shell script and a git config value both read it.
pub(crate) fn forward_slashes(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Give a file an execute bit where the platform has one.
///
/// Windows decides by extension and by what `sh` is willing to run, so there is nothing to
/// set; macOS will not run a hook without it, and git reports that as the hook simply not
/// existing.
pub(crate) fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("mark the fixture program executable");
    }
    #[cfg(not(unix))]
    {
        let _ = path;
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
