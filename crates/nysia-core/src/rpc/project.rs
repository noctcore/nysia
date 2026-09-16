//! The three project verbs, served.
//!
//! `docs/plans/v0.3-delivery-plan.md` §3 is the contract and `nysia-proto` is its Rust
//! spelling; this is the daemon's half. The verbs were on the wire from wave A answering
//! `unsupported`, the table landed in wave B, and this module is what joins them.
//!
//! # A project is two halves, and only one of them is stored
//!
//! §3.3 makes the store **registration-only** on purpose: git owns worktrees and the session
//! registry owns sessions, so a `worktrees` column would be a copy of two things that change
//! without anybody telling the store. [`crate::store::StoredProject`] therefore has the path
//! and no worktrees, [`nysia_proto::Project`] has the worktrees and no path, and this module
//! composes the second from the first **live**, on every list.
//!
//! That is the honest shape and it is not free: composing costs one `git worktree list` per
//! project, and the sidebar shows ten. See [`ProjectService::list`] for what is done about
//! it, which is the part a user waits on.
//!
//! # What is deliberately not on the wire
//!
//! **The path**, and this module is where that has to be kept true. A repository path names
//! a person's disk (traps register #13/#14), so nothing here puts one in an envelope or a
//! log line: the refusals come from [`RegisterRefusal`], which has no field to carry one,
//! and the success lines name the id. [`PathError`] and [`GitError`] both carry paths in
//! their `Display`, so neither is ever rendered into an answer — they are matched on and
//! replaced.
//!
//! # Two shapes the wire cannot spell, and what happens to them
//!
//! [`nysia_proto::Worktree::branch`] is a plain `String` and is not optional, because D-6
//! keys a worktree by its branch. Two real worktrees have no branch to be keyed by:
//!
//! - a **detached HEAD**, and a **bare** repository's git directory
//! - a worktree whose directory has been deleted, which git still lists until it is pruned
//!
//! Both are **left out of the answer** rather than given an invented key. The alternative is
//! putting a commit id in a field named `branch`, and `Start →` takes a branch — so a client
//! round-tripping one back would ask for a worktree keyed by something that is not a branch,
//! which is the representable-but-wrong state D-6 exists to prevent. Each omission is logged
//! with its reason, so a repository that lists nothing is diagnosable rather than mysterious.

use std::sync::Arc;
use std::time::Duration;

use nysia_proto::{
    ErrorCode, ErrorEnvelope, Project, ProjectForget, ProjectId, ProjectRegister,
    ProjectRegistered, RegisterRefusal, ResponsePayload, Worktree,
};

use crate::git::{CanonicalPath, Folder, Git, GitError, PathError, Repository};
use crate::rpc::errors::envelope;
use crate::rpc::session::SessionRegistry;
use crate::store::{Forgotten, Registration, Store, StoreError, StoredProject};

/// The deadline one project's git invocation gets while a list is being composed.
///
/// Much shorter than [`crate::git::DEFAULT_TIMEOUT`], and the reason is that nobody asked
/// for this particular invocation. A registration is a thing a person did and is watching;
/// a list is the sidebar drawing itself, and the window serves its control connection with a
/// single worker draining a queue **in order**
/// (`apps/desktop/src-tauri/src/daemon/control.rs`) — so every millisecond a `project_list`
/// spends in the daemon is a millisecond `terminal_send` waits behind it. While the daemon
/// answered `unsupported` instantly that cost nothing; serving the verb makes it real.
///
/// Two seconds is far longer than `git worktree list` takes on a local repository and short
/// enough that a folder on a disconnected share degrades rather than stalls the window.
const LIST_DEADLINE: Duration = Duration::from_secs(2);

/// The projects this daemon has registered, and the live half of each.
#[derive(Debug)]
pub struct ProjectService {
    store: Arc<Store>,
    sessions: Arc<SessionRegistry>,
    /// The one resolved `git`, or the reason there is none.
    ///
    /// Resolved once at bind rather than per verb, which is what [`Git::locate`] is for: on
    /// Windows the resolution is the difference between an error naming what was looked for
    /// and a bare `NotFound` out of `CreateProcess` (traps register #8).
    ///
    /// **A daemon with no git still starts.** Git is needed by three verbs out of seventeen,
    /// and refusing to bind without it would stop somebody opening a shell on a machine
    /// where git has never been installed — a much larger failure than the one being
    /// reported. The envelope is built once, here, because [`GitError`] is not `Clone` and
    /// every project verb owes the same answer.
    git: Result<Git, ErrorEnvelope>,
}

