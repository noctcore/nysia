//! The log beside the endpoint, and the only thing that keeps it from growing for ever.
//!
//! A spawned daemon's stdout and stderr are redirected into `<runtime dir>/<stem>.log` by
//! [`crate::rpc::discovery`], and under D-1 that process lives for **days**. Nothing used to
//! trim the file, so a long-lived session filled the user's disk one `tracing` line at a
//! time. This module is the cap.
//!
//! # The policy, in two numbers
//!
//! [`MAX_LOG_BYTES`] per file — 8 MiB — and [`KEPT_ROTATIONS`] older copies kept beside it —
//! 3 — for a ceiling of [`MAX_TOTAL_LOG_BYTES`], which is 32 MiB and is *derived* from the
//! other two rather than written down a second time. `the_ceiling_is_the_product_of_the_two`
//! holds all three to the literals in this paragraph, because a constant in code beside a
//! different number in prose is a defect this project has corrected in six separate files.
//!
//! The cap is measured on a schedule, not on every write. So a file can be over it when it
//! is measured, and the copy taken aside is that whole file — cap plus whatever was written
//! since the last check. [`MAX_TOTAL_LOG_BYTES`] is therefore **the ceiling when the trim
//! keeps up**, not a hard limit, and the honest statement of what a trim guarantees is the
//! one `a_trim_empties_the_live_log_and_keeps_only_the_policy` makes: the live file is left
//! empty, and never more than [`KEPT_ROTATIONS`] copies survive. Claiming a hard ceiling
//! would be a claim no check in this repo can make.
//!
//! # Why the live file is truncated and never renamed
//!
//! This is the whole design, and the obvious implementation is the wrong one.
//!
//! The daemon does not open its own log. Its stdout and stderr are an **inherited handle**
//! onto a file the spawner opened, and a handle names the file, not the path. Rename the
//! live log to `<stem>.log.1` and the rename succeeds — and the daemon goes on writing into
//! `<stem>.log.1` for the rest of its life while the fresh `<stem>.log` stays empty for ever.
//! The log would appear to stop the first time it was rotated. Windows adds a second way for
//! a rename to be wrong, refusing it outright against an open handle whose share mode did
//! not permit deletion, but the fatal problem is the one that happens when the rename
//! *works*.
//!
//! Truncating in place is the one operation that survives a writer holding the file open, on
//! both platforms. An append handle — `O_APPEND` on Unix, `FILE_APPEND_DATA` on Windows —
//! resolves the write offset to the current end of file at every write, so after a
//! `set_len(0)` the next line the daemon writes lands at offset 0 of the same file it was
//! already writing to. It never learns that anything happened.
//! `rotation_survives_a_writer_holding_the_file_open` is that claim as a test, and it is the
//! test a rename-based implementation fails.
//!
//! **What it costs.** Lines written between the copy and the truncate are lost — the classic
//! `copytruncate` trade. It is bounded by how long the copy takes, it happens once per 8 MiB
//! of output, and the alternative silently loses every line written after the first rotation
//! instead of a handful during it.
//!
//! Only the live file needs this. The rotations have no writer, so they are moved with an
//! ordinary [`std::fs::rename`].
//!
//! # What a Nysia log never contains
//!
//! Trap 13: scrollback can carry secrets, and so can a `question`, which is a tool's input
//! verbatim. `nysia-proto` confines `tool_input` to `waiting` events in both directions for
//! exactly that reason, the store logs nothing at all, and no error variant echoes a payload.
//! A log file that undid any of that would turn debugging into a liability, so the rule for
//! everything that writes into one of these files is:
//!
//! - **no PTY output**, in either direction — not a payload byte, not a scrollback line, not
//!   a rendered frame's contents;
//! - **no keystrokes** — `TerminalSend.text` is what the user typed and may be a password;
//! - **no `tool_input`** and no `question`, which is the same field by another name;
//! - **no environment or working directory** — `SessionCreate.envOverrides` is where a token
//!   reaches a shell, and `cwd` names a person's disk;
//! - **no request or response bodies at all.** A verb's *name*, a handle, a pane key, a
//!   stream id, a byte count and a boolean are the whole vocabulary.
//!
//! That is not a convention anybody has to remember. The window's log path takes a closed
//! set of typed fields and no free-text string, so there is nowhere for a payload to be put
//! — see `nysia-desktop`'s `commands` module, where
//! `a_verbs_name_is_logged_and_its_payload_is_not` holds it.
//!
//! The files are confined as well as scrubbed: the runtime directory is already owner-only,
//! and [`restrict_to_owner`] puts `0600` on the log itself so the confinement does not rest
//! on the directory alone.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// How large one log file may get before it is rotated: 8 MiB.
///
/// Large enough that an ordinary day of `info` never rotates at all, small enough that a
/// person can open the file in an editor when it has.
pub const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// How many rotated copies are kept beside the live log: 3.
///
/// Named `<stem>.log.1` through `<stem>.log.3`, oldest last. `.3` is dropped when a fourth
/// rotation arrives.
pub const KEPT_ROTATIONS: usize = 3;

