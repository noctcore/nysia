//! The spawn itself: argument construction, confinement, timeouts, and the environment.
//!
//! This is the part of D-15 that is load-bearing. Everything else in this module is a
//! question about a folder; this is the one place the answer is fetched, and the four things
//! the module documentation names are all decided here.
//!
//! # Argument construction
//!
//! A path is not an argument. `git -C <path>` and `git log -- <path>` both put a
//! caller-controlled string on a command line git parses with its own option parser, and a
//! folder called `--upload-pack=calc` is a folder a person can create. None of the questions
//! this module answers passes a path as an argument at all: the working directory is set on
//! the child process, which the option parser never sees.
//!
//! One verb cannot be written that way — `git worktree add <path>` needs its path positional —
//! and that is what `--` is for. [`GitCommand::operand_path`] is the single named entry point
//! that puts a caller's path where git's parser can reach it, and it puts it behind the
//! separator. Measured rather than assumed, with `git worktree add -b h '--upload-pack=calc'`:
//! without the separator git answers ``error: unknown option `upload-pack=calc'`` and exits
//! 129; with it, the worktree is created.
//!
//! # The confinement check on the working directory
//!
//! Expressed as a type. [`GitCommand::run`] takes a [`CanonicalPath`], which can only be
//! built from a path that resolved to an existing directory, so a spawn against something
//! that is not a folder has no spelling.
//!
//! # Timeouts
//!
//! Every invocation carries a deadline. `git` hangs for ordinary reasons — an index lock a
//! crashed process left behind, a network remote, a credential prompt — and under D-1 the
//! daemon it hangs lives for days. On expiry the child's whole tree is killed: on Windows
//! through the Job Object trap 7 names, and on Unix by signalling the process group.
//!
//! # The credential environment
//!
//! Three separate jobs, and the worst case is the quiet one.
//!
//! - **A credential prompt hangs invisibly.** git asks a terminal the daemon does not have,
//!   or pops a helper's window behind everything, and the user sees a spinner.
//!   `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never` turn that into an error, and stdin
//!   is `/dev/null` so there is nothing to read even if something tried.
//! - **`git` config is code execution.** `core.fsmonitor`, `core.hooksPath`, `core.pager`,
//!   `diff.external`, `core.sshCommand` and `core.gitProxy` all name a program git runs, and
//!   all of them can be set in a repository's own `.git/config` — which is a file an agent
//!   Nysia launched can write. Every one is pinned on the command line with `-c`, where it
//!   outranks the config file (architecture §7.5): to nothing where git is free to run no
//!   program, and to git's own default where it is not. [`NEUTRALISED_CONFIG`] has the
//!   difference, and the one key `-c` cannot reach.
//! - **`GIT_*` in this process's environment redirects the answer.** `GIT_DIR` is the sharp
//!   one, and it is not hypothetical: with `GIT_DIR` set, `git rev-parse` in a folder that
//!   is not a repository at all **succeeds** and reports the other repository, so a plain
//!   folder registers as a project pointing at somebody else's git directory. Every override
//!   is removed by name.
//!
//! Removal is by name and never `env_clear`, for the reason `pty::env` gives: clearing the
//! block takes `SSH_AUTH_SOCK`, the Windows credential-manager variables and everything else
//! a credential helper needs, and the failure surfaces much later as an unexplained
//! authentication error. `credential.helper` is deliberately **not** neutralised — helpers
//! are how a user's existing authentication keeps working, and the two settings above are
//! what make them non-interactive.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use super::error::GitError;
use super::path::CanonicalPath;
use super::runner::{EnvPolicy, Finished, RunError, Runner, trim_stderr};

/// How long an invocation gets before it is killed.
///
/// Generous for the local, read-only questions this module asks — `rev-parse` and
/// `worktree list` answer in milliseconds on any repository — and short enough that a user
/// who pointed Nysia at a folder on a disconnected network share gets an error rather than a
/// sidebar that never finishes loading.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// git's exit code for a usage error, as opposed to 128 for an operational failure.
const USAGE_EXIT_CODE: i32 = 129;

