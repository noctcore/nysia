//! Projects: a registered folder on disk, and what git says about it.
//!
//! `docs/plans/v0.3-delivery-plan.md` §3 is the authority for this shape and this module is
//! its Rust spelling. Three workers touch it — proto spells it, the store persists it, the
//! daemon serves it — so there is one definition and nobody writes a second.
//!
//! A project is a **registered folder**, plus what git says about it. Nysia does not own the
//! folder, does not move it, and does not write into it beyond ordinary git operations.
//! [`ProjectForget`] un-registers; it never deletes anything on disk.
//!
//! The field names match what the window already renders (`apps/web/src/store/types.ts`)
//! rather than inventing a parallel shape: wave B replaces those hand-written interfaces
//! with the ones generated from here, and a gratuitous difference would cost that worker a
//! translation layer for nothing.
//!
//! # The identity rule
//!
//! [`ProjectId`] is derived from the canonical path and from nothing else. Not random, and
//! not a row id: a project that comes back under a different id after a daemon restart is a
//! sidebar that reorders itself every time the daemon comes back, and the acceptance test
//! `crates/nysia/tests/projects.rs` measures exactly that. There is one constructor,
//! [`ProjectId::from_canonical_path`], so "derived" is a property of the type rather than a
//! rule someone has to remember.
//!
//! # What is deliberately absent
//!
//! **The path.** §3's table has four fields and none of them is where the folder is. A
//! project row reaches the window, the log file and any export, and a repository path names
//! a person's disk (traps register #13/#14). The daemon holds the path; the wire carries the
//! id derived from it. A path travels in one direction only — into [`ProjectRegister`] —
//! and every refusal this module builds is free of one, so a daemon that logs an error
//! envelope verbatim cannot widen what reaches a log file.
//!
//! **A task model.** D-5 stands: tasks are GitHub Issues, queried live.
//!
//! **Rename and regroup.** §3 says `name` is "renameable later", and later is not v0.3.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{ErrorCode, ErrorEnvelope, NextSteps};
use crate::newtype::deserialize_via_from_str;
use crate::session::SessionSummary;

/// Why a project value could not be read.
///
/// Neither variant carries a path. The daemon knows which path it was handed — it is in the
/// request it is answering — and an error type that repeats it is one more place a path can
/// reach a log line (traps register #13/#14).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    /// A project id was not `proj_` followed by 32 lowercase hex digits.
    #[error("a project id is `proj_<32 lowercase hex digits>`, got {0:?}")]
    ProjectIdShape(String),
    /// A path was not valid Unicode, so it has no one spelling to derive an id from.
    #[error("a project path must be valid Unicode for its id to be stable")]
    PathNotUnicode,
}

/// Every project id carries this prefix, so one is recognisable in a log line.
const PROJECT_ID_PREFIX: &str = "proj_";

/// The digest's width in hex digits. 128 bits, four bits to the digit.
const PROJECT_ID_DIGITS: usize = 32;

/// FNV-1a's 128-bit offset basis.
const FNV_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;

/// FNV-1a's 128-bit prime.
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// FNV-1a over `bytes`, 128 bits wide.
///
/// Written out rather than taken from a dependency or from [`std::hash::DefaultHasher`],
/// for two reasons that both come back to the identity rule:
///
/// - `DefaultHasher`'s algorithm is explicitly unspecified and may change between Rust
///   releases. An id that moved when the toolchain was bumped is precisely the reordering
///   sidebar this type exists to prevent, and it would move on a day nobody was looking at
///   projects.
/// - A crate would have to be added to the workspace dependency table, which is
///   coordinator-owned, to compute a digest that is not a security boundary. Nothing
///   authenticates a project id; it is a key, and collision resistance against an adversary
///   buys nothing here. Against accident, 128 bits over the handful of folders one person
///   registers is not a risk worth a shared-file edit.
///
/// `the_digest_has_not_moved` pins it against literals computed independently, so a
/// "tidy-up" that changed the constants or the byte order fails rather than silently
/// re-keying every project on disk.
fn digest(bytes: &[u8]) -> u128 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The one string form of a canonical path, as the digest sees it.
///
/// `windows` is a parameter rather than a `cfg!` so both rule sets are exercised on both CI
/// legs. The rules are a property of the *filesystem*, so a macOS runner cannot run the
/// Windows ones for real — but it can check that they are the rules, which is what
/// `the_windows_rules_fold_what_windows_calls_one_folder` does.
///
/// What it does, in order:
///
/// 1. Strips Windows' verbatim prefix. [`std::fs::canonicalize`] returns `\\?\C:\…` and
///    `\\?\UNC\server\share\…`; the second becomes `\\server\share\…`, which is the form
///    everything else on Windows writes.
/// 2. Turns `/` into `\`, because both name the same folder on Windows. Never on Unix,
///    where a backslash is an ordinary character in a filename.
/// 3. Folds ASCII case on Windows only. See [`ProjectId::from_canonical_path`] for why the
///    fold stops at ASCII and why Unix does not get one.
/// 4. Drops trailing separators, so `C:\src\nysia\` and `C:\src\nysia` are one project.
fn normalise(path: &str, windows: bool) -> String {
    if !windows {
        return path.trim_end_matches('/').to_owned();
    }

    let stripped = match path.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => rest.to_owned(),
        },
        None => path.to_owned(),
    };
    let folded = stripped.replace('/', r"\").to_ascii_lowercase();
    folded.trim_end_matches('\\').to_owned()
}

