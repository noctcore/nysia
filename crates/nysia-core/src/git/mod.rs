//! The single git chokepoint.
//!
//! Every `git` spawn in Nysia goes through this module (D-15). One place to harden means
//! one place to audit: argument construction, the confinement check on the working
//! directory, timeouts, and the credential environment.
//!
//! The daemon also watches each repository and *pushes* status and diff to clients rather
//! than answering polls, so that the UI never blocks on a git invocation.
//!
//! # What is here now
//!
//! The library v0.3's project registration is built on (§3.2): **what git says about a
//! folder.** No daemon wiring, no store, no verb — those are waves B and C. This is the
//! thing they call.
//!
//! - [`Git::locate`] resolves `git` once, up front, and says so when there is none.
//! - [`inspect`] answers what a folder is: a repository, a folder of repositories, neither,
//!   or a path that is not there. See [`Folder`].
//! - [`Repository`] carries the worktrees, the branch each is on, and which one the folder
//!   was registered from — [`crate::worktree`] owns those, keyed by branch and never by a
//!   task id (D-6).
//! - [`CanonicalPath`] is the path identity a project's id is derived from, and the type
//!   every spawn takes as its working directory.
//!
//! Status and diff, the watcher, and worktree creation are not here yet.
//!
//! # How the hardening is arranged
//!
//! Two layers, split along the line between *running a program safely* and *what one
//! particular program's arguments and environment have to be*.
//!
//! - [`runner`] owns the first: resolve-once, the working directory as a type, the deadline,
//!   the tree-kill that enforces it, the two-thread drain, and the split between "the child
//!   exited" and "the pipe reached EOF". It knows nothing about any program.
//! - [`command`] owns git's half and is worth reading before adding a verb: the `-c`
//!   neutralisers that keep a repository's own `.git/config` from naming a program git will
//!   run, and the `GIT_*` scrub that keeps this process's environment from redirecting the
//!   answer.
//!
//! Two of git's settings are load-bearing in a way that is not obvious from the names.
//! `GIT_TERMINAL_PROMPT=0` exists because a credential prompt is the worst failure available
//! to this module — it hangs, invisibly, in a daemon that lives for days (D-1). And the
//! `GIT_DIR` scrub is not hygiene: with `GIT_DIR` set in the daemon's environment,
//! `git rev-parse` in a folder that is not a repository at all exits 0 and reports the
//! repository the variable names, so a plain folder would register as a project pointing at
//! somebody else's git directory.
//!
//! The split exists because **`gh` is a spawn with the same four concerns and a different
//! policy.** D-5 queries GitHub Issues live, which is a program that reaches the network and
//! authenticates, so it needs the deadline and the tree-kill more sharply than git does — and
//! it cannot use git's argument vector, which prepends `-c` pairs and `--no-pager` that gh
//! would reject before reading the verb. Writing a second spawner was the alternative, and
//! [`runner`]'s own documentation says why it was not taken.
//!
//! An environment list belongs to its program and never to the runner. That is not tidiness:
//! git's scrub list is safe for gh only by accident, and a shared one that grew toward
//! "anything naming a credential" would take `GH_TOKEN` with it and leave every machine
//! permanently unauthenticated. See [`gh::GH_CREDENTIAL_VARS`], which makes scrubbing one a
//! compile error rather than a convention.
//!
//! # What this module needs from git
//!
//! Named as options rather than as a version number, because that is the fact this crate can
//! actually check — see [`REQUIRED_OPTIONS`]. A git without one of them exits 129, which
//! arrives as [`GitError::Usage`] carrying the list rather than as a mysterious parse
//! failure.

pub(crate) mod command;
mod error;
pub mod gh;
mod inspect;
mod path;
pub(crate) mod runner;
#[cfg(test)]
pub(crate) mod testing;

pub use command::{DEFAULT_TIMEOUT, FORCED_VARS, Git, NEUTRALISED_CONFIG, SCRUBBED_VARS};
pub use error::{GitError, PathError};
pub use gh::{Gh, GhFailure};
pub use inspect::{Folder, Repository, inspect, inspect_folder};
pub use path::CanonicalPath;

/// The git options this module's questions are built on.
///
/// Stated as options rather than as "git 2.x or newer": the version a given option arrived in
/// is a fact about git's history that nothing in this repository can verify, and the options
/// themselves are checkable by reading the code that uses them. They are listed here so that
/// [`GitError::Usage`] — git's exit code 129, which is what a missing option produces — can
/// say what was expected instead of leaving somebody to guess.
pub const REQUIRED_OPTIONS: &[&str] = &[
    // `inspect`'s probe. Without `--path-format=absolute` a `.git` comes back relative to a
    // working directory the caller does not have.
    "rev-parse --path-format=absolute --git-common-dir --git-dir --is-bare-repository",
    // The worktree list. `-z` rather than the line-oriented form because a worktree path may
    // contain a newline, and because without it git escapes and quotes a lock reason per
    // `core.quotePath` rather than giving it plainly.
    "worktree list --porcelain -z",
    // `Start ->`'s three. `check-ref-format` decides whether a branch name is one git
    // accepts, so that this crate does not restate a subset of git's rules and refuse names
    // git would take. `for-each-ref --format` answers whether a branch already exists without
    // an exit code to interpret. `worktree add` takes `--`, which is what makes a branch or a
    // directory beginning with a dash an operand rather than an option.
    "check-ref-format refs/heads/<branch>",
    "for-each-ref --format=%(refname) <pattern>",
    "worktree add [-b <branch>] -- <path> [<commit-ish>]",
];
