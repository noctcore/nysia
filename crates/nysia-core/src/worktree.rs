//! Worktrees, keyed by branch.
//!
//! D-6: a worktree is identified by its branch and never by a task id. Task-keying caused
//! three shipped bugs in the system Nysia is replacing, and is free to get right now.
//!
//! Will own creation, discovery, adoption of a worktree that already exists on disk,
//! pruning, and the lexical confinement gates that keep an agent's file access inside the
//! worktree it was given.
//!
//! # What is here now
//!
//! **Discovery**, which is what registering a project needs (v0.3 §3.1): every worktree of a
//! repository, the branch each is on, and which one the person is looking at.
//!
//! **Creation and adoption**, which is what `Start →` needs (v0.3 §3): [`ensure`] hands back
//! the worktree for a branch, making one only when git does not already list one. Removal,
//! pruning and the confinement gates arrive with the worktree manager in v0.4 — which is why
//! nothing here deletes anything, including a worktree whose session failed to start.
//!
//! Read through [`git worktree list --porcelain -z`][porcelain]. `-z` rather than plain
//! `--porcelain` because a worktree path may contain a newline — `git worktree add
//! "$(printf 'a\nb')"` is a directory a person can make — and the line-oriented form would
//! then split one record into two. `--porcelain` and `--verbose` are mutually exclusive, and
//! the annotations `--verbose` is documented to add (`locked`, `prunable`) are present in the
//! porcelain form anyway.
//!
//! [porcelain]: https://git-scm.com/docs/git-worktree#_porcelain_format
//!
//! # The shapes this has to survive
//!
//! Each of these is a real repository somebody will point Nysia at, and each was confirmed
//! against git 2.55 rather than read off a manual page:
//!
//! - A **bare** repository lists one entry with no `HEAD` and no `branch`, only `bare`.
//! - A repository with **no commits yet** lists `HEAD 0000…0` alongside its branch: the ref
//!   exists and points nowhere. It is [`Head::Unborn`], not a detached head, and a caller
//!   that treated the zero oid as a commit would show a project checked out at nothing.
//! - A **detached** head lists `detached` and no branch.
//! - A worktree whose **directory was deleted** keeps its registration under
//!   `.git/worktrees` and is listed with `prunable`. It has a branch and no directory, so
//!   anything that resolves its path fails; [`Worktree::canonical`] is `None` for it and
//!   that is the flag to test rather than the path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::git::command::GitCommand;
use crate::git::{CanonicalPath, Git, GitError};

/// The oid git prints for a branch that has no commit yet.
const UNBORN_OID: &str = "0000000000000000000000000000000000000000";

/// The prefix a local branch's full ref carries.
const HEADS_PREFIX: &str = "refs/heads/";

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// A branch with at least one commit on it. The name is short: `main`, not
    /// `refs/heads/main`.
    Branch {
        /// The branch, which is what a worktree is keyed by (D-6).
        name: String,
        /// The commit it points at.
        commit: String,
    },
    /// A branch that exists and has no commit yet — a repository someone has just `init`ed.
    ///
    /// Its own case rather than a `Branch` with a zero commit, because every caller that
    /// wants to show or compare a commit has to know, and a zero oid that leaks into one is
    /// a commit that will never be found.
    Unborn {
        /// The branch, which is real even though it resolves to nothing.
        name: String,
    },
    /// A commit checked out directly, with no branch.
    Detached {
        /// The commit.
        commit: String,
    },
    /// A bare repository, which has no working tree and therefore nothing checked out.
    Bare,
}

impl Head {
    /// The branch this head is on, if it is on one.
    ///
    /// `Some` for an unborn branch: the branch is the key (D-6) and it exists whether or not
    /// anything has been committed to it.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch { name, .. } | Self::Unborn { name } => Some(name),
            Self::Detached { .. } | Self::Bare => None,
        }
    }
}

/// One worktree of a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The directory, as git spells it, canonicalised when it still exists.
    ///
    /// Kept even when [`Worktree::canonical`] is `None`, because a prunable worktree's
    /// registered path is the only thing that identifies it to `git worktree prune`.
    pub path: PathBuf,
    /// The resolved path, or `None` when the directory is gone.
    ///
    /// This is the field to test for "is this worktree actually on disk". git's own answer,
    /// [`Worktree::prunable`], is about its registration; the two agree in practice and this
    /// one is the one a caller can act on.
    pub canonical: Option<CanonicalPath>,
    /// What it has checked out.
    pub head: Head,
    /// Whether this is git's **main** worktree — the one holding the real `.git` directory.
    ///
    /// Not the same question as [`Worktree::is_primary`], and the difference is load-bearing
    /// for the worktree manager: the main worktree is the one `git worktree remove` refuses,
    /// whoever registered the project and from wherever.
    pub is_main: bool,
    /// Whether this is the checkout the repository was registered from (v0.3 §3.1).
    ///
    /// Decided by containment, not equality: registering `…/nysia/crates/nysia-core` marks
    /// the worktree that folder is inside. Registering a linked worktree directly makes that
    /// worktree primary and leaves `is_main` on another.
    pub is_primary: bool,
    /// Why git considers the registration removable, when it does.
    ///
    /// The case the task names: the directory was deleted and the registration under
    /// `.git/worktrees` survived it.
    pub prunable: Option<String>,
    /// The reason given to `git worktree lock`, when it is locked.
    ///
    /// `Some("")` is a lock with no reason, which is what `git worktree lock` without
    /// `--reason` records. `None` is not locked.
    pub locked: Option<String>,
}