/// A project's durable identity: `proj_<32 lowercase hex digits>`.
///
/// **Stable across restarts and derived from the canonical path.** It is not minted, so
/// there is nothing to persist and nothing to look up: two daemons, or a daemon and the CLI
/// beside it, reach the same id for the same folder without asking each other. Registering
/// the same path twice is one project because it is one id, which is what makes §3.2's
/// idempotency a property rather than a check someone has to write.
///
/// # What "canonical" means, and who produces it
///
/// The canonical path is [`std::fs::canonicalize`]'s answer: absolute, with `.` and `..`
/// resolved, with every symlink followed, and spelled the way the filesystem spells it.
/// That call is the load-bearing step and it needs the filesystem, so **`nysia-core`
/// produces it** — this crate does no IO. What this crate owns is everything after it:
/// the normalisation rules below are the whole of the spelling rule, and this is the only
/// constructor — so a component holding a canonical path derives the id without a round
/// trip, and cannot derive a different one.
///
/// The string rules exist because canonicalising is not quite enough on Windows, where
/// `C:\Users\…` and `c:\users\…` are the same folder and not the same string:
///
/// - **Case is folded, on Windows only.** `fs::canonicalize` already returns the on-disk
///   case, so this catches a caller that skipped it and the drive letter, which is the one
///   component that is routinely typed either way. ASCII only: Unicode case folding is
///   locale-independent in Rust but can change a string's length, which makes the rule
///   harder to state than the mistake it catches.
/// - **Unix is not folded**, and that is deliberate rather than an omission. A
///   case-sensitive volume is a supported configuration on both Linux and macOS, so two
///   differently-cased paths there can be two different folders, and folding them would
///   merge two real projects into one. On a case-insensitive macOS volume it is
///   `fs::canonicalize` — `realpath` — that settles the spelling, which is another reason
///   core must canonicalise rather than hand over what the user typed.
///
/// One consequence worth stating: the id is stable **on one machine**, which is what
/// persistence needs. It is not a cross-platform identifier, because path identity itself
/// is not one. Nysia is local-only until the relay lands (v0.6+), and if an id ever has to
/// travel between machines that is a new decision rather than an assumption this type
/// already made.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
#[ts(export)]
pub struct ProjectId(String);

impl ProjectId {
    /// The id of the project at `path`, which must already be canonical.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::PathNotUnicode`] when the path is not valid Unicode. Such a
    /// path has no one spelling to hash, and hashing a lossy conversion of it would give
    /// two different folders the same id — a silent merge, in the one function the whole
    /// milestone's identity rests on. Refusing it says so.
    pub fn from_canonical_path(path: &Path) -> Result<Self, ProjectError> {
        let text = path.to_str().ok_or(ProjectError::PathNotUnicode)?;
        let digest = digest(normalise(text, cfg!(windows)).as_bytes());
        Ok(Self(format!(
            "{PROJECT_ID_PREFIX}{digest:0width$x}",
            width = PROJECT_ID_DIGITS
        )))
    }

