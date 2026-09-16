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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use nysia_proto::{
    ErrorCode, ErrorEnvelope, Project, ProjectForget, ProjectId, ProjectRegister,
    ProjectRegistered, ProjectStart, ProjectStarted, RegisterRefusal, ResponsePayload,
    SessionCreate, Worktree,
};

use crate::git::{CanonicalPath, Folder, Git, GitError, PathError, Repository};
use crate::rpc::errors::{IntoEnvelope, envelope};
use crate::rpc::session::SessionRegistry;
use crate::store::{Forgotten, Registration, Store, StoreError, StoredProject};
use crate::worktree::StartError;

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

/// The size a `Start →` session opens at.
///
/// The window resizes it as soon as the tab is laid out, so this only has to be a sane
/// terminal rather than the right one — and it is the same pair `nysia session create`
/// defaults to, because two different defaults for the same question is one of them being
/// wrong somewhere.
const START_COLS: u16 = 120;
/// See [`START_COLS`].
const START_ROWS: u16 = 30;

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
    /// behind. Run concurrently under **one absolute deadline**, ten projects cost roughly one
    /// spawn of wall time and the whole verb is bounded by [`LIST_DEADLINE`] — not by ten of
    /// them, which is what a per-project timeout awaited in a loop would have cost.
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

        // **One deadline for the whole list, fixed before the first await.** The tasks are
        // already running concurrently, so they all started at the same instant and an
        // absolute deadline treats every one of them alike. A fresh `timeout(LIST_DEADLINE,
        // …)` per project would not: its clock starts when it is *awaited*, and these are
        // awaited in a loop, so ten unreachable projects would cost ten deadlines end to end
        // — the very stall the fan-out exists to prevent, hidden behind code that looks
        // bounded.
        let deadline = tokio::time::Instant::now() + LIST_DEADLINE;
        let mut projects = Vec::with_capacity(running.len());
        for (unreachable, task) in running {
            projects.push(match tokio::time::timeout_at(deadline, task).await {
                Ok(Ok(Some(project))) => project,
                Ok(Ok(None)) => unreachable,
                Ok(Err(err)) => {
                    tracing::warn!(project = %unreachable.id, %err, "composing a project panicked");
                    unreachable
                }
                Err(_) => {
                    tracing::warn!(
                        project = %unreachable.id,
                        "the project list passed its {LIST_DEADLINE:?} deadline; listing this \
                         project with no worktrees"
                    );
                    unreachable
                }
            });
        }
        ResponsePayload::ProjectList { projects }
    }

    /// `Start →`: a branch-keyed worktree, a session in it, and what opens a tab.
    ///
    /// Blocking, and more so than the others: it canonicalises, runs git up to four times and
    /// spawns a pty. Every caller runs it on the blocking pool.
    ///
    /// # The order, and what is left behind when a step fails
    ///
    /// The worktree first, then the session in it. A worktree that was created and whose
    /// session then failed to spawn is **left where it is**: removing worktrees is v0.4's
    /// verb and the destructive one, and a retry adopts this worktree rather than making a
    /// second — so the cost of leaving it is one directory, and the cost of removing it is a
    /// delete on a path a person may already have opened.
    #[must_use]
    pub fn start(&self, request: &ProjectStart) -> ResponsePayload {
        match self.starting(request) {
            Ok(started) => ResponsePayload::ProjectStart(started),
            Err(envelope) => ResponsePayload::Error(envelope),
        }
    }

    /// The body of [`ProjectService::start`], in the shape `?` can be used in.
    fn starting(&self, request: &ProjectStart) -> Result<ProjectStarted, ErrorEnvelope> {
        let git = self.git()?;
        let stored = self
            .store
            .project(&request.project)
            .map_err(|err| store_refusal(&err))?
            .ok_or_else(|| unknown_project(&request.project))?;

        // The registered folder, re-resolved. A project whose drive has been unplugged lists
        // perfectly well — that is deliberate, see `list` — so this is the first step that
        // finds out, and it must say which of the two it is rather than "could not start".
        let at = CanonicalPath::of(&stored.path).map_err(|err| {
            tracing::warn!(project = %stored.id, kind = path_kind(&err), "a registered folder could not be resolved");
            envelope(
                ErrorCode::PathUnreadable,
                "that project's folder could not be opened",
                "check the folder is still there and that you can open it",
                &[
                    "a project on a drive that is not plugged in lists but cannot be started",
                    "`nysia project forget <id>` removes the registration and nothing on disk",
                ],
            )
        })?;

        // **Before `ensure`, which is the first step that writes.** `create` used to refuse
        // `kind: agent` as its own first statement — after a branch and a worktree had been
        // made for a session that was never going to start, and with nothing in the answer
        // saying so. Serving agent sessions removes that particular refusal; it does not
        // remove the shape, because an agent whose CLI is not installed reaches the same
        // place. Asking first is the fix that outlives the refusal.
        let opening = SessionCreate {
            kind: request.kind,
            // **Minted by the daemon**, because `Start →` creates the session before any
            // window has a leaf to name — the tab is opened *from* this answer. That is the
            // rule `SessionCreate::pane_key` already states for a null key, reached by a
            // different route.
            pane_key: None,
            profile: request.profile.clone(),
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: START_COLS,
            rows: START_ROWS,
        };
        self.sessions
            .precheck(&opening)
            .map_err(IntoEnvelope::into_envelope)?;

        let started = crate::worktree::ensure(git, &at, &request.branch).map_err(start_refusal)?;
        // `canonical` is `Some` for every worktree `ensure` answers with: it adopts only one
        // that has a directory and refuses the registered-but-deleted case by name.
        let worktree = started.worktree();
        let cwd = worktree.canonical.as_ref().ok_or_else(|| {
            internal_start("the worktree that was started has no directory on disk")
        })?;
        let branch = worktree.head.branch().unwrap_or(&request.branch).to_owned();

        let created = self
            .sessions
            .create(&SessionCreate {
                cwd: Some(cwd.as_path().to_path_buf()),
                ..opening
            })
            .map_err(IntoEnvelope::into_envelope)?;

        tracing::info!(
            project = %stored.id,
            branch,
            adopted = started.adopted(),
            handle = %created.handle,
            pane_key = %created.pane_key,
            "started a branch"
        );

        Ok(ProjectStarted {
            branch,
            adopted: started.adopted(),
            handle: created.handle,
            pane_key: created.pane_key,
        })
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
        // **Every worktree with a directory**, including the ones the loop below leaves out.
        // Nysia's own worktrees live inside the main one, so deciding which worktree a session
        // is in is a question about the whole set rather than about each in turn — see
        // `SessionRegistry::summaries_under`. A branchless worktree cannot be put on the wire
        // and is still a directory a session can be inside, so it has to be in this set or
        // every session in one floats up to the main worktree.
        let folders: Vec<CanonicalPath> = repository
            .worktrees
            .iter()
            .filter_map(|worktree| worktree.canonical.clone())
            .collect();
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
                sessions: self.sessions.summaries_under(canonical, &folders),
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
            // `kind` and never `%err`: every `GitError` variant's `Display` names the working
            // directory git ran in, which here is the folder the caller offered.
            tracing::warn!(
                kind = git_kind(err),
                "git would not describe a folder offered for registration"
            );
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
    // `%err` here and `store_kind` in the envelope, which is the same split `git_refusal`
    // makes one screen up and for a related reason. The log is the daemon's own file and the
    // path is the daemon's own database, so naming it there is what makes the line worth
    // keeping; the envelope is read by a person, rendered in a window and pasted into issues,
    // and `C:\Users\<name>\AppData\Local\…\nysia.db` puts somebody's account name in all
    // three for no benefit — the recovery below names the file without spelling it.
    tracing::warn!(%err, "a project could not be read or written");
    envelope(
        ErrorCode::Internal,
        format!(
            "the daemon could not record that project: {}",
            store_kind(err)
        ),
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
        format!(
            "the daemon could not read its project list: {}",
            store_kind(err)
        ),
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

/// What a worktree that could not be started becomes.
///
/// # Why none of these mints a new [`ErrorCode`]
///
/// A code is a wire commitment and clients branch on it; three of these are "this request
/// cannot be served as asked" and the thing a caller can act on is the *step*, not the code.
/// §6.2 makes next steps non-optional precisely so that the interesting part of an error does
/// not have to be encoded in an enum. A branch with a stale worktree is one command away from
/// working, and that command is in the answer.
///
/// [`StartError`] carries no path and neither does anything built here — a worktree's
/// directory is under the person's project (traps register #13/#14).
fn start_refusal(err: StartError) -> ErrorEnvelope {
    match err {
        StartError::Git(err) => match err {
            GitError::NotInstalled { .. } => no_git_envelope(),
            other => {
                tracing::warn!(kind = git_kind(&other), "git would not start a branch");
                envelope(
                    ErrorCode::Internal,
                    format!(
                        "git could not open a worktree for that branch: {}",
                        git_kind(&other)
                    ),
                    "check that `git worktree list` answers in that project",
                    &[
                        "a worktree cannot be created while another git command holds the \
                         index lock",
                        // Said rather than left to be discovered. Everything a caller can be
                        // refused for is answered before anything is created, so this is the
                        // one step that can fail with something already on disk — and what
                        // is there is Nysia's own directory, ignored by git including itself
                        // and reused by the next start.
                        "Nysia's `.nysia` directory may have been created in the project; it \
                         holds nothing but a `.gitignore` that hides it, and starting the \
                         branch again reuses it",
                    ],
                )
                .retryable(true)
            }
        },
        StartError::BranchRefused { branch, reason } => envelope(
            ErrorCode::InvalidRequest,
            format!("{branch:?} cannot be used as a branch: {reason}"),
            "choose a branch name git accepts — `git check-ref-format --branch <name>` is the \
                 same question this asked",
            &[
                "a name beginning with `-` is refused whatever git thinks of it, because it \
                 would reach git's option parser",
            ],
        ),
        StartError::BranchPrunable { branch } => envelope(
            ErrorCode::InvalidRequest,
            format!("{branch:?} already has a worktree whose directory is gone"),
            "run `git worktree prune` in that project, then start the branch again",
            &[
                "git will not check one branch out twice, and the registration it is refusing \
                 over no longer has a directory behind it",
            ],
        ),
        StartError::NoDirectory { branch } => envelope(
            ErrorCode::InvalidRequest,
            format!("there is no free directory left for {branch:?}"),
            "remove or rename the unused worktree directories under `.nysia/worktrees`",
            &["worktree directory names are a location, never a key: the branch is the key"],
        ),
    }
}

/// A `Start →` that reached a state this module believes impossible.
fn internal_start(detail: &str) -> ErrorEnvelope {
    envelope(
        ErrorCode::Internal,
        format!("the daemon could not start that branch: {detail}"),
        "retry the verb; a worktree that was created is adopted rather than duplicated",
        &["`git worktree list` in the project shows what is there"],
    )
    .retryable(true)
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

/// A store failure's variant, as a phrase an envelope can carry.
///
/// **The variant and never the message.** Every path-carrying [`StoreError`] names the
/// database file in its `Display`, and that file lives under the person's profile:
///
/// ```text
/// the daemon could not read its project list: could not read the id of a project in the
/// store at C:\Users\kacpe\AppData\Local\…\nysia.db: a project id is `proj_<32 …>`, got …
/// ```
///
/// It is the *daemon's* own runtime path rather than a caller's, which is why the leak guard
/// beside `register` did not cover it — that one is about a path the caller supplied. It is
/// still somebody's account name, in an envelope that reaches a window and gets pasted into
/// issues. The recovery names the file without spelling it, so nothing is lost by saying
/// which kind of failure it was instead, and the whole `Display` still goes to the log.
///
/// Exhaustive on purpose, like [`git_kind`]: a variant added later has to be given a phrase
/// here rather than silently falling into a catch-all that says nothing.
fn store_kind(err: &StoreError) -> &'static str {
    match err {
        StoreError::Io { .. } => "its database file could not be read or written",
        StoreError::Sqlite { .. } => "its database refused the operation",
        StoreError::NotWal { .. } => "its database is not in WAL mode",
        StoreError::Migration { .. } | StoreError::MigrationOrder { .. } => {
            "its database could not be migrated to this build's schema"
        }
        StoreError::SchemaAhead { .. } => "its database was written by a newer build of Nysia",
        StoreError::State { .. }
        | StoreError::Pane { .. }
        | StoreError::Project { .. }
        | StoreError::Question { .. }
        | StoreError::Timestamp { .. }
        | StoreError::NegativeTimestamp { .. } => "its database holds a row this build cannot read",
        StoreError::Symlink { .. } => "its database path is a symbolic link, which is refused",
        StoreError::Poisoned => "a panic in the daemon left its database connection unusable",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{Scratch, git_or_skip};
    use crate::rpc::{Client, Daemon, DaemonConfig, Endpoint};
    use nysia_proto::{ClientId, ClientRole, SessionKind};

    /// A service over a database of this test's own.
    ///
    /// The directory is returned so the caller can remove it last: the `-wal` and `-shm` live
    /// beside the database, and a test that removes only the file it named leaves two.
    fn service(tag: &str) -> (std::path::PathBuf, Arc<ProjectService>) {
        let dir = std::env::temp_dir().join(format!("nysia-projects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory for the database");
        let store = Arc::new(Store::open(dir.join("nysia.db")).expect("the store opens"));
        let service = Arc::new(ProjectService::new(store, Arc::new(SessionRegistry::new())));
        (dir, service)
    }

    /// Register `path`, expecting it to be accepted.
    fn register(
        service: &ProjectService,
        path: impl Into<std::path::PathBuf>,
    ) -> ProjectRegistered {
        let path = path.into();
        match service.register(&ProjectRegister { path: path.clone() }) {
            ResponsePayload::ProjectRegister(registered) => registered,
            other => panic!("{} should have registered, got {other:?}", path.display()),
        }
    }

    /// The envelope a refused verb answered with.
    fn refused(payload: ResponsePayload) -> ErrorEnvelope {
        match payload {
            ResponsePayload::Error(envelope) => envelope,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Every project the service lists.
    async fn listed(service: &Arc<ProjectService>) -> Vec<Project> {
        match service.list().await {
            ResponsePayload::ProjectList { projects } => projects,
            other => panic!("expected a list, got {other:?}"),
        }
    }

    /// Other spellings of `repo` that name the same folder on this platform.
    fn spellings(repo: &std::path::Path) -> Vec<std::path::PathBuf> {
        #[allow(unused_mut)]
        let mut spellings = vec![
            // Portable, and the one that does not depend on the filesystem's own rules:
            // nothing but `fs::canonicalize` folds a `..` away.
            repo.join("..").join(
                repo.file_name()
                    .expect("the fixture repository has a folder name"),
            ),
        ];
        #[cfg(windows)]
        {
            // Two spellings Windows calls one folder and a byte comparison does not.
            spellings.push(std::path::PathBuf::from(
                repo.to_string_lossy().to_uppercase(),
            ));
            spellings.push(std::path::PathBuf::from(
                repo.to_string_lossy().replace('\\', "/"),
            ));
        }
        #[cfg(unix)]
        {
            // A symlink is a different path to the same directory, and resolving it is the
            // whole of what `CanonicalPath` promises.
            let link = repo.with_file_name("linked-repo");
            let _ = std::fs::remove_file(&link);
            if std::os::unix::fs::symlink(repo, &link).is_ok() {
                spellings.push(link);
            }
        }
        spellings
    }

    /// **The idempotency rule, measured against the daemon rather than against proto.**
    ///
    /// `ProjectId::from_canonical_path` is pure and is tested where it lives. What is tested
    /// nowhere else is that the *daemon* canonicalises before deriving one, and the
    /// acceptance test cannot see it: it registers a folder once.
    ///
    /// The portable second spelling is `…/repo/../repo`, chosen because
    /// `ProjectId::normalise` does **not** resolve `..` — only `fs::canonicalize` does. A
    /// daemon that hashed what the caller typed answers two ids for it, on every platform.
    /// Case and symlinks are added on top because each bites on one platform only, and a
    /// missing canonicalisation is invisible on macOS by luck: `temp_dir()` is under `/var`,
    /// a symlink to `/private/var` that only resolving folds.
    #[tokio::test]
    async fn registering_one_folder_twice_through_different_spellings_is_one_project() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-idempotent");
        let repo = scratch.repository("repo");
        let (dir, service) = service("idempotent");

        let first = register(&service, &repo);
        assert!(
            !first.already_registered,
            "the first registration created the project"
        );

        // **The proof this test trips** (traps register #12). It is only worth running if its
        // second spelling is one that nothing *but* canonicalising folds, and that is a
        // property of `ProjectId::normalise` rather than something to assume: normalise trims
        // separators and, on Windows, folds case and slashes — it does not resolve `..`. So
        // the id derived from the raw spelling must differ from the one the daemon answered.
        // Without this the whole test would pass against a daemon that never canonicalised at
        // all, because two calls with the same argument agree however wrong they both are.
        let traversed = repo.join("..").join(
            repo.file_name()
                .expect("the fixture repository has a folder name"),
        );
        assert_ne!(
            ProjectId::from_canonical_path(&traversed).expect("a temp directory is Unicode"),
            first.project.id,
            "the second spelling must be one only canonicalisation folds, or this test \
             measures nothing"
        );

        for spelling in spellings(&repo) {
            let again = register(&service, &spelling);
            assert!(
                again.already_registered,
                "{} is the same folder, so registering it again is not a second project",
                spelling.display()
            );
            assert_eq!(
                again.project.id,
                first.project.id,
                "{} must derive the id the daemon already holds for that folder",
                spelling.display()
            );
        }

        let projects = listed(&service).await;
        assert_eq!(
            projects
                .iter()
                .map(|project| &project.id)
                .collect::<Vec<_>>(),
            vec![&first.project.id],
            "one folder, however it is spelled, is one project"
        );

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Pointing at a folder *inside* a repository registers the repository.
    ///
    /// `git::inspect` deliberately does not fold "inside a repository" into "is a
    /// repository", leaving the choice to its caller; this is that choice, and the reason is
    /// the rule above. Without it `…/repo` and `…/repo/sub` are two ids for one repository —
    /// the same project in the sidebar twice, with the same worktree list, and nothing to
    /// tell the two apart by.
    #[tokio::test]
    async fn registering_a_folder_inside_a_repository_registers_the_repository() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-inside");
        let repo = scratch.repository("repo");
        let inside = scratch.folder("repo/crates/core");
        let (dir, service) = service("inside");

        let root = register(&service, &repo);
        let below = register(&service, &inside);

        assert_eq!(
            below.project.id, root.project.id,
            "a folder inside a repository is that repository"
        );
        assert!(below.already_registered);
        assert_eq!(listed(&service).await.len(), 1);

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A registered repository reports the branch its checkout is on, marked primary.
    #[tokio::test]
    async fn a_registered_repository_reports_the_branch_its_checkout_is_on() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-branch");
        let repo = scratch.repository("repo");
        let (dir, service) = service("branch");

        let registered = register(&service, &repo);
        assert_eq!(registered.project.name, "repo", "the folder's own name");
        assert_eq!(registered.project.group, Project::DEFAULT_GROUP);

        let projects = listed(&service).await;
        let [project] = projects.as_slice() else {
            panic!("one repository is one project, got {projects:?}");
        };
        let [worktree] = project.worktrees.as_slice() else {
            panic!("one checkout is one worktree, got {:?}", project.worktrees);
        };
        assert_eq!(worktree.branch, Scratch::BRANCH);
        assert!(
            worktree.is_primary,
            "the checkout the project was registered from is the primary one"
        );
        assert!(
            worktree.sessions.is_empty(),
            "no session was started in it, so none is listed under it"
        );

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §3.2's cases, each said in its own words rather than as "could not register".
    #[tokio::test]
    async fn each_thing_a_folder_can_turn_out_to_be_is_said_in_its_own_words() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-refusals");
        let plain = scratch.folder("plain");
        let many = scratch.folder("many");
        scratch.repository("many/one");
        scratch.repository("many/two");
        let (dir, service) = service("refusals");

        let not_a_repository = refused(service.register(&ProjectRegister { path: plain }));
        assert_eq!(*not_a_repository.code(), ErrorCode::NotARepository);

        let several = refused(service.register(&ProjectRegister { path: many }));
        assert_eq!(*several.code(), ErrorCode::ManyRepositories);
        assert!(
            several.message().contains('2'),
            "the refusal states how many it found: {}",
            several.message()
        );
        assert!(
            several
                .next_steps()
                .iter()
                .any(|step| step.contains("one") && step.contains("two")),
            "it names them, so the person can pick one: {:?}",
            several.next_steps()
        );

        assert!(
            listed(&service).await.is_empty(),
            "Nysia registers one repository at a time, so a folder of them registers none"
        );

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// **The leak guard.** A refusal names no path — the caller's or the daemon's own.
    ///
    /// An error envelope is the thing a daemon is most likely to log verbatim, and a
    /// repository path names a person's disk (traps register #13/#14). `RegisterRefusal` has
    /// no field to carry one; this is what stops the daemon putting one there anyway. Every
    /// [`GitError`] and [`PathError`] variant's `Display` contains a path, so a single
    /// `err.to_string()` anywhere on a refusal path fails this.
    ///
    /// # The second half, and why it was missing
    ///
    /// This used to cover only the path a *caller* supplied, and #94 was right that no
    /// envelope carried one of those. What it did not cover is the daemon's own database
    /// path, which `store_refusal` and `list_refusal` rendered straight into the message:
    ///
    /// ```text
    /// the daemon could not read its project list: could not read the id of a project in the
    /// store at C:\Users\kacpe\AppData\Local\…\nysia.db: …
    /// ```
    ///
    /// Not a caller's path, and still a person's account name, in an envelope that reaches a
    /// window and gets pasted into issues. The guard is widened rather than the finding
    /// argued with: a refusal names no path, and which path it was is not the interesting
    /// part.
    #[tokio::test]
    async fn a_refusal_does_not_repeat_the_path_it_was_given() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-no-leak");
        // A distinctive component, so the assertion cannot pass merely because the path was
        // short or ordinary.
        let missing = scratch.root().join("a-clients-private-repository");
        let (dir, service) = service("no-leak");

        let envelope = refused(service.register(&ProjectRegister {
            path: missing.clone(),
        }));
        assert_eq!(*envelope.code(), ErrorCode::PathUnreadable);

        let said = format!("{} {:?}", envelope.message(), envelope.next_steps());
        for secret in [
            missing.to_string_lossy().into_owned(),
            "a-clients-private-repository".to_owned(),
        ] {
            assert!(
                !said.contains(&secret),
                "the refusal carried {secret:?}, which names the caller's disk: {said}"
            );
        }

        // The daemon's own database path, through the two envelopes that used to render a
        // whole `StoreError`. Every path-carrying variant spells the file, so one variant
        // stands for all of them — and the one chosen is the one #94 measured.
        let database = dir.join("nysia.db");
        let store_leak = StoreError::Sqlite {
            action: "read the id of a project",
            path: database.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        };
        for envelope in [store_refusal(&store_leak), list_refusal(&store_leak)] {
            let said = format!("{} {:?}", envelope.message(), envelope.next_steps());
            for secret in [
                database.to_string_lossy().into_owned(),
                "nysia.db".to_owned(),
                whoami(),
            ] {
                assert!(
                    !said.contains(&secret),
                    "the refusal carried {secret:?}, which names the daemon's own disk and \
                     the account it runs as: {said}"
                );
            }
            assert!(
                !envelope.next_steps().is_empty(),
                "and still says what to do about it"
            );
        }

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The account this process runs as, as it appears inside a profile path.
    ///
    /// Asserted on as well as the path itself, because a message could name the account
    /// without naming the whole file — and that is the part that identifies a person.
    fn whoami() -> String {
        for var in ["USERNAME", "USER", "LOGNAME"] {
            if let Some(name) = std::env::var_os(var)
                && !name.is_empty()
            {
                return name.to_string_lossy().into_owned();
            }
        }
        // Nothing to compare against rather than something that matches everything.
        "\u{0}".to_owned()
    }

    /// Forgetting an id nothing is registered under is refused, not quietly accepted.
    ///
    /// The alternative — treating it as idempotent — exits zero on a typo and leaves the
    /// person believing they forgot something they did not. It mirrors `session_close` on a
    /// stale handle.
    #[tokio::test]
    async fn forgetting_a_project_that_is_not_there_says_so() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-forget");
        let repo = scratch.repository("repo");
        let (dir, service) = service("forget");

        let registered = register(&service, &repo);
        let id = registered.project.id.clone();

        assert!(matches!(
            service.forget(&ProjectForget { id: id.clone() }),
            ResponsePayload::ProjectForget
        ));
        assert!(
            listed(&service).await.is_empty(),
            "the registration is gone"
        );
        assert!(
            repo.join(".git").exists(),
            "forgetting removes the registration and nothing on disk"
        );

        let again = refused(service.forget(&ProjectForget { id }));
        assert_eq!(*again.code(), ErrorCode::UnknownProject);

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `Start →` end to end in the daemon: a worktree, a session in it, and the tab's three
    /// values in one answer.
    ///
    /// The second start is the contract the coordinator wrote into both wave C specs: an
    /// existing worktree is **adopted** rather than refused, and the answer says which
    /// happened. It also proves the session lands *in the worktree* rather than in the
    /// project root, which is the difference between a branch-keyed workspace and a tab that
    /// merely says a branch name.
    #[tokio::test]
    async fn starting_a_branch_opens_a_worktree_and_a_session_in_it_and_adopts_it_next_time() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-start");
        let repo = scratch.repository("repo");
        let (dir, service) = service("start");
        let registered = register(&service, &repo);

        let first = match service.start(&ProjectStart {
            project: registered.project.id.clone(),
            branch: "feat/projects".to_owned(),
            kind: nysia_proto::SessionKind::Shell,
            profile: None,
        }) {
            ResponsePayload::ProjectStart(started) => started,
            other => panic!("the branch should have started, got {other:?}"),
        };
        assert_eq!(first.branch, "feat/projects");
        assert!(!first.adopted, "nothing was there to adopt");

        // The session is in the worktree, not in the project root. `summaries_under` is what
        // the sidebar lists a worktree's sessions with, so asking it is asking the same
        // question the window asks.
        let root = CanonicalPath::of(&repo).expect("the project root is on disk");
        let worktree =
            CanonicalPath::of(repo.join(".nysia").join("worktrees").join("feat-projects"))
                .expect("the worktree is on disk");
        let folders = vec![root.clone(), worktree.clone()];
        let inside = service.sessions.summaries_under(&worktree, &folders);
        assert_eq!(
            inside.iter().map(|s| &s.handle).collect::<Vec<_>>(),
            vec![&first.handle],
            "the session it opened is the session that worktree holds"
        );
        // **And the main worktree does not also hold it.** Nysia's worktrees live *inside*
        // the main one, so `contains` is true of both and the positive assertion above passes
        // either way — which is why it was green while one session was listed twice.
        assert!(
            service.sessions.summaries_under(&root, &folders).is_empty(),
            "a session in a nested worktree belongs to the deepest worktree containing it, \
             not to every one"
        );

        let second = match service.start(&ProjectStart {
            project: registered.project.id.clone(),
            branch: "feat/projects".to_owned(),
            kind: nysia_proto::SessionKind::Shell,
            profile: None,
        }) {
            ResponsePayload::ProjectStart(started) => started,
            other => panic!("the branch should have started again, got {other:?}"),
        };
        assert!(
            second.adopted,
            "a worktree that is already there is adopted, never refused"
        );
        assert_ne!(
            second.handle, first.handle,
            "adopting the worktree still opens a second session in it"
        );
        assert_ne!(second.pane_key, first.pane_key, "and its own pane");

        // The project now lists the branch beside its primary checkout, with the sessions
        // under it — which is the whole §3.1 shape, composed live.
        let projects = listed(&service).await;
        let [project] = projects.as_slice() else {
            panic!("one repository is one project, got {projects:?}");
        };
        let started = project
            .worktrees
            .iter()
            .find(|worktree| worktree.branch == "feat/projects")
            .expect("the started branch is one of the project's worktrees");
        assert!(
            !started.is_primary,
            "the checkout it was registered from is"
        );
        assert_eq!(
            started.sessions.len(),
            2,
            "both sessions are listed under it"
        );
        let primary = project
            .worktrees
            .iter()
            .find(|worktree| worktree.is_primary)
            .expect("the checkout the project was registered from");
        assert!(
            primary.sessions.is_empty(),
            "and under nothing else: the primary checkout contains the worktree they are in, \
             which is what listed each of them under both, got {:?}",
            primary.sessions
        );

        for handle in [first.handle, second.handle] {
            let _ = service.sessions.close(&handle);
        }
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Starting a project nothing is registered under is refused by id.
    #[tokio::test]
    async fn starting_a_project_that_is_not_registered_says_so() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let (dir, service) = service("start-unknown");
        let envelope = refused(
            service.start(&ProjectStart {
                project: ProjectId::from_canonical_path(std::path::Path::new("/nowhere/at/all"))
                    .expect("a unicode path"),
                branch: "feat/projects".to_owned(),
                kind: nysia_proto::SessionKind::Shell,
                profile: None,
            }),
        );
        assert_eq!(*envelope.code(), ErrorCode::UnknownProject);

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A registered folder that has stopped being a repository still lists.
    ///
    /// Dropping it would lose a project the store still holds — and the next registration of
    /// that same folder would answer `alreadyRegistered: true` for something the sidebar
    /// never showed. It lists with no worktrees, which is what "git did not describe it"
    /// looks like on the wire, and forgetting it stays the person's decision.
    #[tokio::test]
    async fn a_project_git_can_no_longer_describe_is_listed_without_its_worktrees() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-degraded");
        let repo = scratch.repository("repo");
        let (dir, service) = service("degraded");
        let registered = register(&service, &repo);

        // The folder stays and stops being a repository, which is the half that can be
        // arranged portably. An unplugged drive reaches the same answer by the other route.
        std::fs::remove_dir_all(repo.join(".git")).expect("the git directory can be removed");

        let projects = listed(&service).await;
        let [project] = projects.as_slice() else {
            panic!("the project is still registered, got {projects:?}");
        };
        assert_eq!(project.id, registered.project.id);
        assert!(
            project.worktrees.is_empty(),
            "git cannot describe it, so it has no worktrees to report"
        );

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }
    // ---------------------------------------------------------------------------------
    // The live drive: a real daemon, a real socket, and the real agent CLI.
    // ---------------------------------------------------------------------------------

    /// How long to wait for the agent to paint something and for a hook to arrive.
    ///
    /// Generous, because a first run of the CLI in a folder it has not seen asks about trust
    /// before it draws anything, and because the whole point of the test is what a person
    /// actually sees rather than what a fixture can be made to do quickly.
    const LIVE_DEADLINE: Duration = Duration::from_secs(90);

    /// What the CLI draws once it is past trust and has a session.
    ///
    /// # Why three strings out of the agent's screen are in `rpc/`
    ///
    /// D-4 keeps Claude's specifics in one module and this file is not it — but the rule is
    /// about the **seam**, and the seam is `program_for`, which names no agent and must not.
    /// These three are a test's knowledge of what it is typing at: driving a real CLI through
    /// its trust prompt and reading its banner is not a seam, it is the CLI. They are grouped
    /// here so that a release that moves them is one edit, and the assertion the proof rests
    /// on is the neutral one below — the agent answered with a word this test chose.
    const AGENT_BANNER: &str = "Claude Code v";

    /// What it draws instead when the credentials it was left with are not enough.
    const AGENT_NO_AUTH: &str = "Invalid API key";

    /// The folder-trust question the CLI asks the first time it opens a worktree.
    const AGENT_TRUST: &str = "trust this folder";

    /// What this test asks the agent to say, so that "it answered" is something to look for.
    const AGENT_WORD: &str = "pomegranate";

    /// `Start ->` on a branch, with a real agent in the worktree, proved end to end.
    ///
    /// # Why this is `#[ignore]`d rather than skipped like the rest
    ///
    /// Everything else in this module skips on [`git_or_skip`] and runs everywhere. This one
    /// cannot: it needs the agent CLI installed, a machine whose credentials that CLI will
    /// accept, and `NYSIA_RUNTIME_DIR` pointing somewhere this test may bind a socket — and
    /// `nysia hook`, which the agent spawns, finds this daemon through that same variable or
    /// not at all. Three preconditions no CI runner meets. An `#[ignore]` says that once, in
    /// the place somebody reads before running it; a silent skip would be `path.rs`'s
    /// `a_short_name_expands_to_the_long_one` again — green on every machine and doing its
    /// job on none.
    ///
    /// Run it with `NYSIA_RUNTIME_DIR` set to a directory you own:
    ///
    /// ```text
    /// cargo test -p nysia-core --lib -- --ignored --nocapture a_real_agent
    /// ```
    ///
    /// # What it is for
    ///
    /// The fixture tests next door prove the mechanism — `agent::claude::launch`'s session
    /// tests spawn a CLI they wrote themselves and drive it to a prompt on both legs. Two
    /// things they structurally cannot answer, and this is where they get answered:
    ///
    /// 1. **Authentication.** §7.1 scrubs `ANTHROPIC_API_KEY` and the three
    ///    `CLAUDE_CODE_*SESSION*` markers from every session, which is correct and stays. On
    ///    a machine authenticated by subscription rather than by key, whether what is left is
    ///    enough is a question about that machine, and the only honest way to ask it is to
    ///    start one and look.
    /// 2. **Status.** §3.2 makes the pane-key variable a *hint* and the process tree the
    ///    proof, and the tree an agent started by the daemon **in a worktree** sits in is a
    ///    shape the ancestry walk had never met. The dot is v0.2's whole point.
    #[tokio::test]
    #[ignore = "needs the agent CLI, a machine it can authenticate on, and NYSIA_RUNTIME_DIR"]
    async fn a_real_agent_starts_in_a_worktree_and_lights_its_dot() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let agent = crate::agent::launch(std::iter::empty::<&std::ffi::OsStr>())
            .expect("this test needs the agent CLI on PATH; it is the thing being driven");
        println!("[live] agent resolves to {}", agent.program().display());

        let scratch = Scratch::new("live-agent");
        let repo = scratch.repository("repo");

        // A real daemon on the endpoint this process's environment names, which is the same
        // endpoint the `nysia hook` the agent spawns will resolve.
        let endpoint = Endpoint::from_env().expect("NYSIA_RUNTIME_DIR must name a usable dir");
        println!("[live] daemon endpoint {}", endpoint.listening());
        let (daemon, listener) = Daemon::bind(DaemonConfig {
            idle_retire_after: None,
            ..DaemonConfig::new(endpoint.clone())
        })
        .expect("a daemon binds on the endpoint the environment names");
        let serving = tokio::spawn({
            let daemon = Arc::clone(&daemon);
            async move {
                let _ = daemon.serve(listener).await;
            }
        });

        let client_id: ClientId = "nysia-live".parse().expect("a client id");
        let mut client = Client::connect(&endpoint, &client_id, ClientRole::Control)
            .await
            .expect("the daemon accepts a control connection");

        let project = client
            .project_register(repo.clone())
            .await
            .expect("the fixture repository registers")
            .project;

        // Phase one: a shell, only to make the worktree so the agent's hooks can be written
        // into it before the agent is started. `nysia agent hooks install` writes the user's
        // own settings file; this writes the worktree's, so the machine running the test is
        // left exactly as it was.
        let branch = "feat/live-agent";
        let shell = client
            .project_start(ProjectStart {
                project: project.id.clone(),
                branch: branch.to_owned(),
                kind: SessionKind::Shell,
                profile: None,
            })
            .await
            .expect("a shell starts the branch");
        client
            .session_close(shell.handle)
            .await
            .expect("the shell closes");

        let worktree = repo
            .join(crate::worktree::WORKTREE_BASE[0])
            .join(crate::worktree::WORKTREE_BASE[1])
            .join("feat-live-agent");
        assert!(worktree.is_dir(), "{} is not there", worktree.display());
        let settings = worktree.join(".claude").join("settings.json");
        std::fs::create_dir_all(
            settings
                .parent()
                .expect("the settings file has a directory"),
        )
        .expect("a project settings directory");
        let nysia = built_nysia();
        let change =
            crate::agent::hooks::install(&settings, &nysia).expect("the worktree's hooks install");
        println!(
            "[live] installed {} hooks into {} pointing at {}",
            change.installed,
            settings.display(),
            nysia.display()
        );

        // Phase two: the agent, in the worktree the shell made, adopted by branch (D-6).
        let started = client
            .project_start(ProjectStart {
                project: project.id.clone(),
                branch: branch.to_owned(),
                kind: SessionKind::Agent,
                profile: None,
            })
            .await
            .expect("an agent session starts in the worktree");
        assert!(started.adopted, "the worktree the shell made is adopted");
        println!(
            "[live] agent session {} in pane {}",
            started.handle, started.pane_key
        );

        let sessions = client.session_list().await.expect("the session lists");
        let agent_row = sessions
            .iter()
            .find(|summary| summary.handle == started.handle)
            .expect("the agent session is listed");
        assert_eq!(agent_row.kind, SessionKind::Agent);
        println!("[live] tab title {:?}", agent_row.title);
        assert!(
            !agent_row.title.contains('/') && !agent_row.title.contains('\\'),
            "a tab is labelled with a name, never a path: {:?}",
            agent_row.title
        );

        // The CLI opens in a folder it has never seen, so the first thing on the screen is
        // its trust prompt rather than a REPL. Answering it is part of driving the real
        // thing: a test that stopped here would be reporting "it painted something".
        let trust = live_until(&mut client, &started.handle, |text| {
            text.contains(AGENT_TRUST)
        })
        .await;
        println!("[live] ---- trust prompt ----\n{trust}\n[live] ----------------");
        assert!(
            trust.contains(
                &worktree
                    .file_name()
                    .expect("the worktree has a name")
                    .to_string_lossy()
                    .into_owned()
            ),
            "the agent opened somewhere other than the worktree"
        );
        // Down, then return: the default is "No, exit".
        live_send(&mut client, &started.handle, "\u{1b}[B", false).await;
        live_send(&mut client, &started.handle, "", true).await;

        // It reaches a prompt, and this is the **authentication** question: the session scrub
        // takes `ANTHROPIC_API_KEY` with it deliberately, so whether what is left is enough
        // on a subscription machine is not something to reason about. The banner is the
        // signal rather than a string out of the CLI's chrome, which moves between releases:
        // it is drawn once, after trust is answered and a session exists.
        let screen = live_until(&mut client, &started.handle, |text| {
            text.contains(AGENT_BANNER) || text.contains(AGENT_NO_AUTH)
        })
        .await;
        println!("[live] ---- prompt ----\n{screen}\n[live] ----------------");
        assert!(
            !screen.contains(AGENT_NO_AUTH),
            "the session scrub left the CLI unable to authenticate; widening the scrub is not \
             the fix, and a security default a caller can undo is not a default"
        );
        assert!(
            screen.contains(AGENT_BANNER),
            "the CLI never reached its prompt; screen was:\n{screen}"
        );

        // And the dot lights. The hook the agent runs resolves its pane from the process
        // tree, not from the hint in its environment, so this is the ancestry walk answering
        // for a tree the daemon started inside a worktree.
        // Typed, then submitted, in two writes. The CLI's input box reads a whole line
        // arriving at once as a paste and keeps the trailing return with it, so a single
        // `enter: true` send leaves the prompt sitting in the box unsent — which looks
        // exactly like an agent that never answered.
        live_send(
            &mut client,
            &started.handle,
            &format!("reply with exactly the word {AGENT_WORD} and nothing else"),
            false,
        )
        .await;
        live_send(&mut client, &started.handle, "", true).await;
        let statuses = live_status(&mut client, &started.pane_key).await;
        println!("[live] statuses: {statuses:?}");
        let [status] = statuses.as_slice() else {
            panic!("the agent's pane has one status, got {statuses:?}");
        };
        assert_eq!(status.pane(), &started.pane_key);
        assert!(
            !status.lead.restored_unconfirmed,
            "the pane must have been **proved** from the process tree; an unconfirmed row is \
             the hint being taken on trust, which §3.2 forbids"
        );

        // **The neutral proof, and the one the rest rests on.** An agent that could not
        // authenticate, or that was handed an environment marking it somebody's child
        // session, never gets this far — whatever its banner said. Twice, because the word
        // is on the screen once already as the line that was typed.
        let after = live_until(&mut client, &started.handle, |text| {
            text.matches(AGENT_WORD).count() > 1
        })
        .await;
        println!("[live] ---- after ----\n{after}\n[live] ----------------");
        assert!(
            after.matches(AGENT_WORD).count() > 1,
            "the agent never answered; the screen shows only what was typed:\n{after}"
        );

        let _ = client.session_close(started.handle).await;
        daemon.shutdown();
        serving.abort();
    }

    /// Where `cargo build -p nysia` leaves the binary the hooks point at.
    fn built_nysia() -> std::path::PathBuf {
        let exe = std::env::current_exe().expect("the test binary");
        // `target/<profile>/deps/nysia_core-<hash>.exe` -> `target/<profile>/nysia`.
        let profile_dir = exe
            .parent()
            .and_then(std::path::Path::parent)
            .expect("the test binary is under target/<profile>/deps");
        let nysia = profile_dir.join(if cfg!(windows) { "nysia.exe" } else { "nysia" });
        assert!(
            nysia.is_file(),
            "{} is not there; run `cargo build -p nysia` first",
            nysia.display()
        );
        nysia
    }

    /// Type at the session, the way a person at the tab would.
    async fn live_send(
        client: &mut Client,
        handle: &nysia_proto::SessionHandle,
        text: &str,
        enter: bool,
    ) {
        client
            .terminal_send(nysia_proto::TerminalSend {
                handle: handle.clone(),
                text: text.to_owned(),
                enter,
                interrupt: false,
            })
            .await
            .expect("the session takes input");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// Read the session's screen until `predicate` holds, or give up at [`LIVE_DEADLINE`].
    async fn live_until(
        client: &mut Client,
        handle: &nysia_proto::SessionHandle,
        predicate: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = std::time::Instant::now() + LIVE_DEADLINE;
        loop {
            let read = client
                .terminal_read(nysia_proto::TerminalRead {
                    handle: handle.clone(),
                    mode: nysia_proto::ReadMode::Screen,
                    cursor: None,
                    limit: None,
                })
                .await
                .expect("the screen reads");
            let text = read.lines.join("\n");
            if predicate(&text) || std::time::Instant::now() >= deadline {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Poll for a status row against `pane`, returning whatever was there at the deadline.
    async fn live_status(
        client: &mut Client,
        pane: &nysia_proto::PaneKey,
    ) -> Vec<nysia_proto::AgentStatus> {
        let deadline = std::time::Instant::now() + LIVE_DEADLINE;
        loop {
            let statuses = client.agent_status_list().await.expect("status lists");
            let ours: Vec<_> = statuses
                .into_iter()
                .filter(|status| status.pane() == pane)
                .collect();
            if !ours.is_empty() || std::time::Instant::now() >= deadline {
                return ours;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    /// Everything a `Start →` leaves behind when it cannot finish, measured.
    ///
    /// #94's finding: `conceal` ran before the free-directory search and the branch lookup,
    /// so a start that was going to be refused created `.nysia/` and a `.gitignore` in
    /// somebody's repository first, and the refusal mentioned neither. `--branch HEAD` and
    /// `--branch nul` were the two measured — both pass `git check-ref-format` and both fail
    /// at `worktree add`, which is confirmed here rather than taken on trust.
    ///
    /// What is asserted is the decision `worktree::ensure` now documents:
    ///
    /// - refused **before** anything is created, and the repository is untouched
    /// - refused **at `worktree add`**, and what remains is Nysia's own directory holding
    ///   nothing but the `.gitignore` that hides it — no worktree, no branch, and `git
    ///   status` still clean — with the answer saying so
    #[tokio::test]
    async fn a_start_that_cannot_finish_says_what_it_left_behind() {
        let Some(_git) = git_or_skip() else {
            return;
        };
        let scratch = Scratch::new("project-leftovers");
        let repo = scratch.repository("repo");
        let (dir, service) = service("leftovers");
        let registered = register(&service, &repo);
        let nysia_dir = repo.join(crate::worktree::WORKTREE_BASE[0]);

        // 1. A name git will not take at all. Refused from `check-ref-format`, before the
        //    first syscall that writes.
        let refusal = refused(service.start(&ProjectStart {
            project: registered.project.id.clone(),
            branch: "feat/bad..name".to_owned(),
            kind: SessionKind::Shell,
            profile: None,
        }));
        assert_eq!(*refusal.code(), ErrorCode::InvalidRequest);
        assert!(
            !nysia_dir.exists(),
            "a start refused for its name must leave the repository exactly as it found it"
        );

        // 2. A refusal from **past** the name check: every directory the branch could use is
        //    taken. This is the case the ordering fix is for and the one part 1 cannot see —
        //    `check-ref-format` refuses before `nysia_dir` is even computed, so it was
        //    already leaving nothing. `conceal` used to run before the free-directory search,
        //    so this branch is where it wrote a `.gitignore` for a start that was never going
        //    to happen.
        let base = nysia_dir.join(crate::worktree::WORKTREE_BASE[1]);
        for attempt in 1..=16 {
            let name = if attempt == 1 {
                "crowded".to_owned()
            } else {
                format!("crowded-{attempt}")
            };
            std::fs::create_dir_all(base.join(name)).expect("an occupied directory");
        }
        let refusal = refused(service.start(&ProjectStart {
            project: registered.project.id.clone(),
            branch: "crowded".to_owned(),
            kind: SessionKind::Shell,
            profile: None,
        }));
        assert_eq!(*refusal.code(), ErrorCode::InvalidRequest);
        assert!(
            !nysia_dir.join(".gitignore").exists(),
            "a start refused after the name check must not have written anything either; \
             concealment belongs immediately before `worktree add`, which is the only step \
             that can fail with something on disk"
        );
        std::fs::remove_dir_all(&nysia_dir).expect("the fixture's own directories");

        // 3. A name `check-ref-format` accepts and `worktree add` will not. This is the one
        //    step that can fail with something already on disk.
        let refusal = refused(service.start(&ProjectStart {
            project: registered.project.id.clone(),
            branch: "HEAD".to_owned(),
            kind: SessionKind::Shell,
            profile: None,
        }));
        assert_eq!(*refusal.code(), ErrorCode::Internal);
        assert!(
            refusal
                .next_steps()
                .iter()
                .any(|step| step.contains(".nysia")),
            "the answer must name what it left behind, got {:?}",
            refusal.next_steps()
        );

        // And what it left is only that: no worktree, no branch, nothing `git status` sees.
        assert!(nysia_dir.is_dir());
        let left: Vec<String> = std::fs::read_dir(&nysia_dir)
            .expect("Nysia's directory reads")
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(
            left,
            vec![".gitignore".to_owned()],
            "the only thing a failed start leaves is the file that hides it"
        );
        let status = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&repo)
                .output()
                .expect("git status runs")
                .stdout,
        )
        .into_owned();
        assert!(status.trim().is_empty(), "git sees it: {status:?}");

        let projects = listed(&service).await;
        let [project] = projects.as_slice() else {
            panic!("one repository is one project, got {projects:?}");
        };
        assert_eq!(
            project.worktrees.len(),
            1,
            "no worktree was made, so the project still has only its checkout"
        );

        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// No next step reaches a person with the source's own indentation in it.
    ///
    /// #94's finding, as a rule rather than as three fixed strings: a Rust string literal
    /// split across lines without a trailing `\` keeps every space of the continuation, so
    /// `` `git check-ref-format --branch <name>` is the              same question this
    /// asked `` is what a user saw. Nothing this module says has a reason to contain a run of
    /// spaces, so the rule is simply that none does — and every envelope the module can build
    /// is put through it, so a fourth one added later is covered without anybody remembering
    /// to add it here.
    #[test]
    fn nothing_this_module_says_carries_the_source_indentation_with_it() {
        let envelopes = vec![
            start_refusal(StartError::BranchRefused {
                branch: "feat/x".to_owned(),
                reason: "git check-ref-format refused it",
            }),
            start_refusal(StartError::BranchPrunable {
                branch: "feat/x".to_owned(),
            }),
            start_refusal(StartError::NoDirectory {
                branch: "feat/x".to_owned(),
            }),
            no_git_envelope(),
            internal_start("something"),
            joining("something"),
            unknown_project(
                &ProjectId::from_canonical_path(std::path::Path::new("C:/x")).expect("an id"),
            ),
        ];
        for envelope in &envelopes {
            for text in std::iter::once(envelope.message())
                .chain(envelope.next_steps().iter().map(String::as_str))
            {
                assert!(
                    !text.contains("  "),
                    "a wrapped literal kept its indentation: {text:?}"
                );
            }
        }
    }
}