/// Every worktree of the repository containing `at`, with the one containing `at` primary.
///
/// Public as well as being what [`crate::git::inspect`] calls, because "which branches already
/// have a worktree" is a question on its own: it is what `Start →` has to ask before it
/// creates one, and D-6 says the answer is keyed by branch.
///
/// # Errors
///
/// See [`GitError`]. A list that could not be parsed is [`GitError::Unparsable`] rather than
/// a short list: a worktree silently missing from this is a worktree that gets registered a
/// second time.
pub fn list(git: &Git, at: &CanonicalPath) -> Result<Vec<Worktree>, GitError> {
    let command = crate::git::command::GitCommand::new(["worktree", "list", "--porcelain", "-z"]);
    let stdout = git.run(&command, at)?;
    let mut worktrees = parse(&stdout).map_err(|problem| GitError::Unparsable {
        args: "worktree list --porcelain -z".to_owned(),
        problem,
    })?;
    mark_primary(&mut worktrees, at);
    Ok(worktrees)
}

/// Mark the worktree that `at` is inside, preferring the most deeply nested one.
///
/// Longest match rather than first match, because worktrees nest: a project whose worktrees
/// live at `<project>/.nysia/worktrees/<branch>` has every one of them inside the main
/// worktree, and the first match would make the main worktree primary for all of them.
fn mark_primary(worktrees: &mut [Worktree], at: &CanonicalPath) {
    let deepest = worktrees
        .iter()
        .enumerate()
        .filter(|(_, worktree)| {
            worktree
                .canonical
                .as_ref()
                .is_some_and(|path| path.contains(at))
        })
        .max_by_key(|(_, worktree)| {
            worktree
                .canonical
                .as_ref()
                .map_or(0, |path| path.as_path().components().count())
        })
        .map(|(index, _)| index);
    if let Some(index) = deepest {
        worktrees[index].is_primary = true;
    }
}

/// Where Nysia puts a worktree it creates, under the repository's main worktree.
///
/// `docs/design/2026-09-13-nysia-architecture.md` line 340 decides this — *"Keyed by branch,
/// not task (D-6). Path shape `<project>/.nysia/worktrees/<branch-slug>`"* — and
/// [`mark_primary`] was already written for it: every worktree here is inside the main one,
/// which is why that function takes the deepest match rather than the first.
///
/// Two components rather than one `".nysia/worktrees"` string, so the separator is the
/// platform's and never a literal in a path.
pub const WORKTREE_BASE: [&str; 2] = [".nysia", "worktrees"];

/// The most directory names to try before giving up on a branch.
///
/// A slug is not unique — `feat/x` and `feat-x` reduce to the same name — so a collision is
/// a real case rather than a defensive one. The suffix is only ever a *location*: a worktree
/// is keyed by its branch (D-6) and found again by asking git, never by its directory name,
/// so numbering them costs nothing that matters.
const MAX_DIRECTORY_ATTEMPTS: u32 = 9;

/// The longest a slug may be, in bytes.
///
/// Windows' classic path limit is the binding constraint and it applies to the whole path,
/// not this component — so this is not a guarantee, it is a refusal to be the reason one is
/// hit. A branch name long enough to be truncated is still keyed by its full name.
const MAX_SLUG_BYTES: usize = 60;

/// Names Windows will not give a directory, whatever the filesystem says.
///
/// `CON`, `NUL` and friends are devices in every directory, case-insensitively and with any
/// extension. `git switch -c nul` is perfectly legal, so this is reachable from an ordinary
/// branch name rather than from a hostile one — and the failure without it is
/// `CreateDirectory` refusing for a reason nothing in the error mentions.
const RESERVED_ON_WINDOWS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// What [`ensure`] found or made.
#[derive(Debug)]
pub enum Started {
    /// Nysia created the worktree.
    Created(Worktree),
    /// A worktree for that branch was already on disk, and this is it.
    ///
    /// From an earlier `Start →`, or from a `git worktree add` somebody ran themselves.
    /// Adopting rather than failing is the contract: a verb that refused here would make the
    /// second `Start →` on a branch an error to resolve by hand, in the one workflow v0.3
    /// exists to make routine.
    Adopted(Worktree),
}

impl Started {
    /// The worktree, whichever of the two happened.
    #[must_use]
    pub fn worktree(&self) -> &Worktree {
        match self {
            Self::Created(worktree) | Self::Adopted(worktree) => worktree,
        }
    }

    /// Whether the worktree was already there.
    #[must_use]
    pub const fn adopted(&self) -> bool {
        matches!(self, Self::Adopted(_))
    }
}