/// The config settings pinned on every invocation, each of which names a program git would
/// otherwise run.
///
/// Passed as `-c key=value` ahead of the verb, where they outrank `/etc/gitconfig`, the
/// user's `~/.gitconfig` **and** the repository's own `.git/config` — which is the one that
/// matters, because it is the file an agent working in a checkout can write.
/// `the_neutralisers_stop_programs_the_repository_config_names` proves that precedence by
/// writing each key into a fixture's own `.git/config` first.
///
/// # Empty is not always "no program"
///
/// An empty value is how git spells "run nothing" for a key it is free to skip. For a key git
/// must satisfy, an empty value is how you break the feature instead, and two here are pinned
/// to a program for that reason:
///
/// - **`core.pager`.** An empty pager is not "no pager", so it is pinned to `cat`, and
///   `--no-pager` is passed as well because that is the documented switch.
/// - **`core.sshCommand`, which is the sharp one.** git's `get_ssh_command()` returns the
///   empty string as a *non-NULL* command, so it never falls back to `ssh` and every `ssh://`
///   and `git@host:` remote dies before it reaches the network. Measured, rather than
///   reasoned: `git -c core.sshCommand= ls-remote ssh://127.0.0.1:1/x` answers
///   `error: cannot spawn : No such file or directory` and `fatal: ssh variant 'simple' does
///   not support setting port`, while `core.sshCommand=ssh` — and no `-c` at all — both
///   answer `ssh: connect to host 127.0.0.1 port 1: Connection refused`. So it is pinned to
///   `ssh`, which is git's own default and what an empty value was meant to mean.
///
///   Pinning has a cost worth naming rather than discovering: a user whose `~/.gitconfig`
///   sets `core.sshCommand` to plink, or to `ssh -i <key>`, does not get it here. That is the
///   same stance [`SCRUBBED_VARS`] already takes by removing `GIT_SSH` and `GIT_SSH_COMMAND`
///   — behind this chokepoint the transport is git's own ssh and not one a config file names
///   — and it is the difference between overriding a preference and switching a transport off
///   for everybody.
///
/// # The diff verb has to finish this job
///
/// Diff has two program-running knobs this list does not settle, so the first verb here that
/// produces a diff — wave C's — must pass **`--no-ext-diff --no-textconv`**:
///
/// - **`diff.textconv` is not a git config key**, so an entry for it here would be inert and
///   would make this list look one member more complete than it is. The real key is
///   `diff.<driver>.textconv`, where `<driver>` is named by a `.gitattributes` line, and `-c`
///   cannot wildcard it. Measured against a repository with a diff driver: the textconv
///   program still runs with `-c diff.textconv=` present, and only `--no-textconv` stops it.
/// - **`diff.external` is in the list and is not enough alone.** Empty is not "no external
///   diff": git spawns the empty command and stops with `fatal: external diff died`, exit
///   128. That failure is the *safe* one, and the difference from `core.sshCommand` is worth
///   keeping straight — there, empty silently substitutes a broken transport for a working
///   one; here, empty substitutes a loud error for running whatever a repository's config
///   named. So a diff verb that forgets the flag breaks visibly instead of executing
///   somebody's script. With `--no-ext-diff`, git never consults the key at all, the empty
///   value is never spawned, and a real diff comes back.
///
/// So the claim that every config key naming a program is neutralised by `-c` alone is true
/// of the keys below and not of diff.
///
/// # Which of these are proven by execution
///
/// `the_neutralisers_stop_programs_the_repository_config_names` runs `core.hooksPath`,
/// `core.fsmonitor` and `diff.external` both ways against a fixture whose own `.git/config`
/// names a program, each with a control that fires first. `core.sshCommand` is measured above
/// as a transport rather than in a test, because reaching it needs a remote. `core.pager` is
/// never consulted — `--no-pager` is on every invocation — and `core.askPass` and
/// `core.gitProxy` are held here as list membership only: unverified rather than known good,
/// and behind `GIT_TERMINAL_PROMPT=0`, a null stdin and a deadline, so the worst case each
/// can produce is a bounded failure rather than a hang.
pub const NEUTRALISED_CONFIG: &[(&str, &str)] = &[
    // Runs a filesystem-monitor hook on almost every command.
    ("core.fsmonitor", ""),
    // Relocates the hook directory, so a `post-checkout` can come from anywhere. Empty
    // disables hooks outright, the repository's own `.git/hooks` included.
    ("core.hooksPath", ""),
    // A pager is a program, and one that waits for a keypress is a hang.
    ("core.pager", "cat"),
    // Replaces `diff` wholesale. Empty is not "no external diff" here either — git spawns
    // the empty command and stops — but unlike `core.sshCommand` that failure is the safe
    // one, because the program a repository named does not run. See this list's
    // documentation: wave C's diff verb finishes the job with `--no-ext-diff`.
    ("diff.external", ""),
    // The transport for every `ssh://` and `user@host:` remote. Pinned to git's own default
    // rather than emptied: empty is taken as the command and switches the transport off. See
    // this list's documentation for the measurement.
    ("core.sshCommand", "ssh"),
    // The transport for `git://`.
    ("core.gitProxy", ""),
    // Asks for a password by running a program.
    ("core.askPass", ""),
];