    /// The whole id as it appears on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProjectId {
    type Err = ProjectError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let shape = || ProjectError::ProjectIdShape(s.to_owned());
        let digits = s.strip_prefix(PROJECT_ID_PREFIX).ok_or_else(shape)?;
        // Lowercase only, so one project has exactly one spelling and string equality is
        // meaningful — the same rule the uuid-shaped ids enforce by refusing the braced and
        // simple forms.
        if digits.len() != PROJECT_ID_DIGITS
            || !digits
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(shape());
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

deserialize_via_from_str!(ProjectId);

/// The sessions on one branch.
///
/// **Keyed by branch, never by task id** (D-6). Three shipped bugs in the system Nysia
/// replaces came from task-keying, and this is where that decision first bites: a worktree
/// outlives the task that created it, and two tasks on one branch share it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Worktree {
    /// The branch this worktree has checked out, which is also its key.
    ///
    /// A plain string, as `apps/web/src/store/types.ts` already has it. Git's own rules for
    /// what a branch may be called are `git check-ref-format`'s, and restating a subset of
    /// them here would refuse a name git accepts — a validation that is wrong in the
    /// direction that costs a user their worktree.
    pub branch: String,
    /// Whether this is the checkout the repository was registered from.
    pub is_primary: bool,
    /// The sessions running on this branch, as the sidebar lists them under it.
    pub sessions: Vec<SessionSummary>,
}

/// One registered folder, as the sidebar sees it.
///
/// Four fields, which is §3.1's table. See the module docs for why the path is not among
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Project {
    /// Stable across restarts, derived from the canonical path. See [`ProjectId`].
    pub id: ProjectId,
    /// What the sidebar shows. The folder's own name when it was registered.
    pub name: String,
    /// The sidebar's section header.
    ///
    /// Every project registered in v0.3 gets [`Project::DEFAULT_GROUP`]; grouping a project
    /// somewhere else is a later milestone's verb, and a field that nothing can change yet
    /// is still the right field, because the window already renders it.
    pub group: String,
    /// The worktrees git reports for it, primary first.
    pub worktrees: Vec<Worktree>,
}

impl Project {
    /// The section header a newly registered project lands under.
    ///
    /// `Dev`, which is what the design mock shows and what the window's own seed data uses.
    /// One spelling, in the crate that owns the wire, so the daemon and the store cannot
    /// disagree about it the first time someone looks at an empty sidebar.
    pub const DEFAULT_GROUP: &'static str = "Dev";
}

/// Register a folder.
///
/// The daemon canonicalises the path, derives the [`ProjectId`] from it, and registers the
/// repository there. **Idempotent**: registering the same folder twice is one project, and
/// the answer says which of the two happened — see
/// [`ProjectRegistered::already_registered`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectRegister {
    /// The folder to register, as the caller has it.
    ///
    /// Relative, unresolved and trailing-separator spellings are all fine: canonicalising is
    /// the daemon's job and doing it here would mean every client canonicalised, each
    /// slightly differently, which is the failure this whole type exists to make impossible.
    /// The daemon decides what it is looking at and answers one of [`RegisterRefusal`]'s
    /// cases when it is not a repository.
    pub path: PathBuf,
}

/// List every registered project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectList {}

/// Forget a project.
///
/// Forgetting removes the **registration** and nothing else. The folder, its worktrees, its
/// branches and its history are untouched — Nysia does not own them (§3.1), and a verb that
/// deleted a person's repository because they tidied their sidebar would be the last thing
/// they let it do.
///
/// An id nothing is registered under is [`ErrorCode::UnknownProject`] rather than a quiet
/// success, mirroring `session_close` on a stale handle. The alternative — treating it as
/// idempotent — means a typed id exits zero and the person is left believing they forgot
/// something they did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectForget {
    /// Which project.
    pub id: ProjectId,
}

/// What a registration answers with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectRegistered {
    /// The project, exactly as `project_list` would report it.
    pub project: Project,
    /// Whether this path was already a project before the request arrived.
    ///
    /// Not the same question as [`MutationReceipt::replayed`](crate::MutationReceipt),
    /// which the two would otherwise look like two answers to. `replayed` is about *this
    /// request*: the daemon found a receipt for the id being retried and handed back the
    /// stored answer. This is about *the path*: someone registered it earlier, possibly
    /// days ago, from another client. A first attempt can carry
    /// `alreadyRegistered: true` with no receipt at all, and a replay of a create carries
    /// `replayed: true` with `alreadyRegistered: false`.
    ///
    /// The dialog needs it to say "that project is already in your sidebar" instead of
    /// pretending to have added it.
    pub already_registered: bool,
}

/// How many repositories a [`RegisterRefusal::ManyRepositories`] names.
///
/// A `Projekty/` directory with forty repositories in it is the case this refusal was
/// written for, and forty names in an error is an error nobody reads. The refusal states
/// the true total either way — see [`RegisterRefusal::into_envelope`].
pub const MAX_LISTED_REPOSITORIES: usize = 5;

