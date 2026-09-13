//! Who is on the other end of the socket, proved by the kernel.
//!
//! §3.2 is the part of Nysia that is deliberately better than the system it learns from.
//! Orca proves a calling agent's identity by **hook echo**: the CLI presents a launch token
//! and a hook listener must independently attest that the same token has been posting from
//! that pane. It is indirect, it needs a round trip, and it breaks on legitimate cases.
//!
//! Nysia proves it **directly**, because the daemon spawned the process. Two separate
//! questions, and conflating them is the mistake this module exists to prevent:
//!
//! 1. **May you talk to me at all?** — [`PeerCredentials::authorize`]. The kernel says which
//!    account the peer runs as; anything that is not this daemon's own account is refused at
//!    the handshake. This is the security boundary, and it is the *only* one.
//! 2. **Which session are you speaking from?** — [`ancestry`]. Walk the caller's pid up its
//!    process tree; if it passes through a session leader this daemon spawned, the caller is
//!    that session's agent and the pane identity follows (§3.3). This is *identification*,
//!    not authorisation: the GUI and a human's `nysia session list` are nobody's descendant
//!    and must still work.
//!
//! Environment variables stay a **hint** for speed and are never the proof — nothing in this
//! module reads one. A peer can set any variable it likes; it cannot choose the uid the
//! kernel reports for its socket, or the parent the kernel recorded when it was forked.

use std::collections::HashMap;
use std::fmt;

use nysia_proto::{Incarnation, SessionHandle};

/// How far up a process tree the ancestry walk will look before giving up.
///
/// A bound rather than a `while` loop, because a corrupted or racing process table can
/// present a cycle — a pid whose parent is itself, or two pids that name each other after
/// reuse — and an unbounded walk over one hangs the connection that asked.
const MAX_ANCESTRY_DEPTH: usize = 64;

/// Why the peer could not be identified.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// The kernel would not say who the peer is.
    #[error("could not read peer credentials from the kernel: {0}")]
    Credentials(#[source] std::io::Error),
    /// The platform cannot answer this question, so nothing may be assumed from it.
    #[error("peer credentials are not available on this platform")]
    Unsupported,
}

/// What the kernel says about the process at the other end.
///
/// Every field comes from the kernel and none from the frame. A peer cannot present a
/// different uid any more than it can present a different parent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCredentials {
    /// The peer's process id, where the platform reports one.
    ///
    /// `None` is possible — a Unix that does not carry the pid in its peer credentials —
    /// and it degrades identification, never authorisation: the uid check is what guards the
    /// door, and it does not need a pid.
    pub pid: Option<u32>,
    /// The peer's user, spelled the way this platform spells one: a numeric uid on Unix, a
    /// SID string on Windows.
    pub user: String,
    /// This daemon's own user, for the comparison [`PeerCredentials::authorize`] makes.
    pub daemon_user: String,
}

impl PeerCredentials {
    /// Whether the peer runs as the account this daemon does.
    ///
    /// The whole authorisation decision, and deliberately the whole of it. Everything the
    /// daemon owns — the PTYs, the scrollback, the store — is already reachable by anything
    /// running as this user, so a check finer than "same account" would protect nothing it
    /// does not already have, while a check coarser than this would hand another account a
    /// shell.
    #[must_use]
    pub fn authorize(&self) -> bool {
        !self.user.is_empty() && self.user == self.daemon_user
    }

    /// What to tell a refused peer. Never the credentials themselves.
    #[must_use]
    pub fn refusal_detail(&self) -> String {
        "the connecting process does not run as the account this daemon serves".to_owned()
    }
}

impl fmt::Display for PeerCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pid {
            Some(pid) => write!(f, "pid {pid} as {}", self.user),
            None => write!(f, "unknown pid as {}", self.user),
        }
    }
}

/// The session a caller turned out to be speaking from.
///
/// Produced by [`ancestry`], consumed by anything that wants to attribute a request to a
/// pane rather than to a connection. Absent for the GUI and for a human at a shell, which is
/// the normal case and not a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerSession {
    /// The session the caller descends from.
    pub handle: SessionHandle,
    /// That session's incarnation, which is what status is attributed to (§3.3).
    pub incarnation: Incarnation,
    /// How many process hops separated the caller from the session leader. Zero means the
    /// caller *is* the leader.
    pub depth: usize,
}