/// The environment variables removed before every invocation.
///
/// Three groups, and the first is the one with a test named after it:
///
/// 1. **Repository overrides.** `GIT_DIR` and its family answer the question this module
///    asks, before the working directory is consulted. With `GIT_DIR` set, a `rev-parse` in
///    an empty folder exits 0 and reports the repository the variable names.
/// 2. **Program overrides.** Every variable whose value git treats as a command to run.
///    These are the environment half of [`NEUTRALISED_CONFIG`]; neutralising the config
///    without them would leave the door open.
/// 3. **Loader overrides.** `LD_PRELOAD` and the Darwin `DYLD_*` pair inject a library into
///    the child before its `main` runs, which is program execution by another route.
pub const SCRUBBED_VARS: &[&str] = &[
    // 1. Which repository git operates on.
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    // Config injected through the environment. Removing `GIT_CONFIG_COUNT` disables every
    // numbered `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` pair without having to guess
    // how many there were.
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    // 2. Programs git runs.
    "GIT_EXEC_PATH",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_EXTERNAL_DIFF",
    "GIT_PROXY_COMMAND",
    // 3. Libraries loaded into the child before it runs.
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
];

/// The variables set on every invocation.
///
/// `GIT_OPTIONAL_LOCKS=0` is the least obvious and earns its place: without it a read-only
/// command such as `status` still takes the index lock to refresh it, so a repository whose
/// lock another process is holding turns a question into a wait.
pub const FORCED_VARS: &[(&str, &str)] = &[
    // No prompt on a terminal the daemon does not have.
    ("GIT_TERMINAL_PROMPT", "0"),
    // Git Credential Manager's own switch for the same thing: no window, fail instead.
    ("GCM_INTERACTIVE", "never"),
    // Never block on a lock for a question.
    ("GIT_OPTIONAL_LOCKS", "0"),
    // Messages in one language, so a log from a user's machine reads the same as a log from
    // CI. Porcelain output does not vary by locale, but stderr does.
    ("LC_ALL", "C"),
];

/// git's environment, as the one policy every git spawn runs under.
///
/// The two lists above, bound together so that [`super::runner`] can apply them without
/// knowing what program they are for. It is git's and only git's: `gh` brings its own, for
/// the reason [`EnvPolicy`] states — `GIT_TERMINAL_PROMPT=0` means nothing to gh, and gh's
/// credentials would not survive this scrub list growing.
pub(crate) const GIT_ENV: EnvPolicy = EnvPolicy {
    scrubbed: SCRUBBED_VARS,
    forced: FORCED_VARS,
};

/// A resolved `git`, and the deadline every invocation through it carries.
///
/// A thin policy layer over [`Runner`]: this type owns git's argument vector, git's
/// environment and git's reading of an exit code, and the runner owns everything about
/// spawning a program safely. See [`super::runner`] for why those are two things.
#[derive(Debug, Clone)]
pub struct Git {
    /// The resolved program, its environment policy, and its deadline.
    runner: Runner,
}

impl Git {
    /// Find `git` on `PATH` and pre-validate it.
    ///
    /// # Errors
    ///
    /// [`GitError::NotInstalled`] when nothing on `PATH` matched, which is a real answer for
    /// a machine that has never had git installed and is better given here than as a failed
    /// spawn at the bottom of a call stack.
    pub fn locate() -> Result<Self, GitError> {
        Self::located_as("git")
    }

    /// Find a program by name, so that a test can ask what happens when git is missing.
    ///
    /// Not public: the only production spelling is `git`, and a caller that could choose the
    /// program would be a second chokepoint.
    pub(crate) fn located_as(program: &str) -> Result<Self, GitError> {
        Ok(Self {
            runner: Runner::locate(program, GIT_ENV, DEFAULT_TIMEOUT)
                .map_err(|source| GitError::NotInstalled { source })?,
        })
    }

