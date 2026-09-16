//! Running one program safely, for callers that each bring their own policy.
//!
//! This is the half of D-15 that is **not about git**. Everything here answers "how do you
//! spawn a program from a daemon that lives for days without it becoming a hang, a leak or a
//! way to run something the caller did not name" — resolve-once, the working directory as a
//! type, the deadline, the tree-kill that enforces it, the two-thread drain, and the split
//! between "the child exited" and "the pipe reached EOF".
//!
//! [`command`](super::command) holds the other half: git's argv, git's environment, and
//! git's reading of an exit code. A `gh` module beside it will hold gh's. Both are callers
//! of this.
//!
//! # Why this is a split and not a second chokepoint
//!
//! `gh` needs every one of the properties listed above, and it needs them for sharper
//! reasons than git does: it reaches the network, it authenticates, and its answer is parsed
//! as JSON. What it cannot use is [`GitCommand`](super::command::GitCommand)'s argument
//! vector, which unconditionally prepends git's `-c` neutralisers and `--no-pager` — gh has
//! neither flag and exits before it reads the verb.
//!
//! The alternative was a second spawner, and
//! [`ResolvedProgram::argv`](crate::pty::ResolvedProgram::argv)'s own documentation warns
//! against exactly that. The machinery below took two review rounds to get right — the Job
//! Object, the drain grace, the pid-reuse hazard on Unix — and a copy of it would be a second
//! place for all three to be got wrong. So the *runner* is shared and the *policy* is not.
//!
//! # The one rule that keeps the sharing honest
//!
//! **An environment list belongs to the program, never to the runner.** [`EnvPolicy`] is a
//! parameter for that reason, and this module defines no list of its own: there is nothing
//! here for a later edit to grow "toward anything that names a credential", because there is
//! no shared list to grow. git's lists live with git and gh's live with gh, where the
//! consequence of a change is visible next to the reason for it — and where a per-program
//! compile-time check can see the whole of that program's effective set.

use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::path::CanonicalPath;
use crate::pty::{ResolveError, ResolvedProgram, resolve};

/// How often the deadline is checked while waiting for the child.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The most output a single invocation may produce before it is truncated.
///
/// A bound rather than a guess at what is enough: this reads into memory in a daemon that
/// lives for days, and the program is one on `PATH` that a user can replace. `git worktree
/// list` on a repository with a thousand worktrees is around 100 KiB, and a capped
/// `gh issue list` is far smaller than that.
///
/// Reaching it is reported rather than absorbed — see [`Finished::truncated`].
pub(crate) const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// How long the output pipes are given to reach EOF after the child itself has exited.
///
/// Nearly always zero work: a program closes its pipes as it exits and the drains are
/// already finished. It is the bound on the case where something the child started outlived
/// it and inherited the pipe — see [`run_to_completion`].
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Which environment variables a program is run with, and which it is run without.
///
/// # The lists belong to the program
///
/// This is a parameter rather than a constant because the two programs behind this runner
/// need opposite things, and the difference is not cosmetic. `GIT_TERMINAL_PROMPT=0` does
/// nothing for gh, whose own switch is `GH_PROMPT_DISABLED`; and git's scrub list is safe
/// for gh **only by accident**, because it happens to name no `GH_*` variable.
///
/// That accident is the hazard worth naming. `GH_TOKEN` and `GITHUB_TOKEN` are how a
/// developer authenticates, so a shared list that grew one day toward "anything that names a
/// credential" would not harden gh — it would make every machine running Nysia permanently
/// unauthenticated, and the symptom would be the *Tasks* screen saying nobody is signed in on
/// a machine where somebody plainly is. Keeping the lists with their programs is what makes
/// that edit impossible to make by accident, and a program whose credentials are at stake
/// can add a compile-time check that makes it impossible to make on purpose.
///
/// # Removal is by name, never `env_clear`
///
/// For the reason [`crate::pty`]'s environment handling gives: clearing the block takes
/// `SSH_AUTH_SOCK`, the Windows credential-manager variables and everything else a credential
/// helper needs, and the failure surfaces much later as an unexplained authentication error.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EnvPolicy {
    /// Variables removed from the child's environment, by name.
    pub(crate) scrubbed: &'static [&'static str],
    /// Variables set on the child's environment, whatever the parent's block said.
    pub(crate) forced: &'static [(&'static str, &'static str)],
}

