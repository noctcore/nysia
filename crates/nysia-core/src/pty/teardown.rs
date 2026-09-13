//! Killing a session's whole process tree.
//!
//! The two platforms need entirely different mechanisms, and neither is what
//! `portable-pty`'s own [`ChildKiller`](portable_pty::ChildKiller) does — that signals the
//! direct child only, so a shell's background job survives it.
//!
//! **Windows.** ConPTY has no signals. A child is put into a Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` at spawn time, and every process it starts inherits
//! that membership, so terminating the job — or merely closing its last handle — takes the
//! whole tree. There is a small window between `CreateProcess` returning and
//! `AssignProcessToJobObject` running in which a grandchild started immediately could
//! escape; closing it needs `PROC_THREAD_ATTRIBUTE_JOB_LIST`, which `portable-pty`'s
//! attribute list does not expose. A shell cannot start a grandchild that fast in
//! practice, and the alternative is forking the pty crate.
//!
//! **Unix.** `portable-pty` makes the child a session leader with `setsid`, so `killpg`
//! reaches everything in the shell's own process group: `SIGTERM` first, so a shell can run
//! its exit traps, a short grace period, then `SIGKILL`.
//!
//! `killpg` alone is not enough. An interactive shell with job control puts every `&` job
//! in a process group of its *own*, so `sleep 300 &` is not in the group `killpg` just
//! signalled. §7.1 names the answer — macOS has no `PR_SET_CHILD_SUBREAPER`, so sweep the
//! session — and the sweep is built from the machine's process table.
//!
//! Membership is decided by **two relations**, because neither is sufficient alone:
//!
//! - **Session.** `/proc/<pid>/stat` reports a session id directly on Linux. Elsewhere it
//!   comes from `getsid`, which on Darwin is unconditional: XNU's `getsid` (`kern_prot.c`)
//!   reads `p_sessionid` for any pid it can find and fails only with `ESRCH`, and the
//!   "implementation may restrict this to the caller's session" line on the man page is a
//!   portability note rather than a documented error of that kernel. POSIX does permit a
//!   kernel to restrict it, though, which is the first reason not to rely on it alone.
//! - **Parent.** The transitive closure of parent-to-child links from the leader. This is
//!   the second reason: it reaches a backgrounded job even where the session relation is
//!   unavailable, since the job is still a child of the shell.
//!
//! The tree is captured **before** anything is signalled. Once the leader dies its children
//! reparent to `launchd` or `init` and the parent relation reports nothing, and a zombie is
//! not returned by Darwin's `proc_find` at all, so `getsid` on one reports `ESRCH`. The set
//! to kill has to be taken while the shell is still holding it together.
//!
//! The Darwin flavour matters: `proc_pidinfo` with `PROC_PIDTBSDINFO` is same-user
//! restricted and returns `EPERM` across users, while `PROC_PIDT_SHORTBSDINFO` is on the
//! kernel's `NO_CHECK_SAME_USER` list. The short flavour is the one used here, and it is
//! also the one that carries `pbsi_ppid`.
//!
//! The gap that remains, stated rather than discovered: a grandchild that double-forked and
//! reparented away before teardown began is reachable through the session relation but not
//! through the parent one, so it survives on any kernel that does restrict `getsid`.
//! Nothing in v1 spawns one, and closing it properly needs a process group Nysia allocates
//! rather than one the shell chose.
//!
//! One more consequence worth stating: an interactive `bash` ignores `SIGTERM`, so on a
//! Unix shell the grace period is normally burned in full before the `SIGKILL` phase does
//! the work.

use std::time::{Duration, Instant};

/// How often the grace period is polled for the tree having gone away on its own.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The default grace between `SIGTERM` and `SIGKILL`, inside the 1–3s §7.1 calls for.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// Poll `exited` until it reports `true` or `grace` elapses. Returns whether it exited.
pub(crate) fn wait_for(grace: Duration, mut exited: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        if exited() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(windows)]
pub(crate) use windows_impl::JobObject;

#[cfg(windows)]
mod windows_impl {
    use std::io;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows::core::PCWSTR;

    /// The exit code reported for a tree Nysia terminated, rather than one that exited.
    const TERMINATED_EXIT_CODE: u32 = 1;

    /// An unnamed Job Object configured to kill everything in it when its last handle
    /// closes.
    ///
    /// Dropping this is itself a tree-kill: that is the point of
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and it means a session that is dropped without
    /// an orderly shutdown still cannot leave orphans behind.
    #[derive(Debug)]
    pub(crate) struct JobObject {
        handle: HANDLE,
    }