impl ProjectService {
    /// Serve projects out of `store`, attributing sessions from `sessions`.
    #[must_use]
    pub fn new(store: Arc<Store>, sessions: Arc<SessionRegistry>) -> Self {
        let git = Git::locate().map_err(|err| {
            tracing::warn!(
                %err,
                "git could not be resolved; the project verbs will say so when they are asked"
            );
            no_git_envelope()
        });
        Self {
            store,
            sessions,
            git,
        }
    }

    /// The resolved git, or the answer for a machine that has none.
    fn git(&self) -> Result<&Git, ErrorEnvelope> {
        self.git.as_ref().map_err(Clone::clone)
    }

    /// Register a folder as a project.
    ///
    /// Blocking: it canonicalises, runs git and writes a row. Every caller runs it on the
    /// blocking pool — [`fs::canonicalize`](std::fs::canonicalize) alone blocks for the OS's
    /// own timeout on a disconnected share, **outside any git deadline**, and a runtime
    /// worker parked there stalls every session sharing it.
    #[must_use]
    pub fn register(&self, request: &ProjectRegister) -> ResponsePayload {
        match self.registration(request) {
            Ok(registered) => ResponsePayload::ProjectRegister(registered),
            Err(envelope) => ResponsePayload::Error(envelope),
        }
    }

    /// The body of [`ProjectService::register`], in the shape `?` can be used in.
    fn registration(&self, request: &ProjectRegister) -> Result<ProjectRegistered, ErrorEnvelope> {
        let git = self.git()?;
        // Resolving the caller's path is the first thing that can fail and the first thing
        // that must not leak: §3.2's fourth case is "a path that does not exist, or is not
        // readable", and `RegisterRefusal::Unreadable` is how it is said without repeating
        // the path back into an envelope that may be logged.
        let requested = CanonicalPath::of(&request.path)
            .map_err(|err| refuse(&RegisterRefusal::Unreadable, &GitError::Path(err)))?;

        let repository = match crate::git::inspect_folder(git, &requested) {
            Ok(Folder::Repository(repository)) => repository,
            Ok(Folder::ManyRepositories {
                repositories,
                truncated,
            }) => {
                // §3.2's "ask": one repository at a time, and none of them registered. The
                // names are relative to the folder offered, and `into_envelope` reduces each
                // to its final component, so nothing here widens what an envelope carries.
                let found = repositories
                    .iter()
                    .filter_map(|repository| repository.folder_name())
                    .map(str::to_owned)
                    .collect();
                tracing::info!(
                    truncated,
                    "a folder of repositories was offered; registering none of them"
                );
                return Err(RegisterRefusal::ManyRepositories { found }.into_envelope());
            }
            Ok(Folder::NoRepository) => {
                return Err(RegisterRefusal::NotARepository.into_envelope());
            }
            Err(err) => return Err(git_refusal(&err)),
        };

        // **The repository, not the folder that was pointed at.** `inspect` deliberately does
        // not fold "inside a repository" into "is a repository", so that a caller can decide;
        // this caller decides on the checkout's root. The id is derived from the canonical
        // path, so registering `…/nysia` and `…/nysia/crates/nysia-core` would otherwise be
        // two projects for one repository — the same folder in the sidebar twice, each with
        // the identical worktree list, and no way for a person to tell which is which.
        // Idempotency by path is the whole of §3.2 and it has to survive being told the same
        // repository in two spellings.
        let root = registration_root(&repository, &requested);
        let name = root
            .folder_name()
            // A filesystem root has no name of its own and is still a folder somebody may
            // have run `git init` in. Its own path is the only thing left to call it, and it
            // reaches nothing but this daemon's own sidebar.
            .map_or_else(|| root.to_string(), str::to_owned);

        let registered = self
            .store
            .register_project(&Registration {
                path: root.clone(),
                name,
                group: Project::DEFAULT_GROUP.to_owned(),
            })
            .map_err(|err| store_refusal(&err))?;

        // The id, never the path: this is the line a log file keeps.
        tracing::info!(
            project = %registered.project().id,
            already_registered = registered.already_registered(),
            "registered a project"
        );

        Ok(ProjectRegistered {
            already_registered: registered.already_registered(),
            project: self.compose(&registered.into_project(), &repository),
        })
    }