/// Why an invocation did not produce an ending to read.
///
/// Deliberately small and deliberately *not* an error a caller would show a user: each arm
/// is something the caller turns into its own error type, with its own words and its own
/// next steps. A shared user-facing error here would be this module knowing about verbs.
#[derive(Debug)]
pub(crate) enum RunError {
    /// The resolved program refused the argument vector.
    ///
    /// Only reachable for a program that resolved to a batch shim, where
    /// [`ResolvedProgram::argv`](crate::pty::ResolvedProgram::argv) guards the arguments
    /// `cmd.exe` would act on. Letting it through silently would be the opposite of a
    /// chokepoint.
    Argv(ResolveError),
    /// The argument vector was empty, which no caller in this crate can produce.
    Empty,
    /// The spawn itself failed.
    Spawn(std::io::Error),
}

/// A resolved program, the environment policy it runs under, and its deadline.
///
/// Resolving once rather than per spawn is not only a saving: on Windows it is the
/// difference between an error that names what was looked for and a bare `NotFound` from
/// `CreateProcess` (traps register #8), which is what [`crate::pty::resolve`] exists to give.
#[derive(Debug, Clone)]
pub(crate) struct Runner {
    /// The resolved program, with any interpreter it needs in front of it.
    program: ResolvedProgram,
    /// The environment this program's spawns run under.
    env: EnvPolicy,
    /// The deadline each invocation gets.
    timeout: Duration,
}

impl Runner {
    /// Find `program` on `PATH`, pre-validate it, and bind it to `env`.
    ///
    /// # Errors
    ///
    /// Returns the resolver's own [`ResolveError`], which the caller turns into whatever its
    /// users need to be told. This is a real answer for a machine that has never had the
    /// program installed, and it is better given here than as a failed spawn at the bottom of
    /// a call stack.
    pub(crate) fn locate(
        program: &str,
        env: EnvPolicy,
        timeout: Duration,
    ) -> Result<Self, ResolveError> {
        Ok(Self {
            program: resolve(program)?,
            env,
            timeout,
        })
    }

    /// The same program with a different deadline.
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The deadline each invocation through this handle carries.
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The full vector this program would be spawned with, argv\[0\] first.
    ///
    /// Exists for the tests that need to run the *same* resolved program deliberately
    /// **unhardened**, as the control half of a proof that the hardening does something —
    /// `the_neutralisers_stop_programs_the_repository_config_names` is the example. A control
    /// that spawned a differently-resolved program would be comparing two things at once.
    ///
    /// Not a way to spawn past the policy: it returns a vector, and everything that makes an
    /// invocation safe is applied by [`Runner::run`], which is the only thing that spawns.
    ///
    /// # Errors
    ///
    /// As [`ResolvedProgram::argv`](crate::pty::ResolvedProgram::argv).
    #[cfg(test)]
    pub(crate) fn resolved_argv<I, S>(&self, args: I) -> Result<Vec<OsString>, ResolveError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.program.argv(args)
    }

    /// Run `args` in `at`, and hand back everything about how it ended without judging it.
    ///
    /// Judgement is the caller's, because the two callers judge differently: a non-zero exit
    /// is a failure for git, and for gh it is the *interesting* answer — the difference
    /// between a missing credential and an unreachable host is read out of exactly the
    /// ending this returns.
    ///
    /// # The working directory is a type
    ///
    /// [`CanonicalPath`] can only be built from a path that resolved to an existing
    /// directory, so a spawn against something that is not a folder has no spelling. It is
    /// set on the child rather than passed as an argument, which is the rule both callers
    /// keep: a path an option parser can see is a path that can be an option.
    ///
    /// # Errors
    ///
    /// See [`RunError`].
    pub(crate) fn run(&self, args: &[OsString], at: &CanonicalPath) -> Result<Finished, RunError> {
        let argv = self.program.argv(args).map_err(RunError::Argv)?;
        let (program, rest) = argv.split_first().ok_or(RunError::Empty)?;

        let mut spawn = Command::new(program);
        spawn
            .args(rest)
            // The confinement: the folder is where the child runs, never something it parses.
            .current_dir(at.as_path())
            // Nothing to read, so a prompt that got past the environment still cannot wait.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_environment(&mut spawn, &self.env);

        run_to_completion(spawn, self.timeout).map_err(RunError::Spawn)
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
    /// Whether stdout reached [`MAX_OUTPUT_BYTES`] and was cut.
    ///
    /// Reported rather than absorbed, because a truncated answer is not a short answer and
    /// the difference is silent in both callers' parsers. git's `worktree list` would parse
    /// cleanly and come back *missing worktrees*, which is how a worktree gets registered
    /// twice; gh's JSON would stop mid-array, which parses as a failure but reports the
    /// wrong reason for it. Both callers turn this into an error that says the output was too
    /// large, which is the true sentence and the only one a reader can act on.
    pub(crate) truncated: bool,
}

