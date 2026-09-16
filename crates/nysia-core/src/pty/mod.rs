//! PTY sessions: one pseudo-terminal per session, and the process tree behind it.
//!
//! Will own the `portable-pty` pair for each session, the single blocking reader thread
//! that drains its master, the writer half, resize, and the lifecycle of the child process
//! tree. Shells in v1 are pwsh, cmd, Git Bash and WSL (D-3, D-17).
//!
//! Four things this module exists to get right, each of which has already cost a day:
//!
//! - **ConPTY flags.** `portable-pty` is pinned to wezterm `main` because the crates.io
//!   0.9.0 release drops `RESIZE_QUIRK` and `WIN32_INPUT_MODE`.
//! - **Closing order.** `ClosePseudoConsole` blocks until the client exits, so it must be
//!   called from a thread that is *not* the reader, and only after the reader has drained.
//!   Calling it from the reader deadlocks.
//! - **Tree-kill on Windows.** ConPTY has no signals. Every child goes into a Job Object
//!   with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so that closing the handle takes the whole
//!   tree with it.
//! - **Exit is not EOF.** "The child exited" and "the PTY reached EOF" are different
//!   events; EOF only arrives once every slave fd is closed, which a grandchild can delay
//!   indefinitely. They are reported separately and never inferred from one another.
//!
//! Spawning on Windows resolves the program with a `which`-style lookup first:
//! `CreateProcess` only launches real executables, so `Command::new("claude")` fails with
//! `NotFound` against the `.cmd`/`.ps1` shims npm installs.
//!
//! Owned by wave 1 (W2).

mod env;
mod profile;
mod resolve;
mod session;
mod teardown;

pub use env::{FORCED_COLORTERM, FORCED_TERM, SCRUBBED_VARS, sanitize};
pub use profile::{ProfileError, ShellProfile};
pub use resolve::{ResolveError, ResolvedProgram, resolve};
pub use session::{
    DEFAULT_OUTPUT_QUEUE, Output, PtyOutput, PtySession, SessionProgram, SessionSpec, SpawnError,
};
pub use teardown::DEFAULT_GRACE;

// `crate::git` needs the same Job Object to enforce its timeouts: a `git` killed for
// overrunning its deadline has to take any helper it started with it, and ConPTY has no
// signals to do that with (traps register #7). Re-exported rather than duplicated, because
// two copies of the same unsafe handle-lifetime dance is exactly how trap 7 gets paid for a
// second time.
#[cfg(windows)]
pub(crate) use teardown::JobObject;
