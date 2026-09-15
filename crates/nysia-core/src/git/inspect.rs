//! What git says a folder is.
//!
//! v0.3 §3.2 asks the daemon to decide what it is looking at and say so rather than guess,
//! and names the cases: a git repository, a folder containing several repositories, a folder
//! that is no repository, and a path that does not exist or cannot be read. This module
//! answers the first three as [`Folder`] and the fourth as [`PathError`], because a caller
//! that has to tell "this folder has no git in it" from "this folder is not there" should
//! not have to read a message to do it.
//!
//! Two shapes the plan does not name turn up immediately and are given their own answers:
//!
//! - **A file.** [`PathError::NotADirectory`], not "no repository" — which would send
//!   somebody looking for a `.git` beside a file that is not in one.
//! - **A folder that looks like a repository and git will not open it.**
//!   [`GitError::Refused`], carrying git's own stderr. Dubious ownership is the common one
//!   (a repository cloned by another user, or restored from a backup), and a `.git` file
//!   pointing at a directory that no longer exists is the other. Both exit 128 exactly as a
//!   plain folder does, so telling them apart by reading git's message would be a
//!   classification that breaks the first time git rewords something. The filesystem decides
//!   instead: a `.git` entry, or the layout of a bare repository, means the folder is one and
//!   git's refusal is the news.
//!
//! # A folder of repositories
//!
//! The common shape of a `Projekty/` directory, and Orca's own dialog calls it out. This
//! module **reports the list and registers nothing** — the decision between registering each,
//! registering none, and asking belongs to the verb, and asking is the only one of the three
//! that is not a guess about what somebody meant.
//!
//! The children are classified from the filesystem alone, with no `git` spawn each. A
//! hundred-folder `Projekty/` would otherwise cost a hundred process launches to draw one
//! dialog, and every one of them is confirmed properly the moment it is actually registered.
//! The difference is visible only for a child that looks like a repository and is not one,
//! which then answers [`GitError::Refused`] at registration instead of being absent from the
//! list.

use std::path::PathBuf;

use super::command::{Git, GitCommand};
use super::error::{GitError, PathError};
use super::path::CanonicalPath;
use crate::worktree::{self, Worktree};

/// The most immediate children a folder-of-repositories scan will look at.
///
/// A bound rather than a guess at what is enough. `read_dir` on a directory somebody pointed
/// at by accident — a drive root, a `node_modules` — should cost a bounded amount of work,
/// and [`Folder::ManyRepositories`] says when it stopped early rather than quietly returning
/// a short list.
const MAX_SCANNED_ENTRIES: usize = 4096;

/// What a folder turned out to be.
#[derive(Debug)]
pub enum Folder {
    /// The folder is a git repository, or is inside one.
    ///
    /// "Inside one" is not folded away: pointing at `…/nysia/crates/nysia-core` reports the
    /// repository that contains it, with [`Repository::requested`] still naming the folder
    /// that was asked about, so a caller can offer the repository root rather than silently
    /// registering something the person did not choose.
    Repository(Box<Repository>),
    /// The folder is not a repository, and repositories sit directly inside it.
    ManyRepositories {
        /// The children that look like repositories, sorted, deduplicated by canonical path.
        repositories: Vec<CanonicalPath>,
        /// Whether the scan stopped at [`MAX_SCANNED_ENTRIES`], so this is not all of them.
        truncated: bool,
    },
    /// The folder is readable, is not a repository, and has none directly inside it.
    NoRepository,
}

/// A git repository, as much of it as registering a project needs.
#[derive(Debug)]
pub struct Repository {
    /// The folder that was inspected, which may be below the repository root.
    pub requested: CanonicalPath,
    /// `.git` for this worktree — a directory in the main worktree, a file's target in a
    /// linked one.
    pub git_dir: PathBuf,
    /// The git directory shared by every worktree of this repository.
    ///
    /// This, not [`Repository::git_dir`], is what identifies a repository: every linked
    /// worktree has its own `git_dir` and they all share this one. Registering a linked
    /// worktree and registering the main one are therefore recognisable as the same
    /// repository.
    pub common_dir: PathBuf,
    /// Whether the repository has no working tree of its own.
    pub is_bare: bool,
    /// Every worktree, main first, with the one containing [`Repository::requested`] primary.
    pub worktrees: Vec<Worktree>,
}