/// Scrub the inherited environment and force what every invocation needs.
///
/// Applied to every spawn, after nothing and before nothing: **there is no caller-supplied
/// environment to layer**, which is the point. A scrub a caller can reintroduce a variable
/// through is not a scrub, it is a suggestion — and for git, `GIT_DIR` alone is enough to
/// make the answer be about a repository nobody asked about.
///
/// There is deliberately no overload taking extra variables. If one is ever genuinely needed
/// it belongs behind a separately named entry point whose name says so, so that the call site
/// is obvious in review and greppable, and never on the path everyone already uses.
pub(crate) fn apply_environment(command: &mut Command, env: &EnvPolicy) {
    for name in env.scrubbed {
        command.env_remove(name);
    }
    for (key, value) in env.forced {
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
/// **"the child exited" and "the pipe reached EOF" are different events** (traps register
/// #11). EOF arrives only when every writer has closed, and a helper the child started and
/// detached — `git fsmonitor--daemon` is the one that exists — inherits the pipe and holds it
/// open after the child itself is gone. Waiting for EOF unconditionally is therefore an
/// unbounded wait, so the collection has its own grace period and the function returns what
/// it has rather than never returning at all.
///
/// # A limitation, stated rather than left to be discovered
///
/// What happens to that holder is **not the same on the two platforms**, and the Unix side is
/// the weaker one. On Windows the Job Object is a kernel object rather than a pid, so
/// terminating it once the child has been reaped is precise and the holder goes away with it.
/// On Unix there is no equivalent handle here: the child has been reaped, its pid is the
/// kernel's to hand out again, and signalling a process group by a number that may now belong
/// to somebody else is a worse failure than a truncated answer — so nothing is signalled. The
/// holder keeps running and its drain thread lives as long as the daemon does, bounded at
/// [`MAX_OUTPUT_BYTES`] of memory each but unbounded in number.
///
/// Closing it needs a process group Nysia allocates and holds rather than one it signals by
/// number, which is the same gap [`crate::pty`]'s teardown documents for a grandchild that
/// double-forked away. Nothing v0.3 runs reaches it: the only git helper that detaches is the
/// fsmonitor daemon, and git's neutralisers turn that off.
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
        // Something that outlived the child still holds a pipe.
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
            tracing::warn!("a process outlived the child and still holds its output pipe");
        }
        stdout =
            stdout.or_else(|| collect_within(drain_out.as_ref(), Instant::now() + DRAIN_GRACE));
        stderr =
            stderr.or_else(|| collect_within(drain_err.as_ref(), Instant::now() + DRAIN_GRACE));
    }

    let stdout = stdout.unwrap_or_default();
    let truncated = stdout.truncated;
    Ok(Finished {
        code: status.and_then(|status| status.code()),
        // An answer that could not be collected is empty rather than absent: it then fails
        // parsing with a message saying the output was not the promised shape, which is true.
        stdout: stdout.bytes,
        stderr: stderr.unwrap_or_default().bytes,
        timed_out,
        truncated,
    })
}