/// Why a branch could not become a worktree.
///
/// **No variant carries a path.** Every one of them names the branch instead, which the
/// caller supplied and already has; a worktree's directory is under the person's project and
/// is exactly what traps register #13/#14 keeps out of an envelope and a log line.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// git could not be run, or did not answer.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The branch name is one git would not accept, or one that could be read as an option.
    #[error("{branch:?} is not a branch name this can use: {reason}")]
    BranchRefused {
        /// The name as the caller spelled it.
        branch: String,
        /// Which of the two, in a few words.
        reason: &'static str,
    },
    /// git still lists a worktree for that branch and its directory is gone.
    ///
    /// Its own case because the answer is a specific command. Adoption is impossible — there
    /// is nothing to adopt — and creation is refused by git, which will not check a branch
    /// out twice. `git worktree prune` is what clears it, and saying so is the difference
    /// between a dead end and one command.
    #[error("{branch:?} has a worktree registered whose directory is gone")]
    BranchPrunable {
        /// The branch.
        branch: String,
    },
    /// Every directory name this would have used is taken.
    #[error("no free directory for {branch:?} after {MAX_DIRECTORY_ATTEMPTS} attempts")]
    NoDirectory {
        /// The branch.
        branch: String,
    },
}

/// The worktree for `branch` in the repository containing `at`, creating it if it is not there.
///
/// **Keyed by branch and by nothing else** (D-6). Adoption is decided from `git worktree
/// list`, which is repository-wide, so a worktree made outside Nysia — or in a directory
/// whose name has nothing to do with the branch — is found and used.
///
/// # What it runs, and why each one
///
/// 1. `check-ref-format refs/heads/<branch>`, so legality is **git's answer** rather than a
///    subset of git's rules restated here, which would refuse names git accepts.
/// 2. `worktree list`, to adopt.
/// 3. `for-each-ref refs/heads/<branch>`, to choose between checking the branch out and
///    creating it. Its matches are compared for equality: git's ref patterns match at `/`
///    boundaries, so `refs/heads/feat` also finds `refs/heads/feat/x`, and a prefix test
///    would have this check out a branch that does not exist.
/// 4. `worktree add`.
///
/// Four spawns for something a person clicked, each under the chokepoint's deadline.
///
/// # Errors
///
/// See [`StartError`]. A worktree that was created and then could not be used is **left
/// where it is**: removing worktrees is v0.4's verb and the destructive one, and a retry
/// adopts this one rather than making a second.
pub fn ensure(git: &Git, at: &CanonicalPath, branch: &str) -> Result<Started, StartError> {
    let refuse = |reason: &'static str| StartError::BranchRefused {
        branch: branch.to_owned(),
        reason,
    };
    if branch.is_empty() {
        return Err(refuse("it is empty"));
    }
    // **The argument-injection guard, and it is not a restatement of git's rules.**
    // `git check-ref-format refs/heads/-dashy` exits 0 — the *ref* does not begin with a
    // dash, only the branch does — so step 1 below does not catch this and cannot be asked
    // to. `worktree add -b <branch>` passes the name as a bare argument, which is the one
    // place a caller's string meets git's option parser.
    if branch.starts_with('-') {
        return Err(refuse(
            "a branch that starts with `-` would be read as an option",
        ));
    }
    if !is_valid_branch(git, at, branch)? {
        return Err(refuse("git check-ref-format refused it"));
    }

    let worktrees = list(git, at)?;
    if let Some(existing) = worktrees
        .iter()
        .find(|worktree| worktree.head.branch() == Some(branch))
    {
        return if existing.canonical.is_some() {
            Ok(Started::Adopted(existing.clone()))
        } else {
            Err(StartError::BranchPrunable {
                branch: branch.to_owned(),
            })
        };
    }

    let nysia_dir = nysia_dir(&worktrees, at);
    conceal(&nysia_dir);
    let path = free_directory(&nysia_dir.join(WORKTREE_BASE[1]), branch)?;
    let existing_branch = branch_exists(git, at, branch)?;
    let mut command = if existing_branch {
        // The branch is there and unused: check it out.
        GitCommand::new(["worktree", "add"])
    } else {
        // `-b` is the only place `branch` is a bare argument rather than an operand, which is
        // what the leading-dash guard above is for.
        GitCommand::new(["worktree", "add", "-b", branch])
    }
    .operand_path(&path);
    if existing_branch {
        // `<commit-ish>`, after the `--` that makes it an operand whatever it is spelled.
        command = command.operand_branch(branch);
    }
    git.run(&command, at)?;

    // Asked again rather than assumed. `worktree add` prints what it did and this needs the
    // parsed record — the head it landed on, whether it is the main one, the resolved path —
    // and a second `worktree list` is git's own answer to all three rather than this
    // module's guess at what it just caused.
    let worktrees = list(git, at)?;
    worktrees
        .into_iter()
        .find(|worktree| worktree.head.branch() == Some(branch))
        .map(Started::Created)
        .ok_or_else(|| {
            GitError::Unparsable {
                args: "worktree list --porcelain -z".to_owned(),
                problem: "the worktree that was just added is not in the list".to_owned(),
            }
            .into()
        })
}

/// Whether git would accept `branch` as a branch name.
///
/// `refs/heads/<branch>` rather than `--branch <branch>`: the ref form is pure syntax, where
/// `--branch` also expands `@{-1}` and friends, and embedding the name means this particular
/// spawn cannot see it as an option however it is spelled.
fn is_valid_branch(git: &Git, at: &CanonicalPath, branch: &str) -> Result<bool, GitError> {
    let command = GitCommand::new(["check-ref-format", &format!("refs/heads/{branch}")]);
    let finished = git.capture(&command, at)?;
    // `capture` and not `run`, because a non-zero exit here **is the answer** rather than a
    // failure. `failure` is still what judges everything else: a git too old for the option,
    // a timeout, a signal — classifying those from the exit code alone is the mistake
    // `Finished::failure` exists to stop each caller making separately.
    match finished.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(finished
            .failure(&command, at, git.timeout())
            .unwrap_or(GitError::Unparsable {
                args: "check-ref-format".to_owned(),
                problem: "git neither accepted nor refused the name".to_owned(),
            })),
    }
}