/// Walk `pid` up its process tree until it reaches a session this daemon owns.
///
/// `leaders` maps a session leader's pid to the session it leads — the daemon spawned every
/// one of them, which is exactly why this works and why no token has to be echoed.
///
/// Returns `None` when the caller descends from nothing the daemon owns. That is the
/// expected answer for the window and for a person typing `nysia session list`, so it is an
/// `Option` rather than an error.
#[must_use]
pub fn ancestry(
    pid: u32,
    leaders: &HashMap<u32, (SessionHandle, Incarnation)>,
) -> Option<CallerSession> {
    let mut seen = Vec::with_capacity(MAX_ANCESTRY_DEPTH);
    let mut current = pid;
    for depth in 0..MAX_ANCESTRY_DEPTH {
        if let Some((handle, incarnation)) = leaders.get(&current) {
            return Some(CallerSession {
                handle: handle.clone(),
                incarnation: incarnation.clone(),
                depth,
            });
        }
        // A process table can present a cycle after pid reuse. Walking one forever is a hung
        // connection, so the walk remembers where it has been.
        if seen.contains(&current) {
            return None;
        }
        seen.push(current);
        match parent_of(current) {
            Some(parent) if parent != 0 && parent != current => current = parent,
            _ => return None,
        }
    }
    None
}

/// The parent process id of `pid`, or `None` when the kernel will not say.
///
/// Platform-specific and deliberately narrow: this is the only thing the ancestry walk needs
/// from the process table, and asking for less keeps the unsafe surface to one call per
/// platform.
#[must_use]
pub fn parent_of(pid: u32) -> Option<u32> {
    platform::parent_of(pid)
}

/// Read the peer credentials of an accepted connection.
///
/// # Errors
///
/// Returns [`PeerError`] when the kernel refuses to answer. A daemon that cannot learn who
/// its peer is must refuse the connection rather than serve it, because the uid check is the
/// only thing standing between another account and a shell.
#[cfg(unix)]
pub fn credentials_of(stream: &tokio::net::UnixStream) -> Result<PeerCredentials, PeerError> {
    let cred = stream.peer_cred().map_err(PeerError::Credentials)?;
    Ok(PeerCredentials {
        pid: cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        user: cred.uid().to_string(),
        daemon_user: platform::daemon_user(),
    })
}

/// Read the peer credentials of a connected named-pipe instance.
///
/// # Errors
///
/// As the Unix half: a peer the kernel will not name is a peer the daemon refuses.
#[cfg(windows)]
pub fn credentials_of(
    pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
) -> Result<PeerCredentials, PeerError> {
    platform::pipe_credentials(pipe)
}

#[cfg(unix)]
mod platform {
    /// The account this daemon runs as, as a uid.
    pub(super) fn daemon_user() -> String {
        // SAFETY: `geteuid` reads the calling process's own effective uid. It takes no
        // arguments, touches no memory the caller owns, and cannot fail.
        unsafe { libc::geteuid() }.to_string()
    }

    #[cfg(target_os = "linux")]
    pub(super) fn parent_of(pid: u32) -> Option<u32> {
        // `/proc/<pid>/stat` rather than a sysctl: it needs no unsafe at all. The comm field
        // is parenthesised and may itself contain spaces and parentheses, so the split is
        // after the *last* `)`, never on whitespace from the start.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail = stat.rsplit_once(')')?.1;
        tail.split_whitespace().nth(1)?.parse().ok()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn parent_of(pid: u32) -> Option<u32> {
        use std::mem::size_of;

        let pid = i32::try_from(pid).ok()?;
        let mut info = std::mem::MaybeUninit::<libc::kinfo_proc>::zeroed();
        let mut size = size_of::<libc::kinfo_proc>();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];

        // SAFETY: `mib` is a four-element array and the length passed matches it; the output
        // buffer is a correctly sized, zeroed `kinfo_proc` and `size` is its byte length, so
        // the kernel cannot write past it. The new-value pointer is null with a zero length,
        // which is how `sysctl` is told this is a read.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                info.as_mut_ptr().cast::<libc::c_void>(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        // A short answer means the kernel did not fill the record — most often because the
        // process exited between the connect and this call — and reading `e_ppid` out of a
        // partly written struct would invent an ancestor.
        if rc != 0 || size < size_of::<libc::kinfo_proc>() {
            return None;
        }
        // SAFETY: `sysctl` returned success and filled the whole record, so it is initialised.
        let info = unsafe { info.assume_init() };
        u32::try_from(info.kp_eproc.e_ppid).ok()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn parent_of(_pid: u32) -> Option<u32> {
        // Identification degrades to "unknown", which is a supported state: the uid check is
        // what guards the door, and it works everywhere.
        None
    }
}

#[cfg(windows)]
mod platform {
    use super::{PeerCredentials, PeerError};