/// What the daemon found instead of a repository, in a form the caller can act on.
///
/// §3.2 lists four things a path can turn out to be and is emphatic that the daemon must
/// say which: "could not register" is not an answer a user can act on. One of the four —
/// a git repository — is the success case and answers with a [`ProjectRegistered`]. These
/// are the other three, each with its own [`ErrorCode`] so a client can branch on it
/// without reading prose.
///
/// # No envelope this builds names a path
///
/// Every message and every step below is free of the caller's path, and the two variants
/// that could carry one have no field to carry it in. That is not tidiness: an error
/// envelope is the thing a daemon is most likely to log verbatim, and a repository path
/// names a person's disk (traps register #13/#14). v0.2 confined what reaches a log file
/// and registration must not widen it, so the confinement is in the type rather than in a
/// rule the daemon has to follow.
///
/// [`ManyRepositories`](Self::ManyRepositories) is the one that names anything at all, and
/// [`into_envelope`](Self::into_envelope) reduces each name it was given to its **final
/// path component** before writing it down. A caller cannot widen it by passing absolute
/// paths — the whole answer's value is the names of the repositories to pick between, and a
/// repository's own folder name is already what the sidebar would show as `Project::name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterRefusal {
    /// The folder is there and readable, and git says it is not a repository.
    NotARepository,
    /// The folder is not a repository, but folders inside it are.
    ///
    /// Nysia registers **one repository at a time and registers none of them**, which is
    /// §3.2's "ask" rather than its "register each". Registering each would map one path to
    /// many projects, and the identity rule this milestone rests on — one canonical path,
    /// one id — has no way to spell that. Refusing and naming what it found leaves the
    /// choice with the person who knows which repository they meant, at the cost of one
    /// more click.
    ManyRepositories {
        /// Every repository found, so the total in the refusal is a fact about this list.
        ///
        /// Names relative to the folder that was offered. Reduced to their final component
        /// on the way into the envelope, so a caller that passes absolute paths leaks
        /// nothing.
        found: Vec<String>,
    },
    /// The path does not exist, or could not be read.
    Unreadable,
}

impl RegisterRefusal {
    /// The code a client branches on.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotARepository => ErrorCode::NotARepository,
            Self::ManyRepositories { .. } => ErrorCode::ManyRepositories,
            Self::Unreadable => ErrorCode::PathUnreadable,
        }
    }

    /// The envelope this refusal answers with.
    ///
    /// None of the three is retryable. Every one of them is stable until somebody changes
    /// something — picks a different folder, runs `git init`, plugs the drive back in — so
    /// a client that retried the identical request would get the identical answer, and
    /// `retryable: true` is an invitation to a loop.
    #[must_use]
    pub fn into_envelope(self) -> ErrorEnvelope {
        let code = self.code();
        match self {
            Self::NotARepository => ErrorEnvelope::new(
                code,
                "that folder is not a git repository",
                steps(
                    "Choose the folder that has the `.git` in it — usually one level down \
                     from where you are looking.",
                    &["If it should be a repository, run `git init` in it and register it again."],
                ),
            ),
            Self::ManyRepositories { found } => {
                let total = found.len();
                let names: Vec<String> = found
                    .iter()
                    .take(MAX_LISTED_REPOSITORIES)
                    // The confinement, applied where the name is written rather than where
                    // it was collected.
                    .filter_map(|name| last_component(name))
                    .map(str::to_owned)
                    .collect();
                let listed = names.join(", ");
                let message = if total == 1 {
                    "that folder is not a git repository, but one of the folders in it is"
                        .to_owned()
                } else {
                    format!(
                        "that folder is not a git repository, but {total} of the folders in it are"
                    )
                };
                let pick = if total > names.len() {
                    format!(
                        "Pick one of these and register that folder instead: {listed} — and {} \
                         more inside it.",
                        total - names.len()
                    )
                } else {
                    format!("Pick one of these and register that folder instead: {listed}.")
                };
                ErrorEnvelope::new(
                    code,
                    message,
                    steps(
                        "Nysia registers one repository at a time, so it registered none of them.",
                        &[&pick],
                    ),
                )
            }
            Self::Unreadable => ErrorEnvelope::new(
                code,
                "that path does not exist, or could not be read",
                steps(
                    "Check the path is spelled the way it is on disk and that you can open it.",
                    &[
                        "Nysia never creates the folder for you: register one that is already \
                         there.",
                    ],
                ),
            ),
        }
    }
}