    /// Every registered project, with the live half of each composed beside it.
    ///
    /// # Why this fans out
    ///
    /// Composing costs one `git worktree list` per project. Run in sequence, ten projects is
    /// ten spawns end to end on a path the sidebar waits on, and the window's control
    /// connection is drained in order — so it is also ten spawns that `terminal_send` waits
    /// behind. Run concurrently, ten projects cost roughly one spawn of wall time, and the
    /// worst case is [`LIST_DEADLINE`] rather than ten of them.
    ///
    /// # A project that cannot be reached is still a project
    ///
    /// A folder on an unplugged drive cannot be canonicalised and cannot be inspected, and
    /// neither failure is a reason to drop it: the store keeps
    /// [`StoredProject::path`](crate::store::StoredProject::path) as a plain `PathBuf`
    /// precisely so that reading a row does not depend on the folder still being there. So a
    /// project that times out or refuses is reported **with an empty worktree list** — the
    /// sidebar shows a project that cannot be opened, which is true, rather than losing one
    /// the person registered.
    ///
    /// The timed-out work is not cancelled, because a blocking `stat` on a dead share cannot
    /// be: the deadline bounds how long the *caller* waits, and the thread is returned to the
    /// pool whenever the OS gives up on it.
    pub async fn list(self: &Arc<Self>) -> ResponsePayload {
        let service = Arc::clone(self);
        let stored = match tokio::task::spawn_blocking(move || service.store.projects()).await {
            Ok(Ok(stored)) => stored,
            Ok(Err(err)) => return ResponsePayload::Error(list_refusal(&err)),
            Err(err) => return ResponsePayload::Error(joining(&err.to_string())),
        };

        let mut running = Vec::with_capacity(stored.len());
        for project in stored {
            let unreachable = without_worktrees(&project);
            let service = Arc::clone(self);
            running.push((
                unreachable,
                tokio::task::spawn_blocking(move || service.reached(&project)),
            ));
        }

        let mut projects = Vec::with_capacity(running.len());
        for (unreachable, task) in running {
            // Each project gets its own deadline rather than sharing one across the list.
            // A shared deadline would have the last project in a long list punished for the
            // time the first one took, which is the sidebar reordering its own failures.
            projects.push(match tokio::time::timeout(LIST_DEADLINE, task).await {
                Ok(Ok(Some(project))) => project,
                Ok(Ok(None)) => unreachable,
                Ok(Err(err)) => {
                    tracing::warn!(project = %unreachable.id, %err, "composing a project panicked");
                    unreachable
                }
                Err(_) => {
                    tracing::warn!(
                        project = %unreachable.id,
                        "git did not answer for a project within {LIST_DEADLINE:?}; \
                         listing it with no worktrees"
                    );
                    unreachable
                }
            });
        }
        ResponsePayload::ProjectList { projects }
    }

    /// Forget a project's registration, and nothing else.
    ///
    /// Blocking, for the reason [`ProjectService::register`] is.
    #[must_use]
    pub fn forget(&self, request: &ProjectForget) -> ResponsePayload {
        match self.store.forget_project(&request.id) {
            Ok(Forgotten::Removed) => {
                tracing::info!(project = %request.id, "forgot a project's registration");
                ResponsePayload::ProjectForget
            }
            // Not a quiet success. An id nothing is registered under means the caller believes
            // something is there — a typo, or a project another client forgot first — and
            // exiting zero leaves them believing they forgot something they did not.
            Ok(Forgotten::Unknown) => ResponsePayload::Error(unknown_project(&request.id)),
            Err(err) => ResponsePayload::Error(store_refusal(&err)),
        }
    }