    /// The same git with a different deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.runner = self.runner.with_timeout(timeout);
        self
    }

    /// The deadline each invocation through this handle carries.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.runner.timeout()
    }

    /// Run a command in `at` and return its stdout, or the reason it did not.
    ///
    /// # Errors
    ///
    /// See [`GitError`]. A non-zero exit is an error rather than an empty answer, because
    /// every caller in this module is asking a question whose wrong answer is "no".
    pub(crate) fn run(
        &self,
        command: &GitCommand,
        at: &CanonicalPath,
    ) -> Result<Vec<u8>, GitError> {
        let output = self.capture(command, at)?;
        match output.failure(command, at, self.timeout()) {
            Some(err) => Err(err),
            None => Ok(output.stdout),
        }
    }

    /// Run a command and hand back everything about how it ended, without judging it.
    ///
    /// Separate from [`Git::run`] because classification needs a failure's detail: whether a
    /// folder is a repository is decided by the filesystem plus git's stderr, not by mapping
    /// an exit code onto a guess.
    pub(crate) fn capture(
        &self,
        command: &GitCommand,
        at: &CanonicalPath,
    ) -> Result<Finished, GitError> {
        self.runner
            .run(&command.argv(), at)
            .map_err(|err| match err {
                // Only reachable if `git` resolved to a batch shim, which no git installation
                // ships; the refusal is `pty::resolve`'s BatBadBut guard, and letting it
                // through silently would be the opposite of a chokepoint.
                RunError::Argv(source) => GitError::NotInstalled { source },
                RunError::Empty => GitError::Unparsable {
                    args: command.describe(),
                    problem: "an empty argument vector".to_owned(),
                },
                RunError::Spawn(source) => GitError::Spawn {
                    at: at.as_path().to_path_buf(),
                    source,
                },
            })
    }
}

impl Finished {
    /// What this ending amounts to, or `None` when git answered the question.
    ///
    /// The one place an exit code is judged. [`Git::run`] turns the answer straight into its
    /// `Err`, and `inspect`'s probe needs the same judgement before it may treat a failure as
    /// "this folder is not a repository" — a probe that classified on its own would read a
    /// git too old for [`super::REQUIRED_OPTIONS`] as every folder being unreadable.
    pub(crate) fn failure(
        &self,
        command: &GitCommand,
        at: &CanonicalPath,
        timeout: Duration,
    ) -> Option<GitError> {
        let args = command.describe();
        if self.timed_out {
            return Some(GitError::TimedOut {
                args,
                at: at.as_path().to_path_buf(),
                timeout,
            });
        }
        // Before the exit code, because a truncated answer usually exits **zero**: git wrote
        // more than the cap and said nothing was wrong. A short `worktree list` parses
        // cleanly and comes back missing worktrees, which is how one gets registered twice
        // (see `GitError::Unparsable`) — so the cut is the error rather than whatever the
        // remaining bytes happen to parse as.
        if self.truncated {
            return Some(GitError::Unparsable {
                args,
                problem: "more output than this build will read".to_owned(),
            });
        }
        let stderr = trim_stderr(&self.stderr);
        match self.code {
            Some(0) => None,
            Some(USAGE_EXIT_CODE) => Some(GitError::Usage { args, stderr }),
            code => Some(GitError::Failed {
                args,
                at: at.as_path().to_path_buf(),
                status: code.map_or_else(
                    || "killed by a signal".to_owned(),
                    |code| format!("exit code {code}"),
                ),
                stderr,
            }),
        }
    }
}

/// An argument vector, built so that nothing a caller supplies can become an option.
///
/// The `-c` neutralisers and `--no-pager` are prepended here rather than by each call site,
/// so that "every git spawn is hardened" is a property of the type and not of everybody
/// remembering.
#[derive(Debug, Clone)]
pub(crate) struct GitCommand {
    /// The verb and its flags, all of them written in this module.
    args: Vec<OsString>,
    /// Caller-supplied operands, which go after `--`.
    operands: Vec<OsString>,
}