    use tokio::net::windows::named_pipe::NamedPipeServer;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetLengthSid, GetTokenInformation, IsValidSid, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// A `HANDLE` that closes itself, so every early return below cannot leak one.
    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: the handle came from `OpenProcess` or `OpenProcessToken`, is owned
                // by this value, and is closed exactly once because `Drop` runs once.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// The SID of the account this daemon runs as.
    pub(super) fn daemon_user() -> String {
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing and is
        // valid for the life of the process.
        let process = unsafe { GetCurrentProcess() };
        token_user_sid(process).unwrap_or_default()
    }

    /// The credentials of the process on the other end of a connected pipe instance.
    pub(super) fn pipe_credentials(pipe: &NamedPipeServer) -> Result<PeerCredentials, PeerError> {
        use std::os::windows::io::AsRawHandle;

        let handle = HANDLE(pipe.as_raw_handle());
        let mut pid: u32 = 0;
        // SAFETY: `handle` is the connected server end of the pipe, borrowed from `pipe` for
        // the duration of this call, and `pid` is a live local the kernel writes one u32 to.
        unsafe { GetNamedPipeClientProcessId(handle, &raw mut pid) }
            .map_err(|err| PeerError::Credentials(std::io::Error::other(err)))?;

        // A pid alone says nothing about *who* is on the other end — §3.2 wants the account,
        // and a pid without one would be an identity check that authorises everybody.
        let user = process_user_sid(pid).unwrap_or_default();
        Ok(PeerCredentials {
            pid: Some(pid),
            user,
            daemon_user: daemon_user(),
        })
    }

    /// The SID of the user owning `pid`.
    fn process_user_sid(pid: u32) -> Option<String> {
        // `PROCESS_QUERY_LIMITED_INFORMATION` rather than `PROCESS_QUERY_INFORMATION`: it is
        // enough to open the token for a read and is granted in cases the wider right is not.
        //
        // SAFETY: the arguments are plain values; the call returns a handle or an error and
        // writes nothing through a pointer.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let process = OwnedHandle(process);
        token_user_sid(process.0)
    }

