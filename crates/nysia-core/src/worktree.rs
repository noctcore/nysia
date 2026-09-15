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
//! Discovery, which is what registering a project needs (v0.3 §3.1): every worktree of a
//! repository, the branch each is on, and which one the person is looking at. Creation,
//! removal and the confinement gates arrive with the worktree manager.
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
use std::path::PathBuf;

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