/// Take a drain thread's output, giving up at `until`.
///
/// `None` means the thread has not finished, which means the pipe is still open, which means
/// something is still holding it. A missing receiver — the pipe was never piped — is an empty
/// answer rather than a wait.
fn collect_within(drain: Option<&Drain>, until: Instant) -> Option<Drained> {
    let Some(drain) = drain else {
        return Some(Drained::default());
    };
    drain
        .recv_timeout(until.saturating_duration_since(Instant::now()))
        .ok()
}

/// What one pipe produced, and whether it produced more than it was allowed to.
#[derive(Debug, Default)]
struct Drained {
    bytes: Vec<u8>,
    truncated: bool,
}

/// A drain thread's end of the handover.
///
/// A channel rather than a [`std::thread::JoinHandle`], because a join cannot be given a
/// deadline and this one needs one.
type Drain = std::sync::mpsc::Receiver<Drained>;

/// Read a pipe to EOF on its own thread, stopping at [`MAX_OUTPUT_BYTES`].
fn drain<R: std::io::Read + Send + 'static>(mut pipe: R) -> Drain {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut collected = Drained::default();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let room = MAX_OUTPUT_BYTES.saturating_sub(collected.bytes.len());
                    // Past the cap this keeps reading and discarding rather than returning:
                    // dropping the pipe here would give the child `EPIPE` mid-write, which
                    // turns a large answer into a confusing failure. The flag is what stops
                    // the discarding from being silent.
                    collected.truncated |= read > room;
                    collected.bytes.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
        }
        // The receiver is gone if the caller gave up on this pipe, which is not a failure.
        sender.send(collected).ok();
    });
    receiver
}

