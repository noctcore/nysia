//! Why a git question could not be answered.
//!
//! Two types rather than one, because they answer different questions and a caller usually
//! wants only the first: [`PathError`] is about the path a person typed, and [`GitError`] is
//! about everything after it. The daemon's registration verb maps both onto v0.3 §3.2's
//! cases, and it can only do that if "the folder is not there" and "git refused to open the
//! repository that is there" arrive as different things.
//!
//! Every variant carries the path or the argument vector it failed on. A bare
//! `std::io::Error` reading `Access is denied. (os error 5)` in a daemon log that serves
//! every project on a machine says nothing about which project stopped working.

use std::path::PathBuf;
use std::time::Duration;

use crate::pty::ResolveError;

/// Why a path could not be resolved to a folder this module will look at.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// Nothing is at the path.
    #[error("{} does not exist", path.display())]
    Missing {
        /// The path as the caller spelled it, because that is what they will recognise.
        path: PathBuf,
    },
    /// Something is at the path and the OS would not resolve it.
    ///
    /// Almost always a permission on the path or one of its parents. Kept distinct from
    /// [`PathError::Missing`] because "you cannot see this folder" and "this folder is not
    /// there" send a person to different places.
    #[error("{} could not be read: {source}", path.display())]
    Unreadable {
        /// The path as the caller spelled it.
        path: PathBuf,
        /// What the OS said.
        #[source]
        source: std::io::Error,
    },
    /// The path names a file.
    ///
    /// Its own variant rather than folding into "not a repository", which would send a
    /// caller looking for a `.git` beside a file that is not in one.
    #[error("{} is a file, not a folder", path.display())]
    NotADirectory {
        /// The resolved path.
        path: PathBuf,
    },
}

/// Why a git invocation did not answer the question it was asked.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// The path could not be resolved at all. See [`PathError`].
    #[error(transparent)]
    Path(#[from] PathError),

    /// `git` is not installed, or is not on this process's `PATH`.
    ///
    /// Reported before any spawn. On Windows a bare `Command::new("git")` would fail with
    /// `NotFound` for several unrelated reasons, so the resolution happens up front and says
    /// which one (traps register #8).
    #[error("git is not installed, or not on PATH: {source}")]
    NotInstalled {
        /// What the resolver looked for and did not find.
        #[source]
        source: ResolveError,
    },

    /// The spawn itself failed.
    #[error("could not run git in {}: {source}", at.display())]
    Spawn {
        /// The working directory the spawn was given.
        at: PathBuf,
        /// What the OS said.
        #[source]
        source: std::io::Error,
    },

    /// git ran past its deadline and was killed.
    ///
    /// The variant this module exists to make possible. A git invocation hangs on a lock,
    /// on a network remote, or on a credential prompt, and under D-1 the daemon it hangs
    /// lives for days — so every invocation carries a deadline and a hang becomes this
    /// rather than a daemon nobody can explain.
    #[error("git {args} in {} did not finish within {timeout:?}", at.display())]
    TimedOut {
        /// The argument vector, without the program.
        args: String,
        /// The working directory.
        at: PathBuf,
        /// The deadline it passed.
        timeout: Duration,
    },

    /// git ran and reported a failure.
    ///
    /// Carries git's own stderr rather than a message this module invented. Classifying by
    /// message is what makes an error type wrong the first time git rewords something, and
    /// the cases worth telling apart — a missing repository, dubious ownership, a `.git`
    /// file pointing nowhere — are told apart by [`super::Folder`] from the filesystem
    /// instead.
    #[error("git {args} in {} failed ({status}): {stderr}", at.display())]
    Failed {
        /// The argument vector, without the program.
        args: String,
        /// The working directory.
        at: PathBuf,
        /// How it exited: an exit code, or a description of the signal that ended it.
        status: String,
        /// git's stderr, trimmed.
        stderr: String,
    },

    /// git exited 129, which is its usage error.
    ///
    /// Its own variant because it has exactly two causes and both are this module's fault
    /// rather than the user's: an argument vector built wrong, or a git too old for an
    /// option used here. [`super::REQUIRED_OPTIONS`] is the list, and it is in the message
    /// so that the second cause does not have to be guessed at.
    #[error(
        "git {args} was rejected as a usage error; this build needs {}: {stderr}",
        super::REQUIRED_OPTIONS.join(", ")
    )]
    Usage {
        /// The argument vector, without the program.
        args: String,
        /// git's stderr, trimmed.
        stderr: String,
    },

    /// git's output was not the shape its `--porcelain` contract promises.
    ///
    /// Never inferred over: a worktree list this module could not parse is reported rather
    /// than silently returned short, because a short list is how a worktree gets registered
    /// twice.
    #[error("git {args} printed {problem}")]
    Unparsable {
        /// The argument vector, without the program.
        args: String,
        /// What was wrong with the output.
        problem: String,
    },

    /// The folder looks like a repository and git would not open it.
    ///
    /// Dubious ownership, a `.git` file pointing at a directory that is gone, a corrupt
    /// `HEAD`. Distinguished from "not a repository" by the filesystem rather than by
    /// reading git's message: the folder has a `.git` entry, or the layout of a bare
    /// repository, so answering `NoRepository` for it would be a lie a person cannot act on.
    #[error("{} looks like a git repository, and git would not open it: {stderr}", path.display())]
    Refused {
        /// The folder.
        path: PathBuf,
        /// git's stderr, trimmed, which is where the actual reason is.
        stderr: String,
    },
}