/// What a daemon that does not serve the project verbs yet answers with.
///
/// **Scaffolding, and v0.3 wave C1 replaces it.** The verbs are on the wire from wave A so
/// that the store and the daemon are built against one definition of them; until C1 serves
/// them for real, a daemon that is asked has to say something, and `unsupported` is exactly
/// the code for "a verb this daemon does not serve". The alternative — leaving the variants
/// out until something can answer them — would leave the wire tags undefined for the two
/// waves that have to agree on them.
#[must_use]
pub fn unsupported_envelope() -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::Unsupported,
        "this daemon does not serve the project verbs yet",
        steps(
            "The daemon answering is older than the client asking, or is a build from before \
             v0.3 wave C.",
            &["Compare the two: `nysia --version` reports the client, and the daemon logs its own at startup."],
        ),
    )
    .with_next_command_args(["nysia", "--version"])
}

/// The last segment of `name`, cut at `/` and `\` on **every** platform.
///
/// Not [`Path::file_name`], and the difference is a leak. `Path` splits on the separators
/// of the platform it is compiled for, so on Unix a backslash is an ordinary character:
/// `Path::new(r"C:\Users\someone\Projekty\nysia").file_name()` hands back the whole string,
/// and a Windows-shaped path offered to a daemon running on Unix would reach the envelope
/// intact. The macOS CI leg caught exactly that, on the test written to prove the
/// confinement holds.
///
/// So the rule is the conservative one rather than the faithful one: a confinement must not
/// depend on which platform it is running on any more than it depends on the caller being
/// careful (CLAUDE.md §6). The cost is that a Unix folder genuinely named `my\dir` is
/// written down as `dir` — a worse error message, which is the right side to be wrong on.
///
/// `None` when nothing is left, so a name that was only separators is dropped rather than
/// listed as an empty string.
fn last_component(name: &str) -> Option<&str> {
    let trimmed = name.trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    (!last.is_empty()).then_some(last)
}