/// Whether a local branch of exactly this name already exists.
fn branch_exists(git: &Git, at: &CanonicalPath, branch: &str) -> Result<bool, GitError> {
    let wanted = format!("refs/heads/{branch}");
    let command = GitCommand::new(["for-each-ref", "--format=%(refname)", &wanted]);
    let stdout = git.run(&command, at)?;
    // Equality per line, never a prefix: git matches a ref pattern at `/` boundaries, so
    // `refs/heads/feat` lists `refs/heads/feat/x` as well. Treating that as a hit would have
    // `worktree add` check out a branch nobody created.
    Ok(String::from_utf8_lossy(&stdout)
        .lines()
        .any(|line| line.trim_end_matches('\r') == wanted))
}

/// The directory Nysia keeps its worktrees in, which is the one it also has to conceal.
///
/// Under the **main** worktree's root, so every worktree of a repository lands in one place
/// whichever checkout the project was registered from — registering a linked worktree must
/// not scatter a second base directory inside the first. Falls back to the folder that was
/// inspected when git lists no main worktree with a directory, which is what a bare
/// repository looks like.
fn nysia_dir(worktrees: &[Worktree], at: &CanonicalPath) -> PathBuf {
    let root = worktrees
        .iter()
        .find(|worktree| worktree.is_main)
        .and_then(|worktree| worktree.canonical.as_ref())
        .map_or_else(
            || at.as_path().to_path_buf(),
            |path| path.as_path().to_path_buf(),
        );
    root.join(WORKTREE_BASE[0])
}

/// Keep the directory Nysia makes out of the repository it makes it in.
///
/// The path shape puts worktrees **inside the main worktree**, so `.nysia/` is untracked and
/// `git add -A` stages the worktree as an *embedded git repository*: a gitlink committed by
/// accident, with git's own hint about submodules scrolling past. That is a mistake this
/// module causes and so has to prevent. A `.gitignore` of `*` inside `.nysia/` hides the
/// whole of it — including itself, which is why nothing has to be added anywhere else.
///
/// **Written inside the folder Nysia created, and nowhere else.** `.git/info/exclude` would
/// work too and is the person's file; §3.1 says Nysia does not write into the folder beyond
/// ordinary git operations, so the one thing it writes is in the directory it owns, goes when
/// they delete that directory, and changes nothing about how their repository treats anything
/// else.
///
/// Best effort, and it is not allowed to fail a `Start →`: a repository where this cannot be
/// written is one where the worktree almost certainly cannot be created either, and the
/// worktree is what was asked for. An existing file is left alone — it may be a person's.
fn conceal(nysia_dir: &Path) {
    if std::fs::create_dir_all(nysia_dir).is_err() {
        return;
    }
    let ignore = nysia_dir.join(".gitignore");
    if ignore.exists() {
        return;
    }
    if let Err(err) = std::fs::write(&ignore, "*\n") {
        // `io::Error`'s `Display` names no path, so this says what happened without saying
        // where (traps register #13/#14).
        tracing::warn!(%err, "could not hide Nysia's worktree directory from git");
    }
}

/// A directory under `base` that nothing is using yet.
fn free_directory(base: &Path, branch: &str) -> Result<PathBuf, StartError> {
    let slug = slug(branch);
    for attempt in 1..=MAX_DIRECTORY_ATTEMPTS {
        let name = if attempt == 1 {
            slug.clone()
        } else {
            format!("{slug}-{attempt}")
        };
        let candidate = base.join(&name);
        // Confinement, checked rather than reasoned about. `slug` yields one component with
        // no separator and no `..`, so this cannot fail — which is the point of asserting it
        // here rather than trusting the sanitiser to stay that way.
        if !candidate.starts_with(base) {
            break;
        }
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(StartError::NoDirectory {
        branch: branch.to_owned(),
    })
}

/// A branch name as one directory component.
///
/// The name is a **location and never a key**: `ensure` finds a worktree by asking git which
/// branch each one is on, so two branches that reduce to the same slug are told apart by
/// git rather than by their directories, and `free_directory` only has to find a free name.
/// That is what lets this be lossy without being wrong.
fn slug(branch: &str) -> String {
    let mut slug = String::with_capacity(branch.len());
    for character in branch.chars() {
        // ASCII only. A non-ASCII branch name is legal in git and its bytes are not portable
        // across the two filesystems Nysia runs on, so they become separators like anything
        // else rather than a directory one platform can name and the other cannot.
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= MAX_SLUG_BYTES {
            break;
        }
    }
    // Leading dots would make a hidden directory, and `.`/`..` would not be a new directory
    // at all — which is the whole of why this function exists rather than a `replace` call.
    let trimmed = slug.trim_matches(['-', '.'].as_slice());
    if trimmed.is_empty() {
        return "branch".to_owned();
    }
    // A reserved device name is reserved with any extension, so the test is on the stem.
    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    if RESERVED_ON_WINDOWS.contains(&stem.to_ascii_lowercase().as_str()) {
        return format!("{trimmed}-branch");
    }
    trimmed.to_owned()
}

/// Parse `git worktree list --porcelain -z`.
///
/// Records are separated by an empty NUL-terminated entry, so the stream ends `…\0\0`. Each
/// record opens with `worktree <path>` and carries some of `HEAD <oid>`, `branch <ref>`,
/// `detached`, `bare`, `locked [<reason>]` and `prunable [<reason>]`.
///
/// Pure, and separately tested, because every shape worth checking — a bare repository, an
/// unborn head, a deleted directory, a path containing a newline — is a string rather than a
/// repository that has to be built on disk first.
fn parse(stdout: &[u8]) -> Result<Vec<Worktree>, String> {
    let mut worktrees = Vec::new();
    let mut current: Option<Record> = None;

    for entry in stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            // A record boundary, or the padding after the last one.
            if let Some(record) = current.take() {
                worktrees.push(record.finish()?);
            }
            continue;
        }
        let (key, value) = split_entry(entry);
        if key == b"worktree" {
            if let Some(record) = current.take() {
                worktrees.push(record.finish()?);
            }
            current = Some(Record::new(os_path(value)));
            continue;
        }
        let Some(record) = current.as_mut() else {
            return Err(format!(
                "a {:?} attribute before any worktree",
                String::from_utf8_lossy(key)
            ));
        };
        record.attribute(key, value)?;
    }

    // A stream that did not end in the empty entry, which git always writes. Finishing the
    // record anyway rather than dropping it: a truncated last record is still a worktree,
    // and losing it is the failure this function's error exists to prevent.
    if let Some(record) = current.take() {
        worktrees.push(record.finish()?);
    }

    if let Some(first) = worktrees.first_mut() {
        // git lists the main worktree first, and the porcelain format has no other marker
        // for it. Documented under "List output" rather than merely observed.
        first.is_main = true;
    }
    Ok(worktrees)
}