    /// The SID string of `process`'s token user.
    fn token_user_sid(process: HANDLE) -> Option<String> {
        let mut token = HANDLE::default();
        // SAFETY: `process` is a live process handle borrowed for this call and `token` is a
        // live local the kernel writes one handle to.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }.ok()?;
        let token = OwnedHandle(token);

        // Two calls, because a `TOKEN_USER` is a fixed header followed by a variable-length
        // SID: the first asks how long that is, the second fills a buffer of that length.
        let mut needed: u32 = 0;
        // SAFETY: a null buffer with a zero length is how `GetTokenInformation` is asked for
        // the required size; it writes only `needed` and returns an error, which is expected.
        let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &raw mut needed) };
        if needed == 0 {
            return None;
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: `buffer` is `needed` bytes long, which is exactly what the probing call
        // said the kernel needs, and the length passed matches the allocation.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                needed,
                &raw mut needed,
            )
        }
        .ok()?;

        // SAFETY: on success the kernel wrote a `TOKEN_USER` at the head of `buffer`.
        // `read_unaligned` makes no alignment claim about the allocation, which is what keeps
        // this sound whatever the `Vec<u8>` happened to be aligned to. `buffer` is still live
        // and still owns the SID the copied header points at.
        let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read_unaligned() };
        let sid = sid_to_hex(user.User.Sid);
        drop(buffer);
        sid
    }

    /// Render a SID as the hex of its own bytes, which is what comparison happens on.
    ///
    /// Not `S-1-5-…`: `ConvertSidToStringSidW` lives behind the `Win32_Security_Authorization`
    /// feature, which the workspace dependency table does not enable and which is
    /// coordinator-owned. Byte equality is the same test — a SID's binary form is canonical,
    /// which is exactly why `EqualSid` is a `memcmp` with a length check — and an owned
    /// `String` carries no lifetime, so the daemon's own SID can be read once at startup and
    /// kept without keeping its buffer alive alongside it.
    fn sid_to_hex(sid: PSID) -> Option<String> {
        if sid.is_invalid() {
            return None;
        }
        // SAFETY: `sid` points into a buffer the caller still owns. `IsValidSid` reads the
        // revision and sub-authority count and is the documented way to check a SID before
        // asking for its length.
        if !unsafe { IsValidSid(sid) }.as_bool() {
            return None;
        }
        // SAFETY: as above, and the SID has just been validated, so its length is readable.
        let length = unsafe { GetLengthSid(sid) } as usize;
        if length == 0 {
            return None;
        }
        // SAFETY: the SID is valid and `length` is the length the kernel just reported for
        // it, so the whole range is inside the caller's buffer.
        let bytes = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), length) };
        Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// The parent process id of `pid`, from a process-table snapshot.
    pub(super) fn parent_of(pid: u32) -> Option<u32> {
        // SAFETY: the flags are valid and the second argument is ignored for a process
        // snapshot. The returned handle is owned below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
        let snapshot = OwnedHandle(snapshot);

        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).ok()?,
            ..Default::default()
        };
        // SAFETY: `snapshot` is a live snapshot handle and `entry` is a live local whose
        // `dwSize` has been set, which is what the API requires before the first read.
        if unsafe { Process32FirstW(snapshot.0, &raw mut entry) }.is_err() {
            return None;
        }
        loop {
            if entry.th32ProcessID == pid {
                return Some(entry.th32ParentProcessID);
            }
            // SAFETY: as above; the snapshot is still live and `entry` is still a live local.
            if unsafe { Process32NextW(snapshot.0, &raw mut entry) }.is_err() {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nysia_proto::PaneKey;

    fn leader(pid: u32) -> HashMap<u32, (SessionHandle, Incarnation)> {
        let pane = PaneKey::new("tab_1", "leaf_1").expect("a well-formed pane key");
        HashMap::from([(pid, (SessionHandle::generate(), Incarnation::new(&pane, 0)))])
    }

    #[test]
    fn a_peer_from_another_account_is_refused_and_one_from_this_account_is_not() {
        let mine = PeerCredentials {
            pid: Some(7),
            user: "1000".to_owned(),
            daemon_user: "1000".to_owned(),
        };
        assert!(mine.authorize());

        let theirs = PeerCredentials {
            user: "1001".to_owned(),
            ..mine.clone()
        };
        assert!(!theirs.authorize());

        // An unknown user is refused rather than treated as a match. "The kernel would not
        // say" and "the kernel said it is you" must never collapse into one answer.
        let unknown = PeerCredentials {
            user: String::new(),
            daemon_user: String::new(),
            ..mine
        };
        assert!(!unknown.authorize());
    }

    #[test]
    fn a_refusal_never_repeats_the_credentials_back() {
        let refused = PeerCredentials {
            pid: Some(4242),
            user: "S-1-5-21-secret".to_owned(),
            daemon_user: "S-1-5-18".to_owned(),
        };
        let detail = refused.refusal_detail();
        assert!(!detail.contains("S-1-5-21-secret"));
        assert!(!detail.contains("4242"));
    }

    #[test]
    fn the_caller_that_is_the_session_leader_is_found_at_depth_zero() {
        let leaders = leader(1000);
        let found = ancestry(1000, &leaders).expect("the leader is its own ancestor");
        assert_eq!(found.depth, 0);
    }

    #[test]
    fn a_caller_that_descends_from_nothing_owned_is_not_an_error() {
        // The window and a human's shell are nobody's descendant. Identification returning
        // nothing is the normal case, which is why it is an `Option` and not a refusal.
        assert!(ancestry(std::process::id(), &HashMap::new()).is_none());
    }

    #[test]
    fn this_process_has_a_parent_the_kernel_will_name() {
        // The one place the platform code is exercised for real: if `parent_of` is wrong,
        // ancestry silently never matches and §3.2's whole claim quietly stops being true.
        let parent = parent_of(std::process::id());
        assert!(
            parent.is_some_and(|pid| pid != 0),
            "the test runner was started by something, got {parent:?}"
        );
    }

    #[test]
    fn the_walk_reaches_a_real_ancestor_of_this_process() {
        let parent = parent_of(std::process::id()).expect("this process has a parent");
        let found = ancestry(std::process::id(), &leader(parent)).expect("the parent is owned");
        assert_eq!(found.depth, 1);
    }
}