    /// One stored project with what git says about it, or `None` when git will not say.
    ///
    /// Blocking. `None` rather than an error because the caller's answer to every way this
    /// can fail is the same — report the project with no worktrees — and a `Result` here
    /// would be four error types converted into one discarded value.
    fn reached(&self, stored: &StoredProject) -> Option<Project> {
        let git = self.git.as_ref().ok()?.clone().with_timeout(LIST_DEADLINE);
        // Canonicalising is inside the blocking task and inside the deadline. It is *not*
        // inside git's deadline — `fs::canonicalize` is a plain blocking syscall that git
        // never sees — which is exactly why the timeout in `list` wraps the whole of this
        // rather than relying on `with_timeout` above.
        let at = CanonicalPath::of(&stored.path)
            .inspect_err(|err| {
                tracing::debug!(project = %stored.id, kind = path_kind(err), "a registered folder could not be resolved");
            })
            .ok()?;
        match crate::git::inspect_folder(&git, &at) {
            Ok(Folder::Repository(repository)) => Some(self.compose(stored, &repository)),
            // Registered as a repository and no longer one: the folder was replaced, or the
            // `.git` was removed. The registration stands — forgetting it is the person's
            // call, not a side effect of listing — and it lists with no worktrees.
            Ok(_) => None,
            Err(err) => {
                tracing::debug!(project = %stored.id, kind = git_kind(&err), "git would not describe a registered project");
                None
            }
        }
    }

    /// Fill in a project's live half from what git found.
    ///
    /// The one place a [`Repository`] becomes [`nysia_proto::Worktree`]s, so the rules the
    /// module docs state about branchless and pruned worktrees are applied once.
    fn compose(&self, stored: &StoredProject, repository: &Repository) -> Project {
        let project = without_worktrees(stored);
        let mut worktrees = Vec::with_capacity(repository.worktrees.len());
        for worktree in &repository.worktrees {
            let Some(branch) = worktree.head.branch() else {
                tracing::info!(
                    project = %project.id,
                    "a worktree with no branch is left out of the answer; the wire keys a \
                     worktree by its branch (D-6) and has no way to spell one without"
                );
                continue;
            };
            let Some(canonical) = worktree.canonical.as_ref() else {
                tracing::info!(
                    project = %project.id,
                    branch,
                    "a worktree git still lists has no directory on disk; `git worktree \
                     prune` is what removes the registration"
                );
                continue;
            };
            worktrees.push(Worktree {
                branch: branch.to_owned(),
                is_primary: worktree.is_primary,
                sessions: self.sessions.summaries_under(canonical),
            });
        }
        Project {
            worktrees,
            ..project
        }
    }
}

/// A registered project as it reads when git has not described it.
///
/// The three stored fields and an empty worktree list. It is the answer for a folder on an
/// unplugged drive, for one whose `.git` has gone, and for one git did not describe in time
/// — and it is what makes those a project the sidebar still shows rather than a project the
/// sidebar loses.
fn without_worktrees(stored: &StoredProject) -> Project {
    Project {
        id: stored.id.clone(),
        name: stored.name.clone(),
        group: stored.group.clone(),
        worktrees: Vec::new(),
    }
}

/// Which folder a registration is recorded against.
///
/// The primary worktree's root — the checkout containing what was offered — falling back to
/// the folder itself when no worktree contains it. That fallback is a real repository shape
/// rather than a defensive `else`: a **bare** repository reached from outside its own
/// directory has no worktree containing the folder, and it is still something a person may
/// register.
fn registration_root(repository: &Repository, requested: &CanonicalPath) -> CanonicalPath {
    repository
        .primary()
        .and_then(|worktree| worktree.canonical.clone())
        .unwrap_or_else(|| requested.clone())
}

/// The refusal a git failure becomes.
///
/// **Matched on, never rendered.** Every [`GitError`] variant's `Display` names a path — the
/// working directory it ran in, at least — and an error envelope is the thing a daemon is
/// most likely to log verbatim (traps register #13/#14).
fn git_refusal(err: &GitError) -> ErrorEnvelope {
    match err {
        GitError::Path(_) => refuse(&RegisterRefusal::Unreadable, err),
        GitError::NotInstalled { .. } => no_git_envelope(),
        // Everything else is git running and not answering: a timeout, a non-zero exit, an
        // output this build could not parse. None of them says the folder is not a
        // repository, so none of them may answer as though it did.
        _ => {
            tracing::warn!(kind = git_kind(err), %err, "git would not describe a folder offered for registration");
            envelope(
                ErrorCode::Internal,
                format!("git could not describe that folder: {}", git_kind(err)),
                "check that the folder opens in a terminal and that `git status` answers in it",
                &["a folder on a network share that has gone away is the usual cause"],
            )
            .retryable(true)
        }
    }
}

/// A refusal, with the failure behind it logged rather than sent.
fn refuse(refusal: &RegisterRefusal, err: &GitError) -> ErrorEnvelope {
    tracing::debug!(kind = git_kind(err), "refusing a registration");
    refusal.clone().into_envelope()
}