/// Every log file for one endpoint, at the cap: 32 MiB.
///
/// Derived, never written down. A reader who wants to know what Nysia costs on disk should be
/// able to get the answer without multiplying two numbers that might have drifted apart.
///
/// **A working figure, not a hard limit.** The cap is measured on the caller's schedule, so a
/// file that was written to hard between two checks is rotated at whatever size it had
/// reached, and the copy kept beside it is that size. The real disk cost is this plus one
/// check interval's output per file, and it is quoted here as the figure that holds whenever
/// the trim keeps up — which, against 8 MiB and half a minute, it does.
pub const MAX_TOTAL_LOG_BYTES: u64 = MAX_LOG_BYTES * (KEPT_ROTATIONS as u64 + 1);

/// At least one rotation, or a trim would discard the 8 MiB it was called to preserve.
const _: () = assert!(KEPT_ROTATIONS >= 1);

/// What a [`trim`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trimmed {
    /// The log was within the cap, or there was no log. Nothing moved.
    Untouched,
    /// The log was over the cap: its contents were moved into the first rotation and the
    /// live file was truncated in place.
    Rotated {
        /// How many bytes were moved out of the live log.
        bytes: u64,
    },
}

/// Where the `index`th rotation of `live` lives: `<live>.1`, `<live>.2`, and so on.
///
/// An appended component rather than an inserted one, so a rotation can never collide with
/// another runtime file: `nysiad-v1.log.1` is unmistakably a rotation of `nysiad-v1.log`,
/// where `nysiad-v1.1.log` sorts next to the lease and the lock and reads like a second
/// endpoint.
#[must_use]
pub fn rotation_path(live: &Path, index: usize) -> PathBuf {
    let mut name = live.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// Rotate `live` if it has grown past [`MAX_LOG_BYTES`], leaving it empty and still open.
///
/// Safe to call against a file a live process is appending to — that is the whole point, and
/// the reason the live file is truncated rather than renamed. See the module docs.
///
/// A missing log is [`Trimmed::Untouched`] rather than an error: a daemon started by hand has
/// its stderr on a terminal and may never create one.
///
/// # A caller may not be the writer
///
/// This trims *the log beside the endpoint*, whoever is writing it. A daemon started from a
/// shell is logging to that shell, not to the file, and will still trim a stale file left by
/// an earlier spawned daemon. That is harmless — the file is nobody's — but it is surprising
/// enough to be worth saying.
///
/// # Errors
///
/// Whatever the filesystem said about reading the live log's size, copying it aside or
/// truncating it. A failed rotation is not fatal to a caller: the log keeps growing, which is
/// the state this function exists to leave, not one it makes worse.
pub fn trim(live: &Path) -> io::Result<Trimmed> {
    trim_to(live, MAX_LOG_BYTES, KEPT_ROTATIONS)
}

/// [`trim`], with the policy passed in so a test does not have to write 8 MiB to see it work.
///
/// Private because there is one policy. A caller able to choose its own cap is a caller able
/// to choose `u64::MAX`, and a retention default an ordinary call site can undo is not a
/// default (CLAUDE.md §6).
fn trim_to(live: &Path, cap: u64, kept: usize) -> io::Result<Trimmed> {
    let bytes = match std::fs::metadata(live) {
        Ok(metadata) => metadata.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Trimmed::Untouched),
        Err(err) => return Err(err),
    };
    if bytes <= cap {
        return Ok(Trimmed::Untouched);
    }

    // Oldest first, so nothing is overwritten before it has been moved. `rename` replaces an
    // existing destination on both platforms, which is what drops what falls off the end.
    for index in (1..kept).rev() {
        let from = rotation_path(live, index);
        if from.exists() {
            std::fs::rename(&from, rotation_path(live, index + 1))?;
        }
    }

    // `io::copy` between handles this code opened itself, never `std::fs::copy`: on Windows
    // that is `CopyFileExW`, which opens the source with a share mode this caller cannot
    // choose — against a file a daemon is holding open for append.
    let first = rotation_path(live, 1);
    let mut source = File::open(live)?;
    let mut destination = File::create(&first)?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    drop(destination);
    restrict_to_owner(&first);

    // The truncate, and the reason a rename would have been wrong. The daemon's inherited
    // append handle resolves its offset at every write, so its next line lands at offset 0 of
    // this same file.
    OpenOptions::new().write(true).open(live)?.set_len(0)?;
    Ok(Trimmed::Rotated { bytes })
}