    // A job handle is a kernel object, usable from any thread; the raw pointer inside
    // `HANDLE` is what makes the auto-traits opt out.
    unsafe impl Send for JobObject {}
    unsafe impl Sync for JobObject {}

    impl JobObject {
        /// Create a job that kills its members when the last handle to it closes.
        pub(crate) fn new() -> io::Result<Self> {
            // SAFETY: an unnamed job with default security; the returned handle is owned by
            // this struct and closed exactly once, in `Drop`.
            let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
                .map_err(|err| io::Error::other(format!("CreateJobObjectW failed: {err}")))?;
            let job = Self { handle };

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `info` is a correctly sized, correctly typed structure for the
            // information class being set, and lives for the duration of the call.
            unsafe {
                SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                        .unwrap_or(u32::MAX),
                )
            }
            .map_err(|err| io::Error::other(format!("SetInformationJobObject failed: {err}")))?;

            Ok(job)
        }

        /// Put a process, and therefore everything it goes on to start, into the job.
        pub(crate) fn assign(&self, process: std::os::windows::io::RawHandle) -> io::Result<()> {
            // SAFETY: `process` is the live process handle `portable-pty` holds for the
            // child; the job handle is owned by `self`.
            unsafe { AssignProcessToJobObject(self.handle, HANDLE(process)) }
                .map_err(|err| io::Error::other(format!("AssignProcessToJobObject: {err}")))
        }