impl Repository {
    /// The checkout this repository was inspected from (v0.3 §3.1's `is_primary`).
    ///
    /// `None` when no listed worktree contains the folder that was inspected — a bare
    /// repository reached from somewhere other than its own directory, for instance, since the
    /// only worktree git lists for one is the git directory itself.
    #[must_use]
    pub fn primary(&self) -> Option<&Worktree> {
        self.worktrees.iter().find(|worktree| worktree.is_primary)
    }

    /// git's main worktree — the one holding the real git directory.
    ///
    /// Not the same as [`Repository::primary`]: registering a linked worktree makes that one
    /// primary and leaves this one where it was. The worktree manager needs this one, because
    /// it is the worktree `git worktree remove` refuses.
    #[must_use]
    pub fn main_worktree(&self) -> Option<&Worktree> {
        self.worktrees.iter().find(|worktree| worktree.is_main)
    }

    /// The branch the primary checkout is on, which is what a worktree is keyed by (D-6).
    #[must_use]
    pub fn primary_branch(&self) -> Option<&str> {
        self.primary().and_then(|worktree| worktree.head.branch())
    }
}

/// Decide what `path` is.
///
/// # Errors
///
/// - [`PathError`], through [`GitError::Path`], when the path is missing, unreadable, or a
///   file.
/// - [`GitError::Refused`] when the folder looks like a repository and git would not open it.
/// - Any other [`GitError`] when git could not be run or did not finish.
pub fn inspect(git: &Git, path: impl AsRef<std::path::Path>) -> Result<Folder, GitError> {
    let at = CanonicalPath::of(path)?;
    inspect_folder(git, &at)
}

/// Decide what an already-resolved folder is.
pub fn inspect_folder(git: &Git, at: &CanonicalPath) -> Result<Folder, GitError> {
    match probe(git, at)? {
        Some(probed) => Ok(Folder::Repository(Box::new(Repository {
            worktrees: worktree::list(git, at)?,
            requested: at.clone(),
            git_dir: probed.git_dir,
            common_dir: probed.common_dir,
            is_bare: probed.is_bare,
        }))),
        None => scan_children(at),
    }
}

/// What the `rev-parse` probe reported.
struct Probed {
    /// This worktree's git directory, absolute.
    git_dir: PathBuf,
    /// The git directory shared by every worktree, absolute.
    common_dir: PathBuf,
    /// Whether the repository is bare.
    is_bare: bool,
}

/// Ask git whether `at` is in a repository, and refuse to guess when it will not say.
///
/// `--show-toplevel` is deliberately not used: it fails outright in a bare repository, so a
/// probe built on it reports "not a repository" for one. `--git-common-dir` answers for every
/// shape, and the worktree root comes from the worktree list instead.
///
/// `Ok(None)` means "git says this is not a repository, and the filesystem agrees". A
/// disagreement is [`GitError::Refused`].
fn probe(git: &Git, at: &CanonicalPath) -> Result<Option<Probed>, GitError> {
    let command = GitCommand::new([
        "rev-parse",
        // Absolute, because a relative `.git` is relative to a working directory the caller
        // does not have and would resolve against the daemon's own.
        "--path-format=absolute",
        "--git-common-dir",
        "--git-dir",
        "--is-bare-repository",
    ]);
    let finished = git.capture(&command, at)?;

    if let Some(err) = finished.failure(&command, at, git.timeout()) {
        return classify_failure(err, at, looks_like_repository(at)).map(|()| None);
    }

    let stdout = String::from_utf8_lossy(&finished.stdout);
    let mut lines = stdout.lines();
    let mut next = |what: &str| -> Result<String, GitError> {
        lines
            .next()
            .map(str::to_owned)
            .ok_or_else(|| GitError::Unparsable {
                args: "rev-parse --git-common-dir --git-dir --is-bare-repository".to_owned(),
                problem: format!("no {what}"),
            })
    };
    let common_dir = next("git common dir")?;
    let git_dir = next("git dir")?;
    let is_bare = next("bare flag")?;

    Ok(Some(Probed {
        git_dir: resolved(&git_dir),
        common_dir: resolved(&common_dir),
        // Anything other than the two words git documents is a git that changed its
        // contract, and defaulting to "not bare" would quietly treat a bare repository as a
        // checkout.
        is_bare: match is_bare.as_str() {
            "true" => true,
            "false" => false,
            other => {
                return Err(GitError::Unparsable {
                    args: "rev-parse --is-bare-repository".to_owned(),
                    problem: format!("{other:?} for --is-bare-repository, not true or false"),
                });
            }
        },
    }))
}