/// Build a [`NextSteps`] from literals this module owns.
///
/// [`NextSteps::new`] is fallible because a blank first step is unrepresentable on the wire,
/// and every first step passed here is a non-blank literal in this file — so the error
/// branch cannot be taken. Spelled out rather than `expect`ed: `expect` is denied on a crate
/// boundary (CLAUDE.md §6), and `unreachable!` records *why* it cannot happen instead of
/// asserting that it does not.
fn steps(first: &str, rest: &[&str]) -> NextSteps {
    let mut steps = match NextSteps::new(first) {
        Ok(steps) => steps,
        Err(_) => unreachable!("a non-blank first step is the only thing NextSteps::new asks"),
    };
    for step in rest {
        steps = steps.and(*step);
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id of `text` under one platform's rules, whichever platform is running.
    ///
    /// [`ProjectId::from_canonical_path`] reads `cfg!(windows)`, so it can only ever
    /// exercise one rule set per leg. This is the same two lines with the switch in hand,
    /// which is what lets the Windows rules be checked on a macOS runner and the other way
    /// round.
    fn id_for(text: &str, windows: bool) -> String {
        format!(
            "{PROJECT_ID_PREFIX}{:032x}",
            digest(normalise(text, windows).as_bytes())
        )
    }

    #[test]
    fn the_digest_has_not_moved() {
        // Computed independently of this implementation, from FNV-1a's published constants.
        // The point is not that the numbers are pretty: every project id on every machine
        // is this function's output, so a change here silently re-keys all of them and the
        // sidebar this type exists to keep still reorders itself once.
        assert_eq!(
            id_for("/src/nysia", false),
            "proj_9a8b4bcdaa346c4da0fe52b7dd15df9f"
        );
        assert_eq!(
            id_for(r"C:\src\nysia", true),
            "proj_e585bda3e0332edf092c36227fcd30b2"
        );
        // The empty digest is the offset basis untouched, which pins the basis on its own —
        // a typo'd constant would otherwise only show up mixed with the multiply.
        assert_eq!(
            format!("{:032x}", digest(b"")),
            "6c62272e07bb014262b821756295c58d"
        );
    }

    #[test]
    fn the_windows_rules_fold_what_windows_calls_one_folder() {
        // The case the plan calls out by name: `C:\Users\…` and `c:\users\…` are the same
        // folder and not the same string. Every spelling below is the same folder on
        // Windows, and registering through any of them has to be one project.
        let canonical = id_for(r"C:\src\nysia", true);
        for same in [
            r"c:\src\nysia",
            r"C:\SRC\Nysia",
            r"\\?\C:\src\nysia",
            "C:/src/nysia",
            r"C:\src\nysia\",
            r"C:\src\nysia\\",
        ] {
            assert_eq!(
                id_for(same, true),
                canonical,
                "{same} should be one project"
            );
        }

        // The verbatim UNC form becomes the spelling everything else writes, so a share
        // registered through either is one project.
        assert_eq!(
            id_for(r"\\?\UNC\build\src\nysia", true),
            id_for(r"\\build\src\nysia", true)
        );

        // And a different folder is still a different project: a fold that swallowed
        // everything would pass every assertion above.
        assert_ne!(id_for(r"C:\src\nysia", true), id_for(r"C:\src\orca", true));
    }

    #[test]
    fn the_unix_rules_fold_only_what_unix_calls_one_folder() {
        let canonical = id_for("/src/nysia", false);
        assert_eq!(id_for("/src/nysia/", false), canonical);
        assert_eq!(id_for("/src/nysia///", false), canonical);

        // Case is *not* folded. A case-sensitive volume is supported on Linux and macOS, so
        // these are two folders and folding them would merge two real projects into one.
        assert_ne!(id_for("/src/Nysia", false), canonical);
        // A backslash is an ordinary character in a Unix filename, so it is not a separator
        // and not translated.
        assert_ne!(id_for(r"/src\nysia", false), canonical);
    }

    #[test]
    fn an_id_has_exactly_one_spelling() {
        let id = ProjectId::from_canonical_path(Path::new("/src/nysia")).unwrap();
        assert!(id.as_str().starts_with(PROJECT_ID_PREFIX));
        assert_eq!(
            id.as_str().len(),
            PROJECT_ID_PREFIX.len() + PROJECT_ID_DIGITS
        );
        assert_eq!(id.to_string().parse::<ProjectId>().unwrap(), id);

        for bad in [
            // No prefix.
            "9a8b4bcdaa346c4da0fe52b7dd15df9f",
            // Another type's prefix.
            "sess_9a8b4bcdaa346c4da0fe52b7dd15df9f",
            // Too short, and too long.
            "proj_9a8b4bcdaa346c4da0fe52b7dd15df9",
            "proj_9a8b4bcdaa346c4da0fe52b7dd15df9f0",
            // Uppercase hex: the same digest, a second spelling. Refused for the reason the
            // braced uuid is — string equality has to mean what it says.
            "proj_9A8B4BCDAA346C4DA0FE52B7DD15DF9F",
            // Not hex at all.
            "proj_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "proj_",
        ] {
            assert!(
                bad.parse::<ProjectId>().is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(serde_json::from_str::<ProjectId>("\"proj_nope\"").is_err());
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{id}\""),
            "an id is a bare string on the wire"
        );
    }

    #[test]
    fn a_project_carries_its_worktrees_in_the_shape_the_window_renders() {
        // The field spellings `apps/web/src/store/types.ts` already uses. Wave B replaces
        // those interfaces with the generated ones, and a rename here is a rename there.
        let project = Project {
            id: ProjectId::from_canonical_path(Path::new("/src/nysia")).unwrap(),
            name: "nysia".to_owned(),
            group: Project::DEFAULT_GROUP.to_owned(),
            worktrees: vec![Worktree {
                branch: "main".to_owned(),
                is_primary: true,
                sessions: Vec::new(),
            }],
        };
        let json = serde_json::to_value(&project).unwrap();
        assert_eq!(json["group"], "Dev");
        assert_eq!(json["worktrees"][0]["isPrimary"], true);
        assert_eq!(json["worktrees"][0]["branch"], "main");
        assert!(json["worktrees"][0]["sessions"].is_array());
        assert_eq!(
            json["id"],
            serde_json::Value::String(project.id.to_string()),
            "the id is a bare string, not an object"
        );
        assert_eq!(serde_json::from_value::<Project>(json).unwrap(), project);
    }

    #[test]
    fn registering_says_whether_the_path_was_already_a_project() {
        let registered = ProjectRegistered {
            project: Project {
                id: ProjectId::from_canonical_path(Path::new("/src/nysia")).unwrap(),
                name: "nysia".to_owned(),
                group: Project::DEFAULT_GROUP.to_owned(),
                worktrees: Vec::new(),
            },
            already_registered: true,
        };
        let json = serde_json::to_value(&registered).unwrap();
        assert_eq!(json["alreadyRegistered"], true);
        assert_eq!(
            serde_json::from_value::<ProjectRegistered>(json).unwrap(),
            registered
        );
    }

    #[test]
    fn registering_the_same_folder_twice_is_one_id() {
        // §3.2's idempotency, as a property of the id rather than a check somewhere. The
        // spellings differ only in the ways the platform says do not matter.
        let once = ProjectId::from_canonical_path(Path::new("/src/nysia")).unwrap();
        let again = ProjectId::from_canonical_path(Path::new("/src/nysia/")).unwrap();
        assert_eq!(once, again);
    }

    #[test]
    fn each_of_section_3_2s_cases_is_tellable_apart_on_the_wire() {
        // "Could not register" is not an answer a user can act on. Each case has its own
        // code, so a dialog can branch without reading prose.
        let codes = [
            RegisterRefusal::NotARepository.code(),
            RegisterRefusal::ManyRepositories { found: Vec::new() }.code(),
            RegisterRefusal::Unreadable.code(),
        ];
        assert_eq!(
            codes,
            [
                ErrorCode::NotARepository,
                ErrorCode::ManyRepositories,
                ErrorCode::PathUnreadable
            ]
        );
        // Distinct from each other, and none of them is the catch-all.
        for code in &codes {
            assert_ne!(code, &ErrorCode::Internal);
            assert_ne!(code, &ErrorCode::InvalidRequest);
        }

        for refusal in [
            RegisterRefusal::NotARepository,
            RegisterRefusal::ManyRepositories {
                found: vec!["nysia".to_owned()],
            },
            RegisterRefusal::Unreadable,
        ] {
            let code = refusal.code();
            let envelope = refusal.into_envelope();
            assert_eq!(envelope.code(), &code);
            assert!(
                !envelope.is_retryable(),
                "{code} must not invite a retry loop"
            );
            assert!(
                envelope.next_steps().len() >= 2,
                "{code} answers with too little"
            );
            assert!(
                envelope.next_command_args().is_none(),
                "{code} would have to name a path to carry an argv"
            );
        }
    }

    #[test]
    fn a_refusal_names_no_path() {
        // The guarantee the module doc makes, as a gate. An error envelope is the thing a
        // daemon is most likely to log verbatim (traps register #13/#14), and these two
        // variants have no field a path could arrive in — so the check is that nothing
        // path-shaped is written *here* either.
        for refusal in [RegisterRefusal::NotARepository, RegisterRefusal::Unreadable] {
            let envelope = refusal.into_envelope();
            let written = format!("{}|{}", envelope.message(), envelope.next_steps().join("|"));
            assert!(
                !written.contains('/') && !written.contains('\\'),
                "a refusal wrote something path-shaped: {written}"
            );
        }
    }

    #[test]
    fn a_folder_of_repositories_is_refused_and_says_what_it_found() {
        let refusal = RegisterRefusal::ManyRepositories {
            found: vec!["nysia".to_owned(), "orca".to_owned()],
        };
        let envelope = refusal.into_envelope();
        assert_eq!(envelope.code(), &ErrorCode::ManyRepositories);
        assert!(envelope.message().contains("2 of the folders"));
        let steps = envelope.next_steps().join(" ");
        assert!(steps.contains("registered none of them"), "{steps}");
        assert!(steps.contains("nysia, orca"), "{steps}");

        // One repository inside a folder that is not one reads as a sentence rather than as
        // "1 of the folders".
        let one = RegisterRefusal::ManyRepositories {
            found: vec!["nysia".to_owned()],
        }
        .into_envelope();
        assert!(
            one.message().contains("one of the folders in it is"),
            "{}",
            one.message()
        );
    }

    #[test]
    fn the_total_a_refusal_states_is_a_fact_about_the_list_beneath_it() {
        // A stated total is a fact about the list beneath it (v0.3 plan §6). The names are
        // capped and the total is not, so the two can drift, so a test holds them.
        let found: Vec<String> = (0..MAX_LISTED_REPOSITORIES + 3)
            .map(|n| format!("repo-{n}"))
            .collect();
        let total = found.len();
        let envelope = RegisterRefusal::ManyRepositories { found }.into_envelope();

        assert!(
            envelope
                .message()
                .contains(&format!("{total} of the folders")),
            "{}",
            envelope.message()
        );
        let pick = envelope.next_steps().join(" ");
        for n in 0..MAX_LISTED_REPOSITORIES {
            assert!(pick.contains(&format!("repo-{n}")), "{pick}");
        }
        assert!(
            !pick.contains(&format!("repo-{MAX_LISTED_REPOSITORIES}")),
            "the cap does not hold: {pick}"
        );
        assert!(
            pick.contains(&format!("and {} more", total - MAX_LISTED_REPOSITORIES)),
            "{pick}"
        );

        // Exactly at the cap there is no remainder to state, so the sentence does not claim
        // one — an "and 0 more" would be a total that is a fact about nothing.
        let exact: Vec<String> = (0..MAX_LISTED_REPOSITORIES)
            .map(|n| format!("repo-{n}"))
            .collect();
        let envelope = RegisterRefusal::ManyRepositories { found: exact }.into_envelope();
        assert!(!envelope.next_steps().join(" ").contains("more inside it"));
    }

    #[test]
    fn a_name_that_arrives_as_a_path_reaches_the_envelope_as_a_name() {
        // The confinement is in the constructor rather than in a rule the daemon follows,
        // because a security default an ordinary caller can undo is not a default
        // (CLAUDE.md §6). A daemon that collected absolute paths and passed them straight
        // through leaks nothing.
        //
        // **Both spellings on both platforms**, which is what this test is for. It was
        // written with `Path::file_name`, which splits on the separators of the platform it
        // was compiled for — so the Windows path below survived intact on the macOS leg and
        // the confinement held on exactly one of the two runners. See `last_component`.
        let envelope = RegisterRefusal::ManyRepositories {
            found: vec![
                r"C:\Users\someone\Projekty\nysia".to_owned(),
                "/home/someone/src/orca".to_owned(),
                // A trailing separator leaves nothing after the last one, and a name that
                // is only separators leaves nothing at all: neither may reach the envelope
                // as an empty entry in the list.
                "/home/someone/src/valve/".to_owned(),
                "//".to_owned(),
            ],
        }
        .into_envelope();
        let written = format!("{}|{}", envelope.message(), envelope.next_steps().join("|"));
        assert!(written.contains("nysia"), "{written}");
        assert!(written.contains("orca"), "{written}");
        assert!(written.contains("valve"), "{written}");
        assert!(!written.contains("someone"), "{written}");
        assert!(
            !written.contains('/') && !written.contains('\\'),
            "{written}"
        );
        // The list is three names, not four: the entry that was only separators is dropped
        // rather than written down as nothing. Both numbers stay facts about the list —
        // four were found, three are named, and the remainder is stated as one more, which
        // is what "found and not named here" means whether the reason is the cap or a name
        // that reduced to nothing. Under the cap and still carrying a remainder is the case
        // that would otherwise be spelled as an exact list and quietly be short one.
        assert!(
            written.contains("nysia, orca, valve — and 1 more inside it."),
            "{written}"
        );
        assert!(written.contains("4 of the folders"), "{written}");
    }

    #[test]
    fn the_unserved_answer_says_it_is_scaffolding() {
        // Wave C1 replaces it. Until then this is what a project verb meets, and a reader
        // of a red acceptance test needs it to say so rather than to look like a decision.
        let envelope = unsupported_envelope();
        assert_eq!(envelope.code(), &ErrorCode::Unsupported);
        assert!(!envelope.is_retryable());
        assert_eq!(
            envelope.next_command_args(),
            Some(["nysia".to_owned(), "--version".to_owned()].as_slice())
        );
        // The strings a caller reads verbatim. A wrapped source line that kept its
        // indentation would reach them as a run of spaces.
        for text in std::iter::once(envelope.message())
            .chain(envelope.next_steps().iter().map(String::as_str))
        {
            assert!(
                !text.contains("  "),
                "a run of spaces reached a caller: {text:?}"
            );
        }
    }

    #[test]
    fn every_refusal_reads_as_one_line() {
        // The same check for the three that matter most: these are what the register dialog
        // shows, and source wrapping is invisible until someone reads the rendered string.
        for refusal in [
            RegisterRefusal::NotARepository,
            RegisterRefusal::ManyRepositories {
                found: vec!["nysia".to_owned(), "orca".to_owned(), "valve".to_owned()],
            },
            RegisterRefusal::Unreadable,
        ] {
            let envelope = refusal.into_envelope();
            for text in std::iter::once(envelope.message())
                .chain(envelope.next_steps().iter().map(String::as_str))
            {
                assert!(
                    !text.contains("  "),
                    "a run of spaces reached a caller: {text:?}"
                );
                assert!(!text.contains('\n'), "a newline reached a caller: {text:?}");
            }
        }
    }
}