impl GitCommand {
    /// A command from a verb and its flags.
    ///
    /// Every element is a literal written in this crate. A caller-supplied string belongs in
    /// [`GitCommand::pathspec`], where `--` protects it.
    pub(crate) fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            operands: Vec::new(),
        }
    }

    /// Add a path as a command **operand** — an argument git's own option parser will see.
    ///
    /// Named so that the call site is obvious in review and greppable, because this is the
    /// one way past the rule the rest of the module keeps: a path is a working directory, not
    /// an argument. It exists because `git worktree add <path>` needs a positional path, and
    /// wave C creates worktrees.
    ///
    /// The protection is the `--` this places in front of it. Everything after the separator
    /// is an operand however it is spelled, which is precisely the case a folder named
    /// `--upload-pack=calc` creates — see
    /// `a_dash_leading_operand_is_a_path_only_because_of_the_separator` for git's two answers
    /// to the same path with and without it.
    pub(crate) fn operand_path(mut self, path: &Path) -> Self {
        self.operands.push(path.as_os_str().to_owned());
        self
    }

    /// Add a branch name as a command **operand**, for `git worktree add <path> <commit-ish>`.
    ///
    /// Named for what it carries, like [`GitCommand::operand_path`] and for the same reason:
    /// this is a caller's string reaching git's own argument list, and the call site should
    /// say so in review. It exists because `worktree add` takes the branch to check out
    /// *positionally*, after the path — there is no option form of it to hide behind.
    ///
    /// The `--` this places in front is the protection, and it is the whole of it: everything
    /// after the separator is an operand however it is spelled. `crate::worktree::ensure`
    /// refuses a leading dash before it ever gets here anyway, because the `-b` form of the
    /// same name is a bare argument where no separator can help.
    pub(crate) fn operand_branch(mut self, branch: &str) -> Self {
        self.operands.push(OsString::from(branch));
        self
    }

    /// The full vector: neutralisers, then the verb and its flags, then `--` and operands.
    fn argv(&self) -> Vec<OsString> {
        let mut argv = Vec::with_capacity(NEUTRALISED_CONFIG.len() * 2 + self.args.len() + 3);
        for (key, value) in NEUTRALISED_CONFIG {
            argv.push(OsString::from("-c"));
            argv.push(OsString::from(format!("{key}={value}")));
        }
        argv.push(OsString::from("--no-pager"));
        argv.extend(self.args.iter().cloned());
        if !self.operands.is_empty() {
            argv.push(OsString::from("--"));
            argv.extend(self.operands.iter().cloned());
        }
        argv
    }

    /// The verb and its flags, for an error message.
    ///
    /// The neutralisers are left out deliberately: they are the same on every invocation and
    /// would bury the part of the message that says what was being asked.
    fn describe(&self) -> String {
        let mut parts: Vec<String> = self
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if !self.operands.is_empty() {
            parts.push("--".to_owned());
            parts.extend(
                self.operands
                    .iter()
                    .map(|operand| operand.to_string_lossy().into_owned()),
            );
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::{Command, Stdio};

    use super::*;
    use crate::git::runner::apply_environment;
    use crate::git::testing::{Scratch, git_or_skip};

    #[test]
    fn a_missing_git_is_reported_before_anything_is_spawned() {
        // "What does it do when git is not installed at all?" — resolved up front, so the
        // answer names git rather than surfacing as a `NotFound` from a spawn deep in a call
        // stack, which on Windows has several unrelated causes (traps register #8).
        let err = Git::located_as("nysia-no-such-git-exists").unwrap_err();
        assert!(matches!(err, GitError::NotInstalled { .. }), "{err:?}");
    }

    #[test]
    fn the_neutralisers_precede_the_verb_and_operands_follow_a_double_dash() {
        let argv = GitCommand::new(["worktree", "add", "-b", "hostile"])
            .operand_path(Path::new("--upload-pack=calc"))
            .argv();
        let text: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let verb = text
            .iter()
            .position(|arg| arg == "worktree")
            .expect("the verb is in the vector");
        let separator = text
            .iter()
            .position(|arg| arg == "--")
            .expect("an operand must be preceded by --");
        let operand = text
            .iter()
            .position(|arg| arg == "--upload-pack=calc")
            .expect("the operand is in the vector");

        for (key, value) in NEUTRALISED_CONFIG {
            let setting = format!("{key}={value}");
            let at = text
                .iter()
                .position(|arg| arg == &setting)
                .unwrap_or_else(|| panic!("{setting} is not neutralised"));
            assert_eq!(text[at - 1], "-c", "{setting} must follow a -c");
            assert!(
                at < verb,
                "{setting} must precede the verb to outrank config"
            );
        }
        assert!(separator > verb, "-- belongs after the verb, not before it");
        assert_eq!(operand, separator + 1, "the operand must sit behind --");
    }

    #[test]
    fn a_dash_leading_operand_is_a_path_only_because_of_the_separator() {
        // The attack the task names, and git's two answers to the same string. Without `--`,
        // `--upload-pack=calc` is an option: git prints ``unknown option `upload-pack=calc'``
        // and exits 129. With it, the worktree is created. Both halves are asserted, because
        // the first alone would pass if `--` did nothing and git happened to ignore the
        // argument.
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("operand");
        let repo = scratch.repository("project");
        let at = CanonicalPath::of(&repo).expect("the repository is a folder");
        // Bare, not absolute: an absolute path cannot begin with a dash, so spelling it
        // relative to the working directory is what puts the test in the case it is about.
        let hostile = Path::new("--upload-pack=calc");

        let without = GitCommand::new(["worktree", "add", "-b", "unsafe", "--upload-pack=calc"]);
        let err = git
            .run(&without, &at)
            .expect_err("git must refuse a dash-leading operand that is not behind --");
        match err {
            GitError::Usage { stderr, .. } => assert!(
                stderr.contains("upload-pack"),
                "git must be rejecting this exact string as an option: {stderr}"
            ),
            other => panic!("expected git's usage error, got {other:?}"),
        }

        let with = GitCommand::new(["worktree", "add", "-b", "safe"]).operand_path(hostile);
        git.run(&with, &at)
            .expect("behind -- the same string is a path");
        assert!(
            repo.join("--upload-pack=calc").join(".git").is_file(),
            "the worktree was not created, so the success above was not this path"
        );
    }

    #[test]
    fn every_program_naming_config_setting_is_neutralised() {
        // The list architecture §7.5 names, held as a list rather than as prose. Each of
        // these is a config key whose value git executes, and each can be set in a
        // repository's own .git/config.
        for key in [
            "core.fsmonitor",
            "core.hooksPath",
            "core.pager",
            "diff.external",
            "core.sshCommand",
            "core.gitProxy",
        ] {
            assert!(
                NEUTRALISED_CONFIG.iter().any(|(name, _)| *name == key),
                "{key} names a program git runs and is not neutralised"
            );
        }
        // Pinned, not emptied. `-c core.sshCommand=` is not "no ssh command": git takes the
        // empty string as the command and never falls back, so every ssh:// remote dies
        // before it connects. This shipped empty and is the reason the list's documentation
        // now carries a measurement.
        let pinned = |key: &str| {
            NEUTRALISED_CONFIG
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| *value)
        };
        assert_eq!(
            pinned("core.sshCommand"),
            Some("ssh"),
            "an empty core.sshCommand switches the ssh transport off rather than confining it"
        );
        assert_eq!(
            pinned("core.pager"),
            Some("cat"),
            "an empty pager is not the same as no pager"
        );

        // Deliberately absent, each for its own reason.
        assert!(
            !NEUTRALISED_CONFIG
                .iter()
                .any(|(name, _)| *name == "credential.helper"),
            "neutralising credential.helper breaks authentication rather than confining it"
        );
        assert!(
            !NEUTRALISED_CONFIG
                .iter()
                .any(|(name, _)| *name == "diff.textconv"),
            "diff.textconv is not a git config key, so an entry for it would be inert and \
             would make this list look one member more complete than it is; the real key is \
             diff.<driver>.textconv, which -c cannot wildcard, and the mitigation is \
             --no-textconv on wave C's diff verb"
        );
    }

    #[test]
    fn the_neutralisers_stop_programs_the_repository_config_names() {
        // The list's claim, run rather than asserted about — and run against the threat it
        // exists for. Every key goes into the repository's **own** `.git/config`, which is the
        // file an agent working in a checkout can write, so what this shows is `-c` outranking
        // that file rather than merely beating a default.
        //
        // Each key gets a control that fires first. Without one, a neutraliser that did
        // nothing would still pass: a verb that never reaches the program looks exactly like
        // one that was stopped.
        //
        // The PR that added this module claimed the neutralisers could not be proven
        // behaviourally because nothing here fires a hook. That was wrong — the fixtures run
        // `git commit` and `git worktree add`, which fire `pre-commit` and `post-checkout`
        // today — and this is the proof that was available all along.
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("neutralisers");
        let repo = scratch.repository("project");
        let at = CanonicalPath::of(&repo).expect("the fixture is a folder");
        // One program and one marker per key. Sharing them would let a neutraliser that
        // stopped working be reported against a different key entirely.
        let (hook_program, hook_fired) = scratch.marker_program("hook");
        let (fsmonitor_program, fsmonitor_fired) = scratch.marker_program("fsmonitor");
        let (diff_program, diff_fired) = scratch.marker_program("differ");

        let hooks = scratch.folder("hostile-hooks");
        let hook = hooks.join("post-checkout");
        std::fs::copy(&hook_program, &hook)
            .expect("a hook in a directory of the attacker's choosing");
        crate::git::testing::make_executable(&hook);

        scratch.poison_config(
            &repo,
            &[
                ("core", "hooksPath", hooks.as_path()),
                ("core", "fsmonitor", fsmonitor_program.as_path()),
                ("diff", "external", diff_program.as_path()),
            ],
        );
        // `diff` needs something to diff.
        std::fs::write(repo.join("a.txt"), "changed\n").expect("a change to diff");

        // `core.hooksPath`, through `git worktree add` — the verb `Scratch::add_worktree`
        // already runs, so the hook fires on the fixtures exactly as they stand.
        let loose = scratch.root().join("unguarded-checkout");
        assert!(
            without_neutralisers(
                &git,
                &at,
                &[
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    OsStr::new("-b"),
                    OsStr::new("loose"),
                    loose.as_os_str(),
                ],
                &hook_fired
            ),
            "the control did not fire: plain `git worktree add` must run the relocated \
             post-checkout hook, or the next assertion proves nothing"
        );
        assert!(
            !with_neutralisers(
                &git,
                &at,
                &GitCommand::new(["worktree", "add", "-b", "guarded"])
                    .operand_path(&scratch.root().join("guarded-checkout")),
                &hook_fired
            ),
            "core.hooksPath did not stop a hook the repository's own config relocated"
        );

        // `core.fsmonitor`, which git consults on almost every command that reads the index.
        assert!(
            without_neutralisers(
                &git,
                &at,
                &[OsStr::new("status"), OsStr::new("--porcelain")],
                &fsmonitor_fired
            ),
            "the control did not fire: plain `git status` must run the fsmonitor program"
        );
        assert!(
            !with_neutralisers(
                &git,
                &at,
                &GitCommand::new(["status", "--porcelain"]),
                &fsmonitor_fired
            ),
            "core.fsmonitor did not stop a program the repository's own config named"
        );

        // `diff.external`, which replaces diff wholesale, and is the one key the `-c` does
        // not finish on its own.
        assert!(
            without_neutralisers(&git, &at, &[OsStr::new("diff")], &diff_fired),
            "the control did not fire: plain `git diff` must run the external differ"
        );
        // How wave C's diff verb has to spell it. `--no-ext-diff` stops git consulting the
        // key at all, so the empty value is never spawned and a real diff comes back.
        assert!(
            !with_neutralisers(
                &git,
                &at,
                &GitCommand::new(["diff", "--no-ext-diff"]),
                &diff_fired
            ),
            "diff.external did not stop a program the repository's own config named"
        );
        // And when that flag is forgotten, the failure is the safe one. This is the assertion
        // that says why the entry stays in the list despite not being sufficient: without it,
        // a forgotten flag runs the attacker's program instead of refusing to diff.
        std::fs::remove_file(&diff_fired).ok();
        let forgotten = git
            .run(&GitCommand::new(["diff"]), &at)
            .expect_err("an empty diff.external cannot be spawned, so git must stop");
        assert!(
            matches!(forgotten, GitError::Failed { .. }),
            "a diff verb that forgot --no-ext-diff must fail rather than succeed: {forgotten:?}"
        );
        assert!(
            !diff_fired.exists(),
            "git ran the program the repository's config named instead of the empty one"
        );
    }

    /// Run git in `at` the way plain git would, with none of the chokepoint's `-c` settings,
    /// and say whether the marker program ran.
    ///
    /// This is the control arm. Its exit status is deliberately ignored: what it has to show
    /// is that the verb reaches the program at all, and a `status` whose fsmonitor returned
    /// nonsense has still run it.
    fn without_neutralisers(
        git: &Git,
        at: &CanonicalPath,
        args: &[&OsStr],
        marker: &std::path::Path,
    ) -> bool {
        std::fs::remove_file(marker).ok();
        let argv = git
            .runner
            .resolved_argv(args.iter().copied())
            .expect("the fixture arguments are safe for any resolved program");
        let (program, rest) = argv.split_first().expect("argv holds the program");
        Command::new(program)
            .args(rest)
            .current_dir(at.as_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git ran");
        marker.exists()
    }

    /// Run the same verb through the chokepoint and say whether the marker program ran.
    ///
    /// The run must succeed. A guarded run that failed for an unrelated reason would leave the
    /// marker untouched too, and would read as a neutraliser working.
    fn with_neutralisers(
        git: &Git,
        at: &CanonicalPath,
        command: &GitCommand,
        marker: &std::path::Path,
    ) -> bool {
        std::fs::remove_file(marker).ok();
        git.run(command, at).unwrap_or_else(|err| {
            panic!("the guarded run must succeed, or its silence means nothing: {err}")
        });
        marker.exists()
    }

    #[test]
    fn git_dir_is_removed_from_the_child_environment() {
        // The headline scrub, and the one with a demonstrated consequence: with GIT_DIR set,
        // `git rev-parse --git-common-dir` in a folder that is not a repository exits 0 and
        // prints the repository GIT_DIR names. A plain folder would then register as a
        // project pointing at somebody else's git directory.
        //
        // Asserted on the built `Command` rather than by setting a process-wide variable:
        // `std::env::set_var` is unsafe in edition 2024 for the reason that tests run in
        // parallel threads, and a test that mutates the environment breaks whichever other
        // test is spawning at that moment.
        let mut command = Command::new("git");
        command.env("GIT_DIR", "/somebody/elses/repo/.git");
        apply_environment(&mut command, &GIT_ENV);

        let removed = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("GIT_DIR"));
        assert_eq!(
            removed,
            Some((std::ffi::OsStr::new("GIT_DIR"), None)),
            "GIT_DIR must be removed from the child, not merely left unset here"
        );
    }

    #[test]
    fn the_git_dir_scrub_is_what_stops_a_plain_folder_answering_for_another_repository() {
        // The scrub's consequence, run rather than asserted about — and run both ways, so
        // that the proof trips for the right reason. Without the scrub git exits 0 in a
        // folder that is not a repository and reports the one `GIT_DIR` names; with it, the
        // same folder is not a repository. Delete the `GIT_DIR` entry from `SCRUBBED_VARS`
        // and the second half goes red.
        //
        // The variable is set on the child only. `std::env::set_var` is unsafe in edition
        // 2024 precisely because tests share a process, and mutating the environment here
        // would break whichever other test is spawning at that moment.
        let Some(git) = git_or_skip() else { return };
        let scratch = Scratch::new("gitdir-scrub");
        let repo = scratch.repository("project");
        let plain = scratch.folder("not-a-repository");

        let run = |scrubbed: bool| -> std::process::Output {
            let argv = git
                .runner
                .resolved_argv(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .expect("a plain flag is safe for any resolved program");
            let (program, args) = argv.split_first().expect("argv holds the program");
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(&plain)
                .env("GIT_DIR", repo.join(".git"))
                .stdin(Stdio::null());
            if scrubbed {
                apply_environment(&mut command, &GIT_ENV);
            }
            command.output().expect("git ran")
        };

        let leaked = run(false);
        assert!(
            leaked.status.success() && String::from_utf8_lossy(&leaked.stdout).contains("project"),
            "the trap this scrub exists for did not reproduce, so the assertion below proves              nothing: {leaked:?}"
        );

        let scrubbed = run(true);
        assert!(
            !scrubbed.status.success(),
            "with GIT_DIR scrubbed, a folder that is not a repository must not be one: {}",
            String::from_utf8_lossy(&scrubbed.stdout)
        );
    }

    #[test]
    fn every_scrubbed_variable_is_removed_and_nothing_else_is() {
        let mut command = Command::new("git");
        for name in SCRUBBED_VARS {
            command.env(name, "inherited");
        }
        // The variables a credential helper needs, which is why this is `env_remove` and not
        // `env_clear` — the same reason `pty::env` gives.
        for name in ["SSH_AUTH_SOCK", "HOME", "PATH", "USERPROFILE", "APPDATA"] {
            command.env(name, "keep me");
        }
        apply_environment(&mut command, &GIT_ENV);

        let env: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(std::ffi::OsStr::to_owned)))
            .collect();
        for name in SCRUBBED_VARS {
            let entry = env
                .iter()
                .find(|(key, _)| key == std::ffi::OsStr::new(name))
                .unwrap_or_else(|| panic!("{name} is not in the child environment at all"));
            assert!(entry.1.is_none(), "{name} must be removed");
        }
        for name in ["SSH_AUTH_SOCK", "HOME", "PATH", "USERPROFILE", "APPDATA"] {
            let entry = env
                .iter()
                .find(|(key, _)| key == std::ffi::OsStr::new(name))
                .unwrap_or_else(|| panic!("{name} vanished"));
            assert!(
                entry.1.is_some(),
                "{name} must survive: a credential helper needs it"
            );
        }
    }

    #[test]
    fn a_prompt_cannot_wait_for_an_answer_nobody_will_give() {
        // The worst case named in the module docs, because it hangs invisibly.
        let forced: Vec<&str> = FORCED_VARS.iter().map(|(key, _)| *key).collect();
        assert!(forced.contains(&"GIT_TERMINAL_PROMPT"));
        assert!(forced.contains(&"GCM_INTERACTIVE"));
        assert_eq!(
            FORCED_VARS
                .iter()
                .find(|(key, _)| *key == "GIT_TERMINAL_PROMPT")
                .map(|(_, value)| *value),
            Some("0")
        );
        // The environment half; the config half is core.askPass in NEUTRALISED_CONFIG.
        assert!(SCRUBBED_VARS.contains(&"GIT_ASKPASS"));
        assert!(SCRUBBED_VARS.contains(&"SSH_ASKPASS"));
    }
    #[test]
    fn a_failing_command_carries_gits_own_words_rather_than_invented_ones() {
        let Ok(git) = Git::locate() else {
            return;
        };
        let empty = std::env::temp_dir().join(format!(
            "nysia-git-failure-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&empty).expect("temp dir");
        let at = CanonicalPath::of(&empty).expect("a folder");

        let err = git
            .run(&GitCommand::new(["rev-parse", "--git-dir"]), &at)
            .unwrap_err();
        match err {
            GitError::Failed { stderr, status, .. } => {
                assert!(status.contains("128"), "git exits 128 here, not {status}");
                assert!(!stderr.is_empty(), "git's own reason must reach the error");
            }
            other => panic!("expected a failure carrying git's stderr, got {other:?}"),
        }
        std::fs::remove_dir_all(&empty).ok();
    }
}
