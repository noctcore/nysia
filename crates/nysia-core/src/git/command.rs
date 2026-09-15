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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::error::GitError;
use super::path::CanonicalPath;
use crate::pty::{ResolvedProgram, resolve};

/// How long an invocation gets before it is killed.
///
/// Generous for the local, read-only questions this module asks — `rev-parse` and
/// `worktree list` answer in milliseconds on any repository — and short enough that a user
/// who pointed Nysia at a folder on a disconnected network share gets an error rather than a
/// sidebar that never finishes loading.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the deadline is checked while waiting for the child.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The most output a single invocation may produce before it is truncated.
///
/// A bound rather than a guess at what is enough: this reads into memory in a daemon that
/// lives for days, and `git` is a program on `PATH` that a user can replace. `worktree list`
/// on a repository with a thousand worktrees is around 100 KiB.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// How long the output pipes are given to reach EOF after git itself has exited.
///
/// Nearly always zero work: git closes its pipes as it exits and the drains are already
/// finished. It is the bound on the case where something git started outlived it and
/// inherited the pipe — see [`run_to_completion`].
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// git's exit code for a usage error, as opposed to 128 for an operational failure.
const USAGE_EXIT_CODE: i32 = 129;

/// The config settings pinned on every invocation, each of which names a program git would
/// otherwise run.
///
/// Passed as `-c key=value` ahead of the verb, where they outrank `/etc/gitconfig`, the
/// user's `~/.gitconfig` **and** the repository's own `.git/config` — which is the one that
/// matters, because it is the file an agent working in a checkout can write.
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

/// A resolved `git`, and the deadline every invocation through it carries.
///
/// Resolving once rather than per spawn is not only a saving: on Windows it is the
/// difference between an error that names what was looked for and a bare `NotFound` from
/// `CreateProcess` (traps register #8), which is what `pty::resolve` exists to give.
#[derive(Debug, Clone)]
pub struct Git {
    /// The resolved program, with any interpreter it needs in front of it.
    program: ResolvedProgram,
    /// The deadline each invocation gets.
    timeout: Duration,
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
            program: resolve(program).map_err(|source| GitError::NotInstalled { source })?,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// The same git with a different deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The deadline each invocation through this handle carries.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
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
        match output.failure(command, at, self.timeout) {
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
        let argv = self.program.argv(command.argv()).map_err(|source| {
            // Only reachable if `git` resolved to a batch shim, which no git installation
            // ships; the refusal is `pty::resolve`'s BatBadBut guard, and letting it through
            // silently would be the opposite of a chokepoint.
            GitError::NotInstalled { source }
        })?;
        let (program, args) = argv.split_first().ok_or_else(|| GitError::Unparsable {
            args: command.describe(),
            problem: "an empty argument vector".to_owned(),
        })?;

        let mut spawn = Command::new(program);
        spawn
            .args(args)
            // The confinement: the folder is where the child runs, never something it parses.
            .current_dir(at.as_path())
            // Nothing to read, so a prompt that got past the environment still cannot wait.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_environment(&mut spawn);

        run_to_completion(spawn, self.timeout).map_err(|source| GitError::Spawn {
            at: at.as_path().to_path_buf(),
            source,
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

/// How a child ended, before anything decided whether that was a failure.
#[derive(Debug)]
pub(crate) struct Finished {
    /// Its exit code, or `None` when a signal ended it.
    pub(crate) code: Option<i32>,
    /// Everything it wrote to stdout, truncated at [`MAX_OUTPUT_BYTES`].
    pub(crate) stdout: Vec<u8>,
    /// Everything it wrote to stderr, truncated at [`MAX_OUTPUT_BYTES`].
    pub(crate) stderr: Vec<u8>,
    /// Whether it was killed for passing its deadline.
    pub(crate) timed_out: bool,
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
    #[allow(
        dead_code,
        reason = "the worktree verbs that need a positional path are wave C; the fixtures in `testing` use it today"
    )]
    pub(crate) fn operand_path(mut self, path: &Path) -> Self {
        self.operands.push(path.as_os_str().to_owned());
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

/// Scrub the inherited environment and force what every invocation needs.
///
/// Applied to every spawn, after nothing and before nothing: there is no caller-supplied
/// environment to layer, which is the point. A scrub a caller can reintroduce a variable
/// through is not a scrub — and `GIT_DIR` alone is enough to make this module answer about a
/// repository nobody asked about.
fn apply_environment(command: &mut Command) {
    for name in SCRUBBED_VARS {
        command.env_remove(name);
    }
    for (key, value) in FORCED_VARS {
        command.env(key, value);
    }
}

/// Spawn, drain both pipes, and enforce the deadline.
///
/// The pipes are drained on their own threads because the alternative deadlocks: a child
/// that fills the 64 KiB pipe buffer blocks on write, and a parent that is waiting for the
/// child to exit before it reads will wait forever. `git worktree list` on a repository with
/// a few hundred worktrees passes that buffer.
///
/// The `Child` stays on this thread so that the kill on expiry has something to kill.
/// Handing it to a worker and signalling a remembered pid instead would be a pid-reuse race
/// in a process that lives for days.
///
/// **"git exited" and "the pipe reached EOF" are different events** (traps register #11).
/// EOF arrives only when every writer has closed, and a helper git started and detached —
/// `git fsmonitor--daemon` is the one that exists — inherits the pipe and holds it open after
/// git itself is gone. Waiting for EOF unconditionally is therefore an unbounded wait, so the
/// collection has its own grace period and the function returns what it has rather than never
/// returning at all.
fn run_to_completion(mut spawn: Command, timeout: Duration) -> std::io::Result<Finished> {
    #[cfg(unix)]
    let spawn = unix_kill::in_its_own_session(&mut spawn);

    let mut child = spawn.spawn()?;

    // Windows: everything the child starts inherits the job, so terminating it on expiry
    // takes the whole tree (traps register #7). ConPTY has no signals and neither does a
    // plain `CreateProcess` child, so there is no gentler mechanism to try first.
    #[cfg(windows)]
    let job = windows_kill::confine(&child);

    let drain_out = child.stdout.take().map(drain);
    let drain_err = child.stderr.take().map(drain);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                timed_out = true;
                #[cfg(windows)]
                windows_kill::terminate(job.as_ref(), &mut child);
                #[cfg(unix)]
                unix_kill::terminate(&mut child);
                // The wait is what reaps it; the kill only asks.
                break child.wait().ok();
            }
            None => std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            ),
        }
    };