/// What a store failure means to a caller registering or forgetting a project.
///
/// Not the blanket [`crate::store::StoreError`] envelope, whose next steps are about the
/// agent-status spool: there is no spool behind a registration, and telling somebody their
/// project will be drained at the next daemon start would be a promise nothing keeps.
fn store_refusal(err: &StoreError) -> ErrorEnvelope {
    tracing::warn!(%err, "a project could not be read or written");
    envelope(
        ErrorCode::Internal,
        format!("the daemon could not record that project: {err}"),
        "retry the verb; nothing was written if it failed",
        &["check that the daemon's runtime directory is writable and not full"],
    )
    .retryable(true)
}

/// What a store failure means to `project_list`, which is the one that empties a sidebar.
///
/// # Fail-closed, and why that is the choice
///
/// One row whose id this build cannot read fails the whole list. Skipping it instead would
/// be worse in a way that is quiet: the sidebar would lose a project the store still holds,
/// and the next attempt to register that same folder would answer `alreadyRegistered: true`
/// for a project the person cannot see and cannot forget. A list that refuses says something
/// is wrong once; a list that silently shortens itself says nothing and contradicts the verb
/// beside it.
///
/// The recovery is therefore stated rather than left to be worked out. It is not `nysia
/// project forget`, because the row cannot be addressed by an id that will not parse — it is
/// the database, which [`StoreError`] already names, and losing it costs registrations and
/// nothing on disk. §3.1 is the guarantee behind that: Nysia does not own the folder.
fn list_refusal(err: &StoreError) -> ErrorEnvelope {
    tracing::warn!(%err, "the project list could not be read");
    envelope(
        ErrorCode::Internal,
        format!("the daemon could not read its project list: {err}"),
        "stop the daemon and move that database file aside, then register the folders again",
        &[
            "forgetting one project cannot fix this: the row that will not read is the one \
             with no id to name it by",
            "nothing on disk is lost — a registration is a folder Nysia remembers, never a \
             folder it owns",
        ],
    )
    .retryable(false)
}

/// The answer every project verb owes on a machine with no git.
///
/// Honest about which of the two it is. "Could not register" on a machine that has never had
/// git installed sends a person looking at their folder, which is the one place the problem
/// is not.
fn no_git_envelope() -> ErrorEnvelope {
    envelope(
        ErrorCode::Internal,
        "this machine has no `git` that Nysia can run",
        "install git and restart the daemon; it resolves git once, at startup",
        &[
            "a git installed after the daemon started is not picked up until it restarts",
            "`git --version` in a terminal is the same question this daemon asked",
        ],
    )
    .retryable(false)
}

/// An id nothing is registered under.
fn unknown_project(id: &ProjectId) -> ErrorEnvelope {
    envelope(
        ErrorCode::UnknownProject,
        format!("no project is registered under {id}"),
        "run `nysia project list` to see the projects this daemon holds",
        &["forgetting a project that was already forgotten is not something to retry"],
    )
    .with_next_command_args(["nysia", "project", "list"])
}

/// A blocking task that did not finish.
fn joining(detail: &str) -> ErrorEnvelope {
    envelope(
        ErrorCode::Internal,
        format!("the daemon failed while listing projects: {detail}"),
        "retry the verb",
        &["`nysia session list` will show whether the daemon is still serving"],
    )
    .retryable(true)
}

/// A git failure's variant, as a word for a log line.
///
/// The variant and never the message: every `Display` in [`GitError`] carries at least the
/// working directory git ran in.
fn git_kind(err: &GitError) -> &'static str {
    match err {
        GitError::Path(err) => path_kind(err),
        GitError::NotInstalled { .. } => "git is not installed",
        GitError::Spawn { .. } => "git could not be started",
        GitError::TimedOut { .. } => "git ran past its deadline",
        GitError::Failed { .. } => "git reported a failure",
        GitError::Usage { .. } => "git refused the arguments",
        GitError::Unparsable { .. } => "git's answer could not be read",
        GitError::Refused { .. } => "git would not open the repository",
    }
}

/// A path failure's variant, as a word for a log line.
fn path_kind(err: &PathError) -> &'static str {
    match err {
        PathError::Missing { .. } => "the folder is not there",
        PathError::Unreadable { .. } => "the folder could not be read",
        PathError::NotADirectory { .. } => "the path is a file",
    }
}