        /// Terminate every process in the job.
        pub(crate) fn terminate(&self) -> io::Result<()> {
            // SAFETY: the job handle is owned by `self` and still open.
            unsafe { TerminateJobObject(self.handle, TERMINATED_EXIT_CODE) }
                .map_err(|err| io::Error::other(format!("TerminateJobObject failed: {err}")))
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            // SAFETY: the handle was created by `CreateJobObjectW` and is closed once.
            // Closing the last handle is what kills any survivors.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_impl::{SIGKILL, SIGTERM, all_gone, collect_tree, signal_group, signal_pids};

#[cfg(unix)]
mod unix_impl {
    use std::collections::BTreeSet;
    use std::io;

    /// `SIGTERM`, re-exported so the session module does not have to name `libc` itself.
    pub(crate) const SIGTERM: i32 = libc::SIGTERM;

    /// `SIGKILL`, for when the grace period ran out.
    pub(crate) const SIGKILL: i32 = libc::SIGKILL;

    /// One row of the machine's process table, reduced to what a sweep needs.
    #[derive(Debug, Clone, Copy)]
    struct Process {
        /// The process id.
        pid: i32,
        /// Its parent, or zero when that could not be read.
        ppid: i32,
        /// Its session id, where the kernel will report one to us.
        session: Option<i32>,
    }

    /// Send a signal to the whole process group led by `pgid`.
    ///
    /// The child is a session leader thanks to `portable-pty`'s `setsid`, so its pid is
    /// also its process-group id.
    pub(crate) fn signal_group(pgid: u32, signal: i32) -> io::Result<()> {
        let pgid = i32::try_from(pgid)
            .map_err(|_| io::Error::other(format!("{pgid} is not a valid pgid")))?;
        // SAFETY: `killpg` takes two integers and has no memory effects. A group that has
        // already gone away reports `ESRCH`, which is handled below rather than ignored.
        let result = unsafe { libc::killpg(pgid, signal) };
        if result == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // The group is already gone, which is the outcome the caller wanted.
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(err)
    }

    /// Every process belonging to the session led by `leader`, including `leader` itself.
    ///
    /// Collected by both relations described in the module docs, because neither alone
    /// covers both platforms. Call this while the leader is still alive: after it dies its
    /// children reparent away and the parent relation reports nothing.
    ///
    /// The caller's own pid is never included. The daemon is not inside a session one of
    /// its children created with `setsid`, and a sweep able to reach its own pid would be
    /// an expensive way to find that out.
    pub(crate) fn collect_tree(leader: u32) -> Vec<i32> {
        let Ok(leader) = i32::try_from(leader) else {
            return Vec::new();
        };
        if leader <= 1 {
            return Vec::new();
        }
        let mut wanted = BTreeSet::from([leader]);

        if let Some(table) = process_table() {
            for entry in &table {
                if entry.session == Some(leader) {
                    wanted.insert(entry.pid);
                }
            }
            // Transitive descendants. The table is a snapshot, so this terminates: each
            // pass can only add pids that are in it, and it stops when a pass adds none.
            loop {
                let before = wanted.len();
                for entry in &table {
                    if entry.ppid > 1 && wanted.contains(&entry.ppid) {
                        wanted.insert(entry.pid);
                    }
                }
                if wanted.len() == before {
                    break;
                }
            }
        }

        // SAFETY: `getpid` takes no arguments and cannot fail.
        let own = unsafe { libc::getpid() };
        wanted.remove(&own);
        wanted.into_iter().filter(|pid| *pid > 1).collect()
    }

    /// Signal every pid in `pids`, returning how many accepted it.
    pub(crate) fn signal_pids(pids: &[i32], signal: i32) -> usize {
        pids.iter()
            .filter(|pid| {
                // SAFETY: `kill` takes two integers and has no memory effects. A pid that
                // has gone away reports `ESRCH`, which is the outcome the caller wanted.
                unsafe { libc::kill(**pid, signal) == 0 }
            })
            .count()
    }

    /// Whether none of `pids` is still alive.
    ///
    /// A process that has exited but not yet been reaped still answers this, so the caller
    /// should not include a pid whose parent is a thread of its own — see the session
    /// module, which leaves the leader out for exactly that reason.
    pub(crate) fn all_gone(pids: &[i32]) -> bool {
        // SAFETY: signal 0 delivers nothing and only performs the existence check.
        !pids.iter().any(|pid| unsafe { libc::kill(*pid, 0) == 0 })
    }

    /// The session id of `pid`, or `None` when the kernel will not say.
    ///
    /// Only Darwin needs this: Linux reads the session id straight out of
    /// `/proc/<pid>/stat`, and the fallback arm reports no table at all.
    ///
    /// `ESRCH` is ordinary churn in a full-machine sweep — a pid can exit between being
    /// enumerated and being asked about, and a zombie is not findable at all — so a failure
    /// here skips that row rather than abandoning the walk.
    #[cfg(target_vendor = "apple")]
    fn session_of(pid: i32) -> Option<i32> {
        // SAFETY: `getsid` takes one integer; a failure reports -1.
        let sid = unsafe { libc::getsid(pid) };
        (sid > 0).then_some(sid)
    }

    /// The machine's process table, or `None` where it cannot be read.
    ///
    /// `/proc/<pid>/stat` reports `pid (comm) state ppid pgrp session ...`. `comm` is the
    /// executable name unescaped, so it can contain both spaces and parentheses; splitting
    /// after the **last** `)` is the only way to find the fields that follow it.
    #[cfg(target_os = "linux")]
    fn process_table() -> Option<Vec<Process>> {
        let mut rows = Vec::new();
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some((_, after_comm)) = stat.rsplit_once(')') else {
                continue;
            };
            let mut fields = after_comm.split_whitespace();
            let _state = fields.next();
            let ppid = fields
                .next()
                .and_then(|field| field.parse().ok())
                .unwrap_or(0);
            let _pgrp = fields.next();
            let session = fields.next().and_then(|field| field.parse().ok());
            rows.push(Process { pid, ppid, session });
        }
        Some(rows)
    }

    /// The machine's process table, or `None` where it cannot be read.
    #[cfg(target_vendor = "apple")]
    fn process_table() -> Option<Vec<Process>> {
        let rows = all_pids()?
            .into_iter()
            .filter(|pid| *pid > 0)
            .filter_map(|pid| {
                let ppid = parent_of(pid)?;
                Some(Process {
                    pid,
                    ppid,
                    session: session_of(pid),
                })
            })
            .collect();
        Some(rows)
    }

    /// The parent of `pid`, or `None` when it has gone away or belongs to another user.
    #[cfg(target_vendor = "apple")]
    fn parent_of(pid: i32) -> Option<i32> {
        // SAFETY: `proc_bsdshortinfo` is a plain struct of integers and `c_char` arrays, so
        // an all-zero bit pattern is a valid value to overwrite.
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(size_of::<libc::proc_bsdshortinfo>()).ok()?;
        // SAFETY: the buffer is exactly one `proc_bsdshortinfo` and `size` is its length in
        // bytes, which is what this flavour expects.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        i32::try_from(info.pbsi_ppid).ok()
    }

    /// Every pid on the machine, or `None` where that cannot be asked for.
    ///
    /// `proc_listallpids` with a null buffer reports how many processes there are; the
    /// second call reports how much it wrote. Both returns have been documented as a count
    /// and as a byte length over the years, so neither reading is trusted here: the buffer
    /// is over-allocated, zero-filled, and entries that are not positive pids are dropped,
    /// which is correct under either.
    #[cfg(target_vendor = "apple")]
    fn all_pids() -> Option<Vec<i32>> {
        // SAFETY: a null buffer of length zero is the documented way to ask for the size.
        let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return None;
        }
        let capacity = usize::try_from(needed).ok()?.saturating_mul(2).max(64);
        let mut pids = vec![0i32; capacity];
        let bytes = i32::try_from(capacity.saturating_mul(size_of::<i32>())).ok()?;
        // SAFETY: the buffer holds `capacity` `i32`s and `bytes` is its size in bytes.
        let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if written <= 0 {
            return None;
        }
        pids.retain(|pid| *pid > 0);
        Some(pids)
    }