/// A child's stderr, trimmed and bounded, for an error message.
///
/// Bounded because this reaches a log line and an error envelope: a program can print a great
/// deal on a failure, and a 4 MiB single-line log entry is a log nobody reads.
pub(crate) fn trim_stderr(stderr: &[u8]) -> String {
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
            .inspect_err(|err| tracing::warn!(%err, "spawn: no job object; a timeout kill will reach the direct child only"))
            .ok()?;
        job.assign(child.as_raw_handle())
            .inspect_err(|err| tracing::warn!(%err, "spawn: could not join the job object"))
            .ok()?;
        Some(job)
    }

    /// Terminate the job, and the child directly if there is no job.
    pub(super) fn terminate(job: Option<&JobObject>, child: &mut std::process::Child) {
        if let Some(job) = job
            && let Err(err) = job.terminate()
        {
            tracing::warn!(%err, "spawn: TerminateJobObject failed; falling back to the child");
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
    /// here with exit traps to run, only a child that has already overrun its deadline, and
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
    use std::process::Stdio;

    use super::*;

    /// Pipe both output streams, which [`Runner::run`] does for a real invocation.
    ///
    /// These tests call [`run_to_completion`] directly — the point is the machinery, not any
    /// one program's reading of it — so they have to do for themselves what the method above
    /// would have done. Without it `child.stdout` is `None`, no drain thread is started, and
    /// every assertion about collected output passes vacuously against an empty vector.
    fn piped(mut command: Command) -> Command {
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command
    }

    /// A command that waits far longer than any deadline a test will give it.
    ///
    /// `ping` against loopback rather than a black-holed address: pinging something
    /// unreachable exits immediately on some networks, which would make a timeout test pass
    /// without a timeout ever happening.
    fn a_slow_command() -> Command {
        piped(if cfg!(windows) {
            let mut ping = Command::new("ping");
            ping.args(["-n", "30", "127.0.0.1"]);
            ping
        } else {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            sleep
        })
    }

    #[test]
    fn a_command_that_overruns_its_deadline_is_killed_rather_than_waited_out() {
        // The timeout, against a real child. `run_to_completion` rather than a verb, because
        // the point is the machinery and not any one program's reading of it.
        //
        // The proof that the kill happened is the clock: the command sleeps for 30 seconds
        // and this returns in well under one, so the deadline did something rather than the
        // child merely finishing.
        let started = Instant::now();
        let finished = run_to_completion(a_slow_command(), Duration::from_millis(400))
            .expect("the spawn must succeed");
        let elapsed = started.elapsed();

        assert!(finished.timed_out, "the deadline must be reported");
        assert!(
            elapsed < Duration::from_secs(10),
            "a killed child must not be waited out: {elapsed:?}"
        );
    }

    #[test]
    fn a_deadline_that_is_not_reached_is_not_reported() {
        // The other half, so the test above cannot pass by reporting every command as timed
        // out. A command that finishes well inside its deadline must come back clean.
        let quick = piped(if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/c", "echo done"]);
            cmd
        } else {
            let mut echo = Command::new("echo");
            echo.arg("done");
            echo
        });

        let finished =
            run_to_completion(quick, Duration::from_secs(30)).expect("the spawn must succeed");
        assert!(!finished.timed_out);
        assert_eq!(finished.code, Some(0));
        assert!(!finished.truncated, "a short answer is not a truncated one");
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
        let chatty = piped(if cfg!(windows) {
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
        });

        let finished =
            run_to_completion(chatty, Duration::from_secs(60)).expect("the spawn must succeed");
        assert!(!finished.timed_out, "the command was not given enough time");
        assert!(
            finished.stdout.len() > 64 * 1024,
            "the test wrote {} bytes, which is not past a pipe buffer",
            finished.stdout.len()
        );
        assert!(
            !finished.truncated,
            "64 KiB is nowhere near the cap; a truncation here means the flag is wrong"
        );
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

    #[test]
    fn an_environment_policy_removes_and_forces_exactly_what_it_names_and_nothing_else() {
        // The named proof for the rule the module documentation states: the runner has no
        // list of its own, so what a child's environment is changed by is exactly the
        // policy it was handed. `get_envs` is the actual mutation set applied to the child,
        // which is why it is asserted rather than the constants being re-read.
        //
        // Mutating either list below turns this red, and it is the same assertion each
        // caller's own environment test is built on.
        static POLICY: EnvPolicy = EnvPolicy {
            scrubbed: &["NYSIA_TEST_REMOVED"],
            forced: &[("NYSIA_TEST_FORCED", "1")],
        };

        let mut command = Command::new("nysia-not-spawned");
        apply_environment(&mut command, &POLICY);

        let changes: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();

        // Sorted, because `get_envs` reports the child's overrides in its own order rather
        // than in the order they were applied, and the assertion is about membership.
        let mut changes = changes;
        changes.sort();
        assert_eq!(
            changes,
            vec![
                ("NYSIA_TEST_FORCED".to_owned(), Some("1".to_owned())),
                ("NYSIA_TEST_REMOVED".to_owned(), None),
            ],
            "the runner must change exactly what the policy names"
        );
    }

    #[test]
    fn output_past_the_cap_is_reported_as_truncated_rather_than_silently_cut() {
        // The proof for [`Finished::truncated`]. Driven through `drain` directly rather than
        // through a child, because the cap is 8 MiB and no process in a test suite should be
        // asked to write that much — `io::repeat` produces it from memory in milliseconds.
        //
        // Deleting the `collected.truncated |=` line turns this red while every other test
        // in this module stays green, which is the mutation this test exists for.
        let oversized = std::io::Read::take(std::io::repeat(b'a'), MAX_OUTPUT_BYTES as u64 + 1);
        let drained = drain(oversized)
            .recv_timeout(Duration::from_secs(60))
            .expect("the drain thread finishes on EOF");

        assert!(drained.truncated, "output past the cap must say so");
        assert_eq!(
            drained.bytes.len(),
            MAX_OUTPUT_BYTES,
            "the cap must still bound what is kept"
        );
    }

    #[test]
    fn output_that_exactly_fills_the_cap_is_not_truncated() {
        // The control, and it is not a formality: a `truncated` computed with `>=` rather
        // than `>` passes the test above and reports every full-but-complete answer as cut,
        // which would turn the largest legitimate answer into an error.
        let exact = std::io::Read::take(std::io::repeat(b'a'), MAX_OUTPUT_BYTES as u64);
        let drained = drain(exact)
            .recv_timeout(Duration::from_secs(60))
            .expect("the drain thread finishes on EOF");

        assert!(!drained.truncated, "an answer that fits was not cut");
        assert_eq!(drained.bytes.len(), MAX_OUTPUT_BYTES);
    }
}