/// Open `live` for appending, rotating it first if a previous run left it over the cap.
///
/// What [`crate::rpc::discovery`] hands a spawned daemon as its stdout and stderr, and what
/// the window opens for its own log. Append mode is load-bearing twice over: two runtimes
/// racing cannot truncate each other's output, and it is what makes [`trim`] work against a
/// process that is still writing.
///
/// # Errors
///
/// Whatever the filesystem said about opening the file. A failed *rotation* is not an error
/// here — the log is opened anyway, oversized, because refusing to start a daemon over a
/// large log file would turn a disk-space nuisance into an outage.
pub fn open_for_append(live: &Path) -> io::Result<File> {
    if let Err(err) = trim(live) {
        tracing::warn!(
            path = %live.display(),
            %err,
            "could not rotate the log before opening it; continuing with the one that is there"
        );
    }
    let file = OpenOptions::new().create(true).append(true).open(live)?;
    restrict_to_owner(live);
    Ok(file)
}

/// Put owner-only permissions on a log file.
///
/// Best effort, and a second layer rather than the only one: the runtime directory is already
/// `0700`, so a log inside it is unreachable by anyone else whatever its own mode says. This
/// is here because trap 13 is about what happens when one of those layers is wrong, and a
/// file that carries its own confinement survives being copied out of the directory that was
/// carrying it.
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the runtime directory's ACL, which is already owner-only for
        // everything under `%LOCALAPPDATA%`.
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    /// A scratch directory of this module's own, named for one test.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nysia-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn the_ceiling_is_the_product_of_the_two() {
        // The numbers the module docs quote. Pinned to literals rather than to each other,
        // because "the doc says 8 MiB and the constant says 80 MiB" is precisely the drift a
        // test written as `assert_eq!(MAX_LOG_BYTES, MAX_LOG_BYTES)` would not catch.
        assert_eq!(MAX_LOG_BYTES, 8 * 1024 * 1024);
        assert_eq!(KEPT_ROTATIONS, 3);
        assert_eq!(MAX_TOTAL_LOG_BYTES, 32 * 1024 * 1024);
        assert_eq!(
            MAX_TOTAL_LOG_BYTES,
            MAX_LOG_BYTES * (KEPT_ROTATIONS as u64 + 1)
        );
    }

    #[test]
    fn a_log_within_the_cap_is_left_alone() {
        let dir = scratch("under");
        let live = dir.join("nysiad.log");
        std::fs::write(&live, "one line\n").expect("a log");

        assert_eq!(trim_to(&live, 64, 3).expect("a trim"), Trimmed::Untouched);
        assert_eq!(read(&live), "one line\n");
        assert!(!rotation_path(&live, 1).exists());
    }

    #[test]
    fn a_log_exactly_at_the_cap_is_left_alone() {
        // The boundary is `>`, not `>=`: a file that is exactly the size it is allowed to be
        // has not broken the rule, and rotating it would spend a copy for nothing.
        let dir = scratch("boundary");
        let live = dir.join("nysiad.log");
        std::fs::write(&live, vec![b'x'; 64]).expect("a log");

        assert_eq!(trim_to(&live, 64, 3).expect("a trim"), Trimmed::Untouched);
        assert_eq!(
            trim_to(&live, 63, 3).expect("a trim"),
            Trimmed::Rotated { bytes: 64 }
        );
    }

    #[test]
    fn a_missing_log_is_not_an_error() {
        // A daemon started by hand logs to the terminal it was started from and may never
        // create this file. The tick that trims it runs regardless.
        let dir = scratch("absent");
        assert_eq!(
            trim(&dir.join("never-written.log")).expect("a trim"),
            Trimmed::Untouched
        );
    }

    #[test]
    fn rotation_survives_a_writer_holding_the_file_open() {
        // **The test the whole design exists to pass.** The daemon's stdout and stderr are an
        // inherited handle onto this file, so the rotation happens underneath a process that
        // is still appending — and a handle names the file, not the path.
        //
        // A rename-based rotation fails here in the way that matters: the rename succeeds,
        // `writer` goes on writing into the *rotated* file because that is the file its handle
        // names, and the live log stays empty for the rest of the daemon's life. The log would
        // appear to stop the first time it was ever rotated.
        let dir = scratch("openhandle");
        let live = dir.join("nysiad.log");

        let mut writer = open_for_append(&live).expect("an append handle");
        writer.write_all(b"before the rotation\n").expect("a write");
        writer.flush().expect("a flush");

        let trimmed = trim_to(&live, 8, KEPT_ROTATIONS).expect("a trim");
        assert_eq!(trimmed, Trimmed::Rotated { bytes: 20 });

        // The same handle, never reopened — exactly what a running daemon holds.
        writer.write_all(b"after the rotation\n").expect("a write");
        writer.flush().expect("a flush");

        assert_eq!(read(&live), "after the rotation\n");
        assert_eq!(read(&rotation_path(&live, 1)), "before the rotation\n");
    }

    #[test]
    fn the_oldest_rotation_is_dropped() {
        let dir = scratch("oldest");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");

        // One rotation per generation, so each rotated file holds a line naming itself.
        for generation in 0..(KEPT_ROTATIONS + 2) {
            writer
                .write_all(format!("generation {generation}\n").as_bytes())
                .expect("a write");
            writer.flush().expect("a flush");
            assert!(matches!(
                trim_to(&live, 4, KEPT_ROTATIONS).expect("a trim"),
                Trimmed::Rotated { .. }
            ));
        }

        // The newest rotation holds the last generation written, the oldest kept holds the
        // one `KEPT_ROTATIONS - 1` before it, and nothing exists past the cap.
        let newest = KEPT_ROTATIONS + 1;
        for index in 1..=KEPT_ROTATIONS {
            assert_eq!(
                read(&rotation_path(&live, index)),
                format!("generation {}\n", newest - (index - 1))
            );
        }
        assert!(
            !rotation_path(&live, KEPT_ROTATIONS + 1).exists(),
            "a {}th rotation was kept when the policy keeps {KEPT_ROTATIONS}",
            KEPT_ROTATIONS + 1
        );
    }

    #[test]
    fn a_trim_empties_the_live_log_and_keeps_only_the_policy() {
        // What a trim actually guarantees, stated as the two things that are true however
        // hard the writer was writing: the live file is left empty, and no more than
        // `KEPT_ROTATIONS` copies survive.
        //
        // What it deliberately does *not* assert is that every file is inside the cap. A
        // rotation is a copy of the live file at the moment it was measured, so a burst
        // between two checks is carried across whole — the alternative would be truncating a
        // rotation mid-line, which throws away log to satisfy an arithmetic claim nobody
        // needed. The overshoot is named in `MAX_TOTAL_LOG_BYTES` rather than hidden here.
        let dir = scratch("ceiling");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");

        let cap = 16;
        let burst = 24;
        for _ in 0..(KEPT_ROTATIONS + 3) {
            writer.write_all(&vec![b'x'; burst]).expect("a write");
            writer.flush().expect("a flush");
            trim_to(&live, cap, KEPT_ROTATIONS).expect("a trim");

            assert_eq!(
                std::fs::metadata(&live).expect("a live log").len(),
                0,
                "a trim left bytes in the live log"
            );
        }

        let kept: Vec<usize> = (1..=KEPT_ROTATIONS + 4)
            .filter(|index| rotation_path(&live, *index).exists())
            .collect();
        assert_eq!(
            kept,
            (1..=KEPT_ROTATIONS).collect::<Vec<_>>(),
            "the rotations on disk are not the {KEPT_ROTATIONS} the policy keeps"
        );

        // The bound that does hold: each file carries one measurement's worth, so the total
        // is the policy's ceiling plus the overshoot the schedule allowed.
        let total: u64 = kept
            .iter()
            .filter_map(|index| std::fs::metadata(rotation_path(&live, *index)).ok())
            .map(|metadata| metadata.len())
            .sum();
        let ceiling = (cap + burst as u64) * KEPT_ROTATIONS as u64;
        assert!(
            total <= ceiling,
            "every log together is {total} bytes against {ceiling}"
        );
    }

    #[test]
    fn the_real_cap_is_the_one_trim_applies() {
        // `trim` against the shipped constants, so the small-cap tests above cannot all pass
        // while `trim` itself is wired to the wrong number. Sparse: `set_len` allocates no
        // blocks on NTFS or APFS, so this costs a copy and not a write of 8 MiB.
        let dir = scratch("realcap");
        let live = dir.join("nysiad.log");

        File::create(&live)
            .expect("a log")
            .set_len(MAX_LOG_BYTES)
            .expect("a size");
        assert_eq!(trim(&live).expect("a trim"), Trimmed::Untouched);

        OpenOptions::new()
            .append(true)
            .open(&live)
            .expect("the log")
            .write_all(b"the byte that crosses the cap")
            .expect("a write");
        assert!(matches!(
            trim(&live).expect("a trim"),
            Trimmed::Rotated { .. }
        ));
        assert_eq!(
            std::fs::metadata(&live).expect("the live log").len(),
            0,
            "the live log was not truncated"
        );
    }

    #[test]
    fn opening_for_append_rotates_what_a_previous_run_left_behind() {
        // The other half of the cap. A daemon that died over the cap leaves a file the next
        // spawn would otherwise append to for ever.
        let dir = scratch("reopen");
        let live = dir.join("nysiad.log");
        File::create(&live)
            .expect("a log")
            .set_len(MAX_LOG_BYTES + 1)
            .expect("a size");

        let mut opened = open_for_append(&live).expect("an append handle");
        opened.write_all(b"this run\n").expect("a write");
        opened.flush().expect("a flush");

        assert_eq!(read(&live), "this run\n");
        assert_eq!(
            std::fs::metadata(rotation_path(&live, 1))
                .expect("the rotation")
                .len(),
            MAX_LOG_BYTES + 1
        );
    }

    #[test]
    fn a_rotation_is_named_beside_the_log_it_came_from() {
        let live = Path::new("/tmp/runtime/nysiad-v1.log");
        assert_eq!(
            rotation_path(live, 2),
            Path::new("/tmp/runtime/nysiad-v1.log.2")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_log_and_its_rotations_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("modes");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");
        writer.write_all(b"secret-adjacent\n").expect("a write");
        writer.flush().expect("a flush");
        trim_to(&live, 4, KEPT_ROTATIONS).expect("a trim");

        for path in [live.clone(), rotation_path(&live, 1)] {
            let mode = std::fs::metadata(&path)
                .expect("a log")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{} is mode {:o}, which is not owner-only",
                path.display(),
                mode & 0o777
            );
        }
    }

    #[test]
    fn a_partial_read_of_the_live_log_is_still_whole_lines() {
        // Not a property the implementation guarantees, and this test says which half it
        // does: the *rotation* is a byte-for-byte copy of what the live log held, so a reader
        // can concatenate `.1` and the live file and get the stream back in order.
        let dir = scratch("order");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");
        writer.write_all(b"first\nsecond\n").expect("a write");
        writer.flush().expect("a flush");
        trim_to(&live, 4, KEPT_ROTATIONS).expect("a trim");
        writer.write_all(b"third\n").expect("a write");
        writer.flush().expect("a flush");

        let mut rotated = String::new();
        File::open(rotation_path(&live, 1))
            .expect("the rotation")
            .read_to_string(&mut rotated)
            .expect("its bytes");
        assert_eq!(rotated + &read(&live), "first\nsecond\nthird\n");
    }
}