/// Split `key value` on the first space; an attribute with no value is all key.
fn split_entry(entry: &[u8]) -> (&[u8], &[u8]) {
    match entry.iter().position(|byte| *byte == b' ') {
        Some(at) => (&entry[..at], &entry[at + 1..]),
        None => (entry, &[]),
    }
}

/// A path as the platform spells one.
///
/// Windows paths are UTF-16 and git prints them as UTF-8, so the lossy conversion there is
/// the conversion. On Unix a path is bytes and may not be UTF-8 at all, so the bytes are
/// kept as they arrived rather than mangled into replacement characters.
#[cfg(unix)]
fn os_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

/// A path as the platform spells one. See the Unix version for why these differ.
#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from(String::from_utf8_lossy(bytes).into_owned()))
}

/// One record of the porcelain stream, before it is known to be complete.
struct Record {
    /// The path the record opened with.
    path: PathBuf,
    /// `HEAD <oid>`, absent for a bare repository.
    commit: Option<String>,
    /// `branch <ref>`, absent for a detached head and a bare repository.
    branch: Option<String>,
    /// Whether `detached` was present.
    detached: bool,
    /// Whether `bare` was present.
    bare: bool,
    /// `prunable [<reason>]`.
    prunable: Option<String>,
    /// `locked [<reason>]`.
    locked: Option<String>,
}

impl Record {
    /// Open a record on its `worktree <path>` entry.
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            commit: None,
            branch: None,
            detached: false,
            bare: false,
            prunable: None,
            locked: None,
        }
    }

    /// Take one attribute entry.
    fn attribute(&mut self, key: &[u8], value: &[u8]) -> Result<(), String> {
        let text = || String::from_utf8_lossy(value).into_owned();
        match key {
            b"HEAD" => self.commit = Some(text()),
            b"branch" => self.branch = Some(text()),
            b"detached" => self.detached = true,
            b"bare" => self.bare = true,
            b"prunable" => self.prunable = Some(text()),
            b"locked" => self.locked = Some(text()),
            // Unknown attributes are ignored rather than refused: git adds them over time
            // and a new one is not a reason to stop answering what a folder is. An
            // unparsable *record* is still an error, which is the case that matters.
            _ => {}
        }
        Ok(())
    }

    /// Turn the record into a worktree, deciding which head it has.
    fn finish(self) -> Result<Worktree, String> {
        let head = if self.bare {
            Head::Bare
        } else {
            match (self.branch, self.commit) {
                (Some(branch), Some(commit)) if commit == UNBORN_OID => Head::Unborn {
                    name: short_branch(&branch),
                },
                (Some(branch), Some(commit)) => Head::Branch {
                    name: short_branch(&branch),
                    commit,
                },
                (None, Some(commit)) => Head::Detached { commit },
                (Some(branch), None) => {
                    return Err(format!(
                        "a worktree on {branch} with no HEAD, which the porcelain format does not produce"
                    ));
                }
                (None, None) => {
                    return Err(format!(
                        "a worktree at {} with neither a HEAD nor `bare`",
                        self.path.display()
                    ));
                }
            }
        };
        // `detached` is not consulted: it is implied by a HEAD with no branch, and trusting
        // the derived form means one code path decides rather than two that can disagree.
        Ok(Worktree {
            canonical: CanonicalPath::of_git_output(&self.path),
            path: self.path,
            head,
            is_main: false,
            is_primary: false,
            prunable: self.prunable,
            locked: self.locked,
        })
    }
}