    // Collected after the child is gone, so an ordinary command's pipes are already at EOF
    // and this costs nothing. The grace is for the case above.
    let until = Instant::now() + DRAIN_GRACE;
    let mut stdout = collect_within(drain_out.as_ref(), until);
    let mut stderr = collect_within(drain_err.as_ref(), until);
    if stdout.is_none() || stderr.is_none() {
        // Something that outlived git still holds a pipe.
        #[cfg(windows)]
        {
            // The job holds this child's descendants and nothing else, so terminating it
            // reaches the holder precisely — and it is still safe after the child has been
            // reaped, because a job is a kernel object rather than a pid that can be reused.
            windows_kill::terminate(job.as_ref(), &mut child);
        }
        #[cfg(unix)]
        {
            // Deliberately *not* `killpg` here. The child has been reaped, so its pid is the
            // kernel's to hand out again, and signalling a group by a number that may now
            // belong to somebody else is a worse failure than a truncated answer.
            tracing::warn!("a process outlived git and still holds its output pipe");
        }
        stdout =
            stdout.or_else(|| collect_within(drain_out.as_ref(), Instant::now() + DRAIN_GRACE));
        stderr =
            stderr.or_else(|| collect_within(drain_err.as_ref(), Instant::now() + DRAIN_GRACE));
    }

    Ok(Finished {
        code: status.and_then(|status| status.code()),
        // An answer that could not be collected is empty rather than absent: it then fails
        // parsing with a message saying the output was not the promised shape, which is true.
        stdout: stdout.unwrap_or_default(),
        stderr: stderr.unwrap_or_default(),
        timed_out,
    })
}

/// Take a drain thread's output, giving up at `until`.
///
/// `None` means the thread has not finished, which means the pipe is still open, which means
/// something is still holding it. A missing receiver — the pipe was never piped — is an empty
/// answer rather than a wait.
fn collect_within(drain: Option<&Drain>, until: Instant) -> Option<Vec<u8>> {
    let Some(drain) = drain else {
        return Some(Vec::new());
    };
    drain
        .recv_timeout(until.saturating_duration_since(Instant::now()))
        .ok()
}

