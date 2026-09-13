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
//! `killpg` alone is not enough, though, and §7.1 already says why: macOS has no
//! `PR_SET_CHILD_SUBREAPER`, so the answer there is to **sweep by session id**. An
//! interactive shell with job control puts every `&` job in a process group of its *own*,
//! so `sleep 300 &` is not in the group `killpg` just signalled — but it is still in the
//! session the `setsid` created, and a session id is not reused while any member is alive.
//! The sweep is what actually reclaims it.
//!
//! Two consequences worth stating rather than discovering. An interactive `bash` ignores
//! `SIGTERM`, so on a Unix shell the grace period is normally burned in full before the
//! `SIGKILL` phase does the work. And enumerating a session needs a process listing, which
//! exists on Linux (`/proc`) and macOS (`proc_listallpids`) — the two Unix shapes Nysia
//! cares about — and nowhere else; elsewhere the code falls back to the process-group
//! check alone.

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
pub(crate) use unix_impl::{SIGKILL, SIGTERM, session_is_gone, signal_group, signal_session};

#[cfg(unix)]
mod unix_impl {
    use std::io;

    /// `SIGTERM`, re-exported so the session module does not have to name `libc` itself.
    pub(crate) const SIGTERM: i32 = libc::SIGTERM;

    /// `SIGKILL`, for when the grace period ran out.
    pub(crate) const SIGKILL: i32 = libc::SIGKILL;

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

    /// Signal every process in the session led by `sid`, including the jobs an interactive
    /// shell put into process groups of their own.
    ///
    /// Returns how many processes were signalled, which is zero both when the session is
    /// empty and when this platform cannot enumerate one.
    pub(crate) fn signal_session(sid: u32, signal: i32) -> usize {
        let Some(pids) = pids_in_session(sid) else {
            return 0;
        };
        pids.into_iter()
            .filter(|pid| {
                // SAFETY: `kill` takes two integers and has no memory effects.
                unsafe { libc::kill(*pid, signal) == 0 }
            })
            .count()
    }

    /// Whether nothing is left of the session led by `sid`.
    ///
    /// Falls back to the process-group check on a platform that cannot list processes, so
    /// that the grace period still means something there.
    pub(crate) fn session_is_gone(sid: u32) -> bool {
        match pids_in_session(sid) {
            Some(pids) => pids.is_empty(),
            None => group_is_gone(sid),
        }
    }

    /// Whether no process is left in the group led by `pgid`.
    ///
    /// Signal zero performs the existence and permission checks without delivering
    /// anything, which is the cheap way to poll a grace period.
    fn group_is_gone(pgid: u32) -> bool {
        let Ok(pgid) = i32::try_from(pgid) else {
            return true;
        };
        // SAFETY: as above; signal 0 delivers nothing.
        let result = unsafe { libc::killpg(pgid, 0) };
        result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    /// Every live pid in session `sid`, or `None` where processes cannot be enumerated.
    ///
    /// The calling process is excluded on principle: the daemon is never inside a session
    /// one of its children created with `setsid`, and a sweep that could reach its own pid
    /// would be an expensive way to discover otherwise.
    fn pids_in_session(sid: u32) -> Option<Vec<i32>> {
        let Ok(sid) = i32::try_from(sid) else {
            return Some(Vec::new());
        };
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let own = unsafe { libc::getpid() };
        let members = all_pids()?
            .into_iter()
            .filter(|pid| *pid > 0 && *pid != own && session_of(*pid) == Some(sid))
            .collect();
        Some(members)
    }

    /// The session id of `pid`, or `None` if it has gone away.
    fn session_of(pid: i32) -> Option<i32> {
        // SAFETY: `getsid` takes one integer; a dead pid reports `ESRCH` as -1.
        let sid = unsafe { libc::getsid(pid) };
        (sid > 0).then_some(sid)
    }

    /// Every pid on the machine, or `None` where that cannot be asked for.
    #[cfg(target_os = "linux")]
    fn all_pids() -> Option<Vec<i32>> {
        let entries = std::fs::read_dir("/proc").ok()?;
        Some(
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
                .collect(),
        )
    }

    /// Every pid on the machine, or `None` where that cannot be asked for.
    #[cfg(target_vendor = "apple")]
    fn all_pids() -> Option<Vec<i32>> {
        // A first call with a null buffer reports the size needed, in bytes. The count can
        // grow between the two calls, so the buffer is over-allocated and the result is
        // truncated to what the second call actually wrote.
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
        let count = usize::try_from(written).ok()?.min(capacity);
        pids.truncate(count);
        Some(pids)
    }

    /// Every pid on the machine, or `None` where that cannot be asked for.
    ///
    /// Nysia ships on Windows and macOS; this arm exists so the crate still compiles on
    /// another Unix, and it deliberately reports "cannot enumerate" rather than "nothing is
    /// running".
    #[cfg(all(unix, not(target_os = "linux"), not(target_vendor = "apple")))]
    fn all_pids() -> Option<Vec<i32>> {
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
        // because "the tree is gone" is what the caller asked for.
        assert!(signal_group(u32::MAX / 2, SIGTERM).is_ok());
        assert_eq!(signal_session(u32::MAX / 2, SIGTERM), 0);
        assert!(session_is_gone(u32::MAX / 2));
    }

    #[test]
    #[cfg(unix)]
    fn a_live_session_is_enumerable_and_does_not_read_as_gone() {
        // The sweep is only worth anything if it can see a live session; this process is
        // in one, so that is the one to check against.
        // SAFETY: `getsid(0)` asks about the calling process and cannot fail.
        let own = unsafe { libc::getsid(0) };
        assert!(own > 0);
        let sid = u32::try_from(own).expect("a session id is positive");
        assert!(
            !session_is_gone(sid),
            "the caller's own session must not read as empty"
        );
    }
}