/// `refs/heads/main` as `main`, and anything else whole.
///
/// A ref outside `refs/heads/` is kept as it arrived rather than trimmed to its last
/// component: shortening `refs/remotes/origin/main` to `main` would name a different branch.
fn short_branch(full: &str) -> String {
    full.strip_prefix(HEADS_PREFIX).unwrap_or(full).to_owned()
}

/// The path a worktree record carried, for a test that needs to build one.
#[cfg(test)]
fn record_path(path: &str) -> PathBuf {
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `-z` stream the way git writes one: every entry NUL-terminated, an empty
    /// entry between records.
    fn porcelain(records: &[&[&str]]) -> Vec<u8> {
        let mut out = Vec::new();
        for record in records {
            for entry in *record {
                out.extend_from_slice(entry.as_bytes());
                out.push(0);
            }
            out.push(0);
        }
        out
    }

    #[test]
    fn a_slug_is_one_directory_component_whatever_the_branch_is_called() {
        // A branch is keyed by its name; the slug is only where the directory goes, so it may
        // be lossy. What it may never be is more than one component, or a name that resolves
        // somewhere else.
        assert_eq!(slug("trunk"), "trunk");
        assert_eq!(slug("feat/projects"), "feat-projects");
        assert_eq!(slug("Shironex/nysia-projverbs"), "Shironex-nysia-projverbs");
        assert_eq!(slug("release/v1.2.3"), "release-v1.2.3");
        // Runs collapse rather than stacking up into `a---b`.
        assert_eq!(slug("a//b\\\\c"), "a-b-c");
        // Leading dots would make a hidden directory; `.` and `..` would not be a directory
        // at all. This is the case that makes the function more than a `replace` call.
        assert_eq!(slug(".."), "branch");
        assert_eq!(slug("."), "branch");
        assert_eq!(slug("///"), "branch");
        assert_eq!(slug(".hidden"), "hidden");
        // Non-ASCII is not portable across the two filesystems Nysia runs on.
        assert_eq!(slug("feature/café"), "feature-caf");

        for branch in [
            "feat/projects",
            "..",
            "a//b",
            ".hidden",
            "release/v1.2.3",
            "café",
        ] {
            let slug = slug(branch);
            assert_eq!(
                Path::new(&slug).components().count(),
                1,
                "{branch:?} produced {slug:?}, which is not one component"
            );
            assert!(!slug.starts_with('.'), "{slug:?} would be hidden");
        }
    }

    #[test]
    fn a_branch_named_for_a_windows_device_does_not_become_a_directory_nobody_can_make() {
        // `git switch -c nul` is legal, and `CreateDirectory("nul")` is not — on Windows a
        // device name is reserved in every directory, case-insensitively and with any
        // extension. Reached from an ordinary branch name rather than a hostile one.
        assert_eq!(slug("nul"), "nul-branch");
        assert_eq!(slug("CON"), "CON-branch");
        assert_eq!(slug("com1"), "com1-branch");
        assert_eq!(slug("nul.txt"), "nul.txt-branch");
        // Not a false positive on a name that merely starts with one.
        assert_eq!(slug("console"), "console");
        assert_eq!(slug("nullable"), "nullable");
    }

    #[test]
    fn a_slug_is_bounded_so_a_branch_name_is_never_the_reason_a_path_limit_is_hit() {
        let slug = slug(&"a/".repeat(200));
        assert!(
            slug.len() <= MAX_SLUG_BYTES,
            "{} bytes is past the cap",
            slug.len()
        );
        assert_eq!(Path::new(&slug).components().count(), 1);
    }

    #[test]
    fn a_branch_that_could_be_read_as_an_option_is_refused_before_git_sees_it() {
        let Some(git) = crate::git::testing::git_or_skip() else {
            return;
        };
        let scratch = crate::git::testing::Scratch::new("worktree-dashy");
        let repo = scratch.repository("repo");
        let at = CanonicalPath::of(&repo).expect("the repository resolves");

        // **This is not a restatement of git's rules, and the proof is that git accepts it.**
        // `git check-ref-format refs/heads/-dashy` exits 0 — the *ref* does not begin with a
        // dash, only the branch does — so the validation step cannot catch this and is not
        // asked to. `worktree add -b <branch>` is where the name meets git's option parser.
        assert!(
            is_valid_branch(&git, &at, "-dashy").expect("git answers"),
            "git itself accepts this name, which is why the guard in `ensure` is needed"
        );
        let refused = ensure(&git, &at, "-dashy");
        assert!(
            matches!(refused, Err(StartError::BranchRefused { .. })),
            "got {refused:?}"
        );

        // And a name git really will not take is refused too, by asking git rather than by
        // guessing.
        assert!(matches!(
            ensure(&git, &at, "bad..name"),
            Err(StartError::BranchRefused { .. })
        ));
        assert!(matches!(
            ensure(&git, &at, ""),
            Err(StartError::BranchRefused { .. })
        ));
    }

    #[test]
    fn a_branch_is_created_once_and_adopted_afterwards() {
        let Some(git) = crate::git::testing::git_or_skip() else {
            return;
        };
        let scratch = crate::git::testing::Scratch::new("worktree-ensure");
        let repo = scratch.repository("repo");
        let at = CanonicalPath::of(&repo).expect("the repository resolves");

        let created = ensure(&git, &at, "feat/projects").expect("the branch starts");
        assert!(!created.adopted(), "nothing was there to adopt");
        let made = created.worktree().canonical.clone().expect("it is on disk");
        assert_eq!(created.worktree().head.branch(), Some("feat/projects"));
        assert!(
            made.as_path().ends_with(Path::new("feat-projects")),
            "the architecture doc fixes the path shape: {}",
            made
        );
        // `at`, never `repo`. `made` is a `CanonicalPath` and `repo` is what `temp_dir()`
        // handed back — on macOS those are `/private/var/…` and `/var/…`, two spellings of
        // one directory that `starts_with` compares as different, because `/var` is a
        // symlink. Comparing a resolved path against an unresolved one passes on Windows and
        // on Linux and fails on exactly one runner.
        assert!(
            made.as_path()
                .starts_with(at.as_path().join(".nysia").join("worktrees")),
            "a worktree is confined to the base directory: {made}"
        );

        // The second call is the contract: adopt, never a second worktree and never a
        // failure. Keyed by the branch, so the directory name plays no part in finding it.
        let adopted = ensure(&git, &at, "feat/projects").expect("the branch starts again");
        assert!(adopted.adopted(), "the worktree was already there");
        assert_eq!(adopted.worktree().canonical.as_ref(), Some(&made));

        // An existing branch with no worktree is checked out rather than re-created, which is
        // the other half of `branch_exists`.
        git.run(&GitCommand::new(["branch", "already-there"]), &at)
            .expect("a branch with no worktree");
        let existing = ensure(&git, &at, "already-there").expect("an existing branch starts");
        assert!(!existing.adopted());
        assert_eq!(existing.worktree().head.branch(), Some("already-there"));

        // Two branches whose slugs collide get two directories, because a slug is a location
        // and the branch is the key.
        let collided = ensure(&git, &at, "feat-projects").expect("the colliding branch starts");
        assert_ne!(
            collided.worktree().canonical.as_ref(),
            Some(&made),
            "two branches must not share one worktree directory"
        );
    }

    #[test]
    fn a_worktree_nysia_makes_does_not_turn_up_in_the_repository_it_is_in() {
        let Some(git) = crate::git::testing::git_or_skip() else {
            return;
        };
        let scratch = crate::git::testing::Scratch::new("worktree-conceal");
        let repo = scratch.repository("repo");
        let at = CanonicalPath::of(&repo).expect("the repository resolves");

        ensure(&git, &at, "feat/projects").expect("the branch starts");

        // **The assertion that matters is git's**, not the presence of a file. The path shape
        // puts worktrees inside the main worktree, so without concealment `git status` shows
        // `.nysia/` and `git add -A` stages the worktree as an embedded git repository — a
        // gitlink committed by accident. Asserting only that `.gitignore` exists would pass
        // against a `.gitignore` that said nothing.
        let status = git
            .run(&GitCommand::new(["status", "--porcelain"]), &at)
            .expect("git reports status");
        assert!(
            status.is_empty(),
            "starting a branch left the repository dirty: {:?}",
            String::from_utf8_lossy(&status)
        );

        // And the concealment is inside the folder Nysia created, not in the person's own
        // files: §3.1 says Nysia does not write into the folder beyond ordinary git
        // operations, so `.git/info/exclude` is left alone.
        let exclude = repo.join(".git").join("info").join("exclude");
        let excluded = std::fs::read_to_string(&exclude).unwrap_or_default();
        assert!(
            !excluded.contains("nysia"),
            "Nysia wrote into the repository's own exclude file: {excluded}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join(".nysia").join(".gitignore"))
                .expect("the concealment is in Nysia's own directory"),
            "*\n"
        );
    }

    #[test]
    fn a_branch_whose_worktree_directory_was_deleted_says_which_command_clears_it() {
        let Some(git) = crate::git::testing::git_or_skip() else {
            return;
        };
        let scratch = crate::git::testing::Scratch::new("worktree-prunable");
        let repo = scratch.repository("repo");
        let at = CanonicalPath::of(&repo).expect("the repository resolves");

        let created = ensure(&git, &at, "feat/gone").expect("the branch starts");
        let made = created.worktree().canonical.clone().expect("it is on disk");
        std::fs::remove_dir_all(made.as_path()).expect("the directory can be removed");

        // git still lists the registration, so there is nothing to adopt and `worktree add`
        // would refuse to check the branch out twice. Its own case because the answer is one
        // command, and a caller told only "could not start" has nowhere to go.
        let refused = ensure(&git, &at, "feat/gone");
        assert!(
            matches!(refused, Err(StartError::BranchPrunable { .. })),
            "got {refused:?}"
        );
    }

    #[test]
    fn a_branch_is_reported_short_and_with_its_commit() {
        let stream = porcelain(&[&[
            "worktree /work/repo",
            "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
            "branch refs/heads/main",
        ]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(worktrees.len(), 1);
        assert_eq!(
            worktrees[0].head,
            Head::Branch {
                name: "main".to_owned(),
                commit: "1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4".to_owned(),
            }
        );
        assert_eq!(worktrees[0].head.branch(), Some("main"));
        assert!(worktrees[0].is_main, "git lists the main worktree first");
    }

    #[test]
    fn a_repository_with_no_commits_is_unborn_and_not_detached() {
        // `git init` and nothing else. The ref exists and points nowhere, and a caller that
        // read the zero oid as a commit would show a project checked out at a commit that
        // does not exist.
        let stream = porcelain(&[&[
            "worktree /work/fresh",
            "HEAD 0000000000000000000000000000000000000000",
            "branch refs/heads/main",
        ]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(
            worktrees[0].head,
            Head::Unborn {
                name: "main".to_owned()
            }
        );
        assert_eq!(
            worktrees[0].head.branch(),
            Some("main"),
            "an unborn branch is still the key a worktree is identified by (D-6)"
        );
    }

    #[test]
    fn a_bare_repository_has_no_head_and_no_branch() {
        let stream = porcelain(&[&["worktree /work/bare.git", "bare"]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(worktrees[0].head, Head::Bare);
        assert_eq!(worktrees[0].head.branch(), None);
    }

    #[test]
    fn a_detached_head_reports_its_commit_and_no_branch() {
        let stream = porcelain(&[&[
            "worktree /work/repo",
            "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
            "detached",
        ]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(
            worktrees[0].head,
            Head::Detached {
                commit: "1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4".to_owned()
            }
        );
        assert_eq!(worktrees[0].head.branch(), None);
    }

    #[test]
    fn a_deleted_directory_keeps_its_registration_and_its_branch() {
        // The case the task names. git still lists it, with a branch and a reason it can be
        // pruned; nothing on disk answers to the path.
        let stream = porcelain(&[
            &[
                "worktree /work/repo",
                "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
                "branch refs/heads/main",
            ],
            &[
                "worktree /work/gone",
                "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
                "branch refs/heads/feature",
                "prunable gitdir file points to non-existent location",
            ],
        ]);
        let worktrees = parse(&stream).expect("two records");
        assert_eq!(worktrees.len(), 2);
        assert_eq!(
            worktrees[1].prunable.as_deref(),
            Some("gitdir file points to non-existent location")
        );
        assert_eq!(worktrees[1].head.branch(), Some("feature"));
        assert!(
            worktrees[1].canonical.is_none(),
            "/work/gone does not exist, so it has no canonical path"
        );
        assert_eq!(
            worktrees[1].path,
            record_path("/work/gone"),
            "the registered path survives, because it is what `git worktree prune` needs"
        );
    }

    #[test]
    fn a_lock_with_and_without_a_reason_are_told_apart() {
        let stream = porcelain(&[
            &[
                "worktree /work/a",
                "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
                "branch refs/heads/a",
                "locked because the drive is removable",
            ],
            &[
                "worktree /work/b",
                "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
                "branch refs/heads/b",
                "locked",
            ],
            &[
                "worktree /work/c",
                "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
                "branch refs/heads/c",
            ],
        ]);
        let worktrees = parse(&stream).expect("three records");
        assert_eq!(
            worktrees[0].locked.as_deref(),
            Some("because the drive is removable")
        );
        assert_eq!(
            worktrees[1].locked.as_deref(),
            Some(""),
            "locked with no reason is still locked"
        );
        assert_eq!(worktrees[2].locked, None);
    }

    #[test]
    fn a_path_containing_a_newline_stays_one_record() {
        // The whole reason for `-z`. The line-oriented `--porcelain` would split this into a
        // `worktree` record and an unparsable fragment, and the worktree would go missing.
        let stream = porcelain(&[&[
            "worktree /work/two\nlines",
            "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
            "branch refs/heads/main",
        ]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(worktrees.len(), 1, "a newline must not split a record");
        assert_eq!(worktrees[0].path, record_path("/work/two\nlines"));
        assert_eq!(worktrees[0].head.branch(), Some("main"));
    }

    #[test]
    fn a_ref_outside_refs_heads_is_not_shortened_into_a_different_branch() {
        let stream = porcelain(&[&[
            "worktree /work/repo",
            "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
            "branch refs/remotes/origin/main",
        ]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(
            worktrees[0].head.branch(),
            Some("refs/remotes/origin/main"),
            "trimming to the last component would name a different branch"
        );
    }

    #[test]
    fn an_unknown_attribute_is_ignored_rather_than_refused() {
        // git adds annotations over time, and one this build has not heard of is not a
        // reason to stop being able to say what a folder is.
        let stream = porcelain(&[&[
            "worktree /work/repo",
            "HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4",
            "branch refs/heads/main",
            "something-git-2-99-added yes",
        ]]);
        let worktrees = parse(&stream).expect("one record");
        assert_eq!(worktrees[0].head.branch(), Some("main"));
    }

    #[test]
    fn a_record_with_neither_a_head_nor_bare_is_refused() {
        // Not silently dropped. A worktree missing from this list is a worktree that gets
        // registered a second time, so the list is either right or an error.
        let stream = porcelain(&[&["worktree /work/repo"]]);
        let err = parse(&stream).unwrap_err();
        assert!(err.contains("neither a HEAD nor"), "{err}");
    }

    #[test]
    fn an_attribute_before_any_worktree_is_refused() {
        let stream = porcelain(&[&["HEAD 1a2b3c4d5e6f71829304a5b6c7d8e9f0a1b2c3d4"]]);
        let err = parse(&stream).unwrap_err();
        assert!(err.contains("before any worktree"), "{err}");
    }

    #[test]
    fn an_empty_stream_is_an_empty_list() {
        assert_eq!(parse(&[]).expect("nothing to parse"), Vec::new());
    }
}