/// A drain thread's end of the handover.
///
/// A channel rather than a [`std::thread::JoinHandle`], because a join cannot be given a
/// deadline and this one needs one.
type Drain = std::sync::mpsc::Receiver<Vec<u8>>;

/// Read a pipe to EOF on its own thread, stopping at [`MAX_OUTPUT_BYTES`].
fn drain<R: std::io::Read + Send + 'static>(mut pipe: R) -> Drain {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let room = MAX_OUTPUT_BYTES.saturating_sub(collected.len());
                    // Past the cap this keeps reading and discarding rather than returning:
                    // dropping the pipe here would give the child `EPIPE` mid-write, which
                    // turns a large answer into a confusing failure.
                    collected.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
        }
        // The receiver is gone if the caller gave up on this pipe, which is not a failure.
        sender.send(collected).ok();
    });
    receiver
}

/// git's stderr, trimmed and bounded, for an error message.
///
/// Bounded because this reaches a log line and an error envelope: git can print a great deal
/// on a failure, and a 4 MiB single-line log entry is a log nobody reads.
fn trim_stderr(stderr: &[u8]) -> String {
    const MAX_MESSAGE_BYTES: usize = 2000;

    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    if trimmed.len() <= MAX_MESSAGE_BYTES {
        return trimmed.to_owned();
    }
    let mut cut = MAX_MESSAGE_BYTES;
    while cut > 0 && !trimmed.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &trimmed[..cut])
}

/// Tree-kill on Windows, through the Job Object trap 7 names.
#[cfg(windows)]
mod windows_kill {
    use crate::pty::JobObject;

    /// Put a spawned child into a job that kills its members when the job is terminated, or
    /// when the last handle to it closes.
    ///
    /// Returns `None` when the job could not be created or the child could not be assigned,
    /// which leaves the direct-child kill below as the fallback rather than failing a spawn
    /// that is otherwise fine. The window between `CreateProcess` returning and
    /// `AssignProcessToJobObject` running is the same one `pty::teardown` documents; closing
    /// it needs `PROC_THREAD_ATTRIBUTE_JOB_LIST`, and `std::process` does not expose the
    /// attribute list.
    pub(super) fn confine(child: &std::process::Child) -> Option<JobObject> {
        use std::os::windows::io::AsRawHandle;

        let job = JobObject::new()
            .inspect_err(|err| tracing::warn!(%err, "git spawn: no job object; a timeout kill will reach the direct child only"))
            .ok()?;
        job.assign(child.as_raw_handle())
            .inspect_err(|err| tracing::warn!(%err, "git spawn: could not join the job object"))
            .ok()?;
        Some(job)
    }

    /// Terminate the job, and the child directly if there is no job.
    pub(super) fn terminate(job: Option<&JobObject>, child: &mut std::process::Child) {
        if let Some(job) = job
            && let Err(err) = job.terminate()
        {
            tracing::warn!(%err, "git spawn: TerminateJobObject failed; falling back to the child");
        }
        // Belt and braces: a child already terminated by the job reports `InvalidInput` or
        // succeeds, and neither is worth a log line.
        let _ = child.kill();
    }
}

/// Tree-kill on Unix, by signalling the process group.
#[cfg(unix)]
mod unix_kill {
    use std::process::Command;

    /// Make the child a session leader, so that its pid is also a process-group id.
    ///
    /// Without this the child shares the daemon's process group, and signalling that group
    /// on a timeout would signal the daemon. It is the same `setsid` `portable-pty` does for
    /// a pty child, done here because `std::process` does not.
    pub(super) fn in_its_own_session(command: &mut Command) -> &mut Command {
        use std::os::unix::process::CommandExt;

        // SAFETY: `pre_exec` runs between `fork` and `exec`, where only async-signal-safe
        // calls are allowed. `setsid` is one; it allocates nothing and takes no lock.
        unsafe {
            command.pre_exec(|| {
                // `EPERM` means this process is already a group leader, which is harmless:
                // the group is then its own, which is all the kill needs.
                libc::setsid();
                Ok(())
            })
        }
    }