/// Decide what a failed probe means, given whether the folder looks like a repository.
///
/// `Ok(())` is "git says this is not a repository, and the filesystem agrees" — the only
/// reading under which a failure is news about the folder rather than about git.
///
/// Split out and pure because it is the decision that goes wrong quietly. Reading *every*
/// failure as "not a repository" would turn a git too old for one of
/// [`super::REQUIRED_OPTIONS`] — which exits 129 for every folder alike — into a machine on
/// which no repository is one, and a timeout into the same.
fn classify_failure(
    err: GitError,
    at: &CanonicalPath,
    looks_like_repository: bool,
) -> Result<(), GitError> {
    match err {
        // git ran, understood the question, and would not answer it. Only here does the
        // filesystem get to decide.
        GitError::Failed { stderr, .. } => {
            if looks_like_repository {
                Err(GitError::Refused {
                    path: at.as_path().to_path_buf(),
                    stderr,
                })
            } else {
                Ok(())
            }
        }
        // A deadline, a usage error, a git that could not be spawned. None of these is news
        // about the folder, and reading one as "not a repository" would tell a person their
        // repository is not one.
        other => Err(other),
    }
}

/// A path git printed, canonicalised where it can be.
///
/// git prints `C:/Users/kacpe/repo/.git` on Windows and the rest of this module speaks
/// `C:\Users\kacpe\repo\.git`; comparing the two as strings is how one repository becomes
/// two. Canonicalisation can fail — the git directory of a worktree whose registration
/// outlived its directory — and the path git gave is kept as it arrived in that case, because
/// a wrong-looking path is more use than none.
fn resolved(printed: &str) -> PathBuf {
    let raw = PathBuf::from(printed);
    CanonicalPath::of_git_output(&raw).map_or(raw, |path| path.as_path().to_path_buf())
}