    /// The machine's process table, or `None` where it cannot be read.
    ///
    /// Nysia ships on Windows and macOS; this arm exists so the crate still compiles on
    /// another Unix, and it deliberately reports "cannot enumerate" rather than "nothing is
    /// running", so the caller falls back to the process group alone.
    #[cfg(all(unix, not(target_os = "linux"), not(target_vendor = "apple")))]
    fn process_table() -> Option<Vec<Process>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_for_returns_as_soon_as_the_condition_holds() {
        let start = Instant::now();
        assert!(wait_for(Duration::from_secs(30), || true));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn wait_for_gives_up_at_the_deadline_rather_than_hanging() {
        let start = Instant::now();
        assert!(!wait_for(Duration::from_millis(80), || false));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(80), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    #[test]
    #[cfg(windows)]
    fn a_job_object_can_be_created_and_terminated_while_empty() {
        let job = JobObject::new().expect("a job object must be creatable");
        // Terminating an empty job is a no-op rather than an error, which is what makes
        // shutdown idempotent.
        job.terminate().expect("terminate on an empty job");
    }

    #[test]
    #[cfg(unix)]
    fn signalling_a_group_that_is_already_gone_is_not_an_error() {
        // A pgid this high cannot exist; the point is that `ESRCH` reads as success,
        // because "the group is gone" is what the caller asked for.
        assert!(signal_group(u32::MAX / 2, SIGTERM).is_ok());
        assert_eq!(signal_pids(&[i32::MAX / 2], SIGTERM), 0);
        assert!(all_gone(&[i32::MAX / 2]));
    }

    #[test]
    #[cfg(unix)]
    fn the_sweep_sees_a_live_process_and_never_includes_the_caller() {
        // Two properties, both of which the sweep would be dangerous without, and neither of
        // which is "the tree has the right contents" — that is the test below, which builds
        // a process it can name.
        // SAFETY: `getsid(0)` and `getpid` ask about the calling process and cannot fail.
        let (own_session, own_pid) = unsafe { (libc::getsid(0), libc::getpid()) };
        assert!(own_session > 0);

        let tree = collect_tree(u32::try_from(own_session).expect("a session id is positive"));
        assert!(
            !tree.contains(&own_pid),
            "the sweep must never include the caller"
        );
        assert!(
            !all_gone(&[own_pid]),
            "this process is plainly alive, so liveness detection is working"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_child_of_the_leader_is_collected_even_from_a_process_group_of_its_own() {
        // The macOS case in miniature, and the one `killpg` alone misses. The child calls
        // `setpgid` for itself before exec, so it really has left the caller's process
        // group: without that the test would have been asserting the easy case and calling
        // it the hard one.
        use std::os::unix::process::CommandExt;

        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: `setpgid` is async-signal-safe and touches no memory. Making the child
        // its own group leader is the whole point of the test.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().expect("/bin/sh must spawn");
        let child_pid = i32::try_from(child.id()).expect("a pid is positive");

        // SAFETY: `getpid` and `getpgid` cannot fail for the calling process.
        let own_pid = unsafe { libc::getpid() };
        // The child is in a group of its own, so a group-based sweep would miss it.
        // SAFETY: `getpgid` on a live child of ours.
        let child_group = unsafe { libc::getpgid(child_pid) };
        assert_eq!(
            child_group, child_pid,
            "the child should lead its own process group"
        );
        assert_ne!(
            child_group,
            unsafe { libc::getpgid(own_pid) },
            "and that group should not be the caller's"
        );

        let tree = collect_tree(u32::try_from(own_pid).expect("a pid is positive"));
        assert!(
            tree.contains(&child_pid),
            "the child {child_pid} must be in the collected tree {tree:?}"
        );

        assert_eq!(signal_pids(&[child_pid], SIGKILL), 1);
        let _ = child.wait();
    }
}