    /// Kill the child's process group, then the child itself.
    ///
    /// `SIGKILL` without a `SIGTERM` grace period, unlike `pty::teardown`: there is no shell
    /// here with exit traps to run, only a `git` that has already overrun its deadline, and
    /// a second grace period is a second way for the daemon to wait.
    pub(super) fn terminate(child: &mut std::process::Child) {
        let pid = child.id();
        if let Ok(pgid) = i32::try_from(pid) {
            // SAFETY: `killpg` takes two integers and has no memory effects. A group that is
            // already gone reports `ESRCH`, which needs no handling: the child is reaped by
            // the `wait` that follows either way.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        apply_environment(&mut command);

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
                .program
                .argv(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .expect("a plain flag is safe for any resolved program");
            let (program, args) = argv.split_first().expect("argv holds the program");
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(&plain)
                .env("GIT_DIR", repo.join(".git"))
                .stdin(Stdio::null());
            if scrubbed {
                apply_environment(&mut command);
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
        apply_environment(&mut command);

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

    /// A command that waits far longer than any deadline a test will give it.
    ///
    /// `ping` against loopback rather than a black-holed address: pinging something
    /// unreachable exits immediately on some networks, which would make a timeout test pass
    /// without a timeout ever happening.
    fn a_slow_command() -> Command {
        let mut command = if cfg!(windows) {
            let mut ping = Command::new("ping");
            ping.args(["-n", "30", "127.0.0.1"]);
            ping
        } else {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            sleep
        };
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command
    }

    #[test]
    fn a_command_that_overruns_its_deadline_is_killed_rather_than_waited_out() {
        // The timeout, against a real child. `run_to_completion` rather than `Git::run`,
        // because `GitCommand` prepends git's `-c` neutralisers to everything it builds — so
        // a non-git program driven through it exits instantly on an unknown switch, and a
        // timeout test written that way passes without ever timing out.
        //
        // The proof that the kill happened is the clock. `run_to_completion` finishes with
        // `child.wait()`, which blocks until the child is really gone, so returning in well
        // under the ~29s the command would take *is* the assertion that it was killed.
        let started = Instant::now();
        let finished = run_to_completion(a_slow_command(), Duration::from_millis(400))
            .expect("the spawn itself must succeed");
        let elapsed = started.elapsed();

        assert!(
            finished.timed_out,
            "the deadline passed and was not reported"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "waited {elapsed:?} for a 400ms deadline, so nothing killed the child"
        );
    }

    #[test]
    fn a_deadline_that_is_not_reached_is_not_reported() {
        // The other half, so the test above cannot pass by reporting every command as timed
        // out. A command that finishes well inside its deadline must come back clean.
        let mut quick = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/c", "echo done"]);
            cmd
        } else {
            let mut echo = Command::new("echo");
            echo.arg("done");
            echo
        };
        quick.stdin(Stdio::null());
        quick.stdout(Stdio::piped());
        quick.stderr(Stdio::piped());

        let finished =
            run_to_completion(quick, Duration::from_secs(30)).expect("the spawn must succeed");
        assert!(!finished.timed_out);
        assert_eq!(finished.code, Some(0));
        assert!(
            String::from_utf8_lossy(&finished.stdout).contains("done"),
            "stdout was not collected at all"
        );
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        // Why the pipes are drained on their own threads. A child that fills the ~64 KiB
        // pipe buffer blocks on write, and a parent that waits for the child before reading
        // waits forever. This writes several times that.
        let mut chatty = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args([
                "/c",
                "for /L %i in (1,1,4000) do @echo 0123456789012345678901234567890123456789",
            ]);
            cmd
        } else {
            let mut sh = Command::new("sh");
            sh.args([
                "-c",
                "i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done",
            ]);
            sh
        };
        chatty.stdin(Stdio::null());
        chatty.stdout(Stdio::piped());
        chatty.stderr(Stdio::piped());

        let finished =
            run_to_completion(chatty, Duration::from_secs(60)).expect("the spawn must succeed");
        assert!(!finished.timed_out, "the command was not given enough time");
        assert!(
            finished.stdout.len() > 64 * 1024,
            "the test wrote {} bytes, which is not past a pipe buffer",
            finished.stdout.len()
        );
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

    #[test]
    fn a_long_stderr_is_bounded_on_a_character_boundary() {
        let wide = "é".repeat(4000);
        let trimmed = trim_stderr(wide.as_bytes());
        assert!(
            trimmed.len() <= 2001 + 3,
            "{} bytes is not bounded",
            trimmed.len()
        );
        assert!(trimmed.ends_with('…'));
        // The cut landed on a boundary, or the slice above would have panicked. Asserting it
        // explicitly so the reason the loop exists is visible.
        assert!(trimmed.chars().all(|c| c == '…' || c == 'é'));
    }
}