/// Whether a folder has the shape of a repository, without asking git.
///
/// Two shapes, because there are two: a checkout has a `.git` entry — a directory in the main
/// worktree, a file in a linked one — and a bare repository has `HEAD` beside `objects` and
/// `refs`. All three of the bare markers are required: a folder with a `HEAD` file in it is
/// not a repository, and `refs` alone is a name anybody might use.
fn looks_like_repository(at: &CanonicalPath) -> bool {
    let dir = at.as_path();
    if dir.join(".git").exists() {
        return true;
    }
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

/// List the immediate children that look like repositories.
fn scan_children(at: &CanonicalPath) -> Result<Folder, GitError> {
    let entries = std::fs::read_dir(at.as_path()).map_err(|source| PathError::Unreadable {
        path: at.as_path().to_path_buf(),
        source,
    })?;

    let mut repositories = Vec::new();
    let mut seen = 0_usize;
    let mut truncated = false;
    for entry in entries {
        // One unreadable entry is not a reason to fail the whole scan: a `Projekty/` folder
        // with a stale junction in it should still list the repositories beside it.
        let Ok(entry) = entry else { continue };
        seen += 1;
        if seen > MAX_SCANNED_ENTRIES {
            truncated = true;
            break;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            // `is_dir` on the entry rather than on the path, so a symlink to a directory is
            // followed only by the canonicalisation below — which is where a link that points
            // outside, or at nothing, stops being a candidate.
            let followed = entry.path();
            if !followed.is_dir() {
                continue;
            }
        }
        let Ok(child) = CanonicalPath::of(entry.path()) else {
            continue;
        };
        if looks_like_repository(&child) {
            repositories.push(child);
        }
    }

    if repositories.is_empty() && !truncated {
        return Ok(Folder::NoRepository);
    }
    if repositories.is_empty() {
        // The scan stopped early and found nothing, which is not the same claim as "there is
        // nothing here" — so it is still the many-repositories answer, with an empty list and
        // the flag set.
        return Ok(Folder::ManyRepositories {
            repositories,
            truncated,
        });
    }
    // A stable order, because this list is drawn in a dialog and `read_dir` returns whatever
    // the filesystem feels like.
    repositories.sort();
    repositories.dedup();
    Ok(Folder::ManyRepositories {
        repositories,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{Scratch, git_or_skip};

    #[test]
    fn a_checkout_is_a_repository_with_its_branch() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("checkout");
        let repo = scratch.repository("project");

        let folder = inspect(&git, &repo).expect("a repository");
        let Folder::Repository(repository) = folder else {
            panic!("{repo:?} is a repository");
        };
        assert!(!repository.is_bare);
        assert_eq!(repository.worktrees.len(), 1);
        assert_eq!(
            repository.primary_branch(),
            Some(Scratch::BRANCH),
            "the primary checkout is on the branch it was created with"
        );
        assert!(
            repository
                .main_worktree()
                .is_some_and(|worktree| worktree.is_primary),
            "a single-worktree repository's main worktree is also the primary one"
        );
    }

    #[test]
    fn a_folder_inside_a_repository_reports_the_repository_it_is_in() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("nested");
        let repo = scratch.repository("project");
        let nested = repo.join("crates").join("inner");
        std::fs::create_dir_all(&nested).expect("nested folder");

        let Folder::Repository(repository) = inspect(&git, &nested).expect("a repository") else {
            panic!("a folder inside a repository is in one");
        };
        assert_eq!(
            repository.requested,
            CanonicalPath::of(&nested).expect("the nested folder resolves"),
            "the folder that was asked about is reported, not silently replaced by the root"
        );
        assert!(
            repository
                .primary()
                .and_then(|worktree| worktree.canonical.as_ref())
                .is_some_and(|root| root.contains(&repository.requested)),
            "the primary worktree is the one containing the folder that was inspected"
        );
    }

    #[test]
    fn a_folder_of_repositories_lists_them_and_registers_nothing() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("many");
        let projekty = scratch.folder("Projekty");
        let one = scratch.repository("Projekty/alpha");
        let two = scratch.repository("Projekty/beta");
        scratch.folder("Projekty/not-a-repo");

        let Folder::ManyRepositories {
            repositories,
            truncated,
        } = inspect(&git, &projekty).expect("a folder of repositories")
        else {
            panic!("a folder holding two repositories is not one repository");
        };
        assert!(!truncated);
        assert_eq!(
            repositories,
            vec![
                CanonicalPath::of(&one).expect("alpha"),
                CanonicalPath::of(&two).expect("beta"),
            ],
            "both repositories, sorted, and the plain folder left out"
        );
    }

    #[test]
    fn a_bare_repository_beside_others_is_still_a_repository() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("bare-child");
        let projekty = scratch.folder("Projekty");
        let bare = scratch.bare_repository("Projekty/mirror.git");

        let Folder::ManyRepositories { repositories, .. } =
            inspect(&git, &projekty).expect("a folder of repositories")
        else {
            panic!("the folder itself is not a repository");
        };
        assert_eq!(
            repositories,
            vec![CanonicalPath::of(&bare).expect("the bare repository")],
            "a bare repository has no .git, so the HEAD/objects/refs layout has to find it"
        );
    }

    #[test]
    fn a_bare_repository_reports_itself_as_bare() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("bare");
        let bare = scratch.bare_repository("mirror.git");

        let Folder::Repository(repository) = inspect(&git, &bare).expect("a repository") else {
            panic!("a bare repository is a repository");
        };
        assert!(repository.is_bare);
        assert_eq!(
            repository.worktrees.len(),
            1,
            "git lists the bare repository itself as its one worktree"
        );
        assert_eq!(
            repository.worktrees[0].head,
            crate::worktree::Head::Bare,
            "there is no branch checked out anywhere"
        );
    }

    #[test]
    fn a_repository_with_no_commits_is_a_repository_on_an_unborn_branch() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("unborn");
        let repo = scratch.empty_repository("fresh");

        let Folder::Repository(repository) = inspect(&git, &repo).expect("a repository") else {
            panic!("`git init` and nothing else is still a repository");
        };
        assert_eq!(
            repository.worktrees[0].head,
            crate::worktree::Head::Unborn {
                name: Scratch::BRANCH.to_owned()
            },
            "HEAD exists and points nowhere"
        );
        assert_eq!(repository.primary_branch(), Some(Scratch::BRANCH));
    }

    #[test]
    fn a_plain_folder_is_no_repository() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("plain");
        let folder = scratch.folder("just-a-folder");

        assert!(
            matches!(
                inspect(&git, &folder).expect("a readable folder"),
                Folder::NoRepository
            ),
            "an empty folder with nothing in it is no repository"
        );
    }

    #[test]
    fn a_missing_path_and_a_file_are_their_own_answers() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("shapes");
        let file = scratch.root().join("README.md");
        std::fs::write(&file, "not a folder").expect("write");

        let missing = inspect(&git, scratch.root().join("nowhere")).unwrap_err();
        assert!(
            matches!(missing, GitError::Path(PathError::Missing { .. })),
            "{missing:?}"
        );
        let not_a_folder = inspect(&git, &file).unwrap_err();
        assert!(
            matches!(
                not_a_folder,
                GitError::Path(PathError::NotADirectory { .. })
            ),
            "{not_a_folder:?}"
        );
    }

    #[test]
    fn a_folder_that_looks_like_a_repository_and_will_not_open_says_so() {
        // The fifth shape. A `.git` file pointing at a directory that is not there exits 128
        // exactly as a plain folder does, and answering `NoRepository` for it would tell a
        // person their repository is not one.
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("broken");
        let broken = scratch.folder("broken");
        std::fs::write(broken.join(".git"), "gitdir: /nowhere/at/all\n").expect("write .git");

        let err = inspect(&git, &broken).unwrap_err();
        let GitError::Refused { stderr, .. } = err else {
            panic!("a folder with a .git in it is not `NoRepository`: {err:?}");
        };
        assert!(
            !stderr.is_empty(),
            "git's own reason is the news, and it has to reach the caller"
        );
    }

    #[test]
    fn only_a_failure_git_understood_is_read_as_not_a_repository() {
        // The mistake this function exists to prevent, and the one a probe that classified on
        // its own would make: a git without one of `REQUIRED_OPTIONS` exits 129 for every
        // folder, so folding that into "not a repository" makes a whole machine repositoryless
        // and tells a person their repository is not one. Replace the `other => Err(other)`
        // arm with `_ => Ok(())` and the last two assertions go red.
        let scratch = Scratch::new("classify");
        let at = CanonicalPath::of(scratch.root()).expect("the scratch directory is a folder");

        let failed = || GitError::Failed {
            args: "rev-parse".to_owned(),
            at: at.as_path().to_path_buf(),
            status: "exit code 128".to_owned(),
            stderr: "fatal: not a git repository".to_owned(),
        };

        assert!(
            classify_failure(failed(), &at, false).is_ok(),
            "a folder with no .git that git refuses is simply not a repository"
        );
        assert!(
            matches!(
                classify_failure(failed(), &at, true),
                Err(GitError::Refused { .. })
            ),
            "a folder that looks like a repository and is refused is not `NoRepository`"
        );
        assert!(
            matches!(
                classify_failure(
                    GitError::Usage {
                        args: "rev-parse".to_owned(),
                        stderr: "error: unknown option `path-format=absolute'".to_owned(),
                    },
                    &at,
                    false
                ),
                Err(GitError::Usage { .. })
            ),
            "a git too old for the probe is news about git, not about the folder"
        );
        assert!(
            matches!(
                classify_failure(
                    GitError::TimedOut {
                        args: "rev-parse".to_owned(),
                        at: at.as_path().to_path_buf(),
                        timeout: std::time::Duration::from_secs(1),
                    },
                    &at,
                    false
                ),
                Err(GitError::TimedOut { .. })
            ),
            "a deadline is not an answer about the folder either"
        );
    }

    #[test]
    fn a_folder_named_like_a_flag_never_becomes_one() {
        // "Does a folder named like a flag reach git as an argument?" No: the folder is the
        // child's working directory and git's option parser never sees it. If this module
        // ever passed the path with `-C` or as a positional, `--upload-pack` would be a real
        // remote-command injection.
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("flaggy");
        let hostile = scratch.repository("--upload-pack=calc");

        let Folder::Repository(repository) = inspect(&git, &hostile).expect("a repository") else {
            panic!("a folder with a hostile name is still a repository");
        };
        assert_eq!(repository.primary_branch(), Some(Scratch::BRANCH));
        assert!(
            repository
                .requested
                .folder_name()
                .is_some_and(|name| name.starts_with("--upload-pack")),
            "the folder really is named like a flag, or this test proves nothing"
        );
    }

    #[test]
    fn a_worktree_is_keyed_by_its_branch_and_the_registered_one_is_primary() {
        // D-6, and the distinction between `is_main` and `is_primary`: registering a linked
        // worktree makes that one primary and leaves the main worktree where it was.
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("linked");
        let repo = scratch.repository("project");
        let linked = scratch.root().join("feature-checkout");
        scratch.add_worktree(&repo, &linked, "feature");

        let Folder::Repository(from_linked) = inspect(&git, &linked).expect("a repository") else {
            panic!("a linked worktree is in a repository");
        };
        assert_eq!(from_linked.worktrees.len(), 2);
        assert_eq!(from_linked.primary_branch(), Some("feature"));
        assert_eq!(
            from_linked
                .main_worktree()
                .and_then(|worktree| worktree.head.branch()),
            Some(Scratch::BRANCH),
            "the main worktree is still on the branch it was on"
        );
        assert!(
            from_linked
                .main_worktree()
                .is_some_and(|worktree| !worktree.is_primary),
            "registering the linked worktree must not also mark the main one"
        );

        // And from the other end: the same repository, the other worktree primary.
        let Folder::Repository(from_main) = inspect(&git, &repo).expect("a repository") else {
            panic!("the main worktree is in a repository");
        };
        assert_eq!(from_main.primary_branch(), Some(Scratch::BRANCH));
        assert_eq!(
            from_main.common_dir, from_linked.common_dir,
            "both worktrees share one git directory, which is what identifies the repository"
        );
    }

    #[test]
    fn a_worktree_whose_directory_was_deleted_keeps_its_branch_and_loses_its_path() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("deleted");
        let repo = scratch.repository("project");
        let doomed = scratch.root().join("doomed");
        scratch.add_worktree(&repo, &doomed, "doomed");
        std::fs::remove_dir_all(&doomed).expect("delete the worktree directory");

        let Folder::Repository(repository) = inspect(&git, &repo).expect("a repository") else {
            panic!("deleting a linked worktree does not unmake the repository");
        };
        let gone = repository
            .worktrees
            .iter()
            .find(|worktree| worktree.head.branch() == Some("doomed"))
            .expect("the registration under .git/worktrees survives the directory");
        assert!(
            gone.canonical.is_none(),
            "the directory is gone, so there is no canonical path for it"
        );
        assert!(
            gone.prunable.is_some(),
            "git says it can be pruned, and that is the news a caller acts on"
        );
        assert!(
            !gone.is_primary,
            "a worktree with no directory cannot be the one you are in"
        );
    }

    #[test]
    fn a_submodule_is_its_own_repository_with_its_own_git_directory() {
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("submodule");
        let outer = scratch.repository("outer");
        // A submodule's checkout carries a `.git` *file* pointing into the superproject's
        // `.git/modules`, which is the same shape a linked worktree has and a different
        // repository from the one around it.
        let inner = outer.join("vendor").join("inner");
        std::fs::create_dir_all(&inner).expect("submodule folder");
        let modules = outer.join(".git").join("modules").join("inner");
        scratch.init_git_dir(&modules);
        std::fs::write(
            inner.join(".git"),
            format!(
                "gitdir: {}\n",
                modules.display().to_string().replace('\\', "/")
            ),
        )
        .expect("write the submodule .git file");

        let Folder::Repository(repository) = inspect(&git, &inner).expect("a repository") else {
            panic!("a submodule checkout is a repository");
        };
        let outer_common = match inspect(&git, &outer).expect("the superproject") {
            Folder::Repository(outer) => outer.common_dir,
            other => panic!("the superproject is a repository: {other:?}"),
        };
        assert_ne!(
            repository.common_dir, outer_common,
            "a submodule is a separate repository, not a worktree of the one around it"
        );
    }
}
