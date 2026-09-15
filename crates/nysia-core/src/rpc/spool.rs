//! The disk spool: a status that could not be handed over, kept until a daemon can take it.
//!
//! §2.3 and §5.2 point 3. Orca spools because its HTTP POST can fail; Nysia keeps the spool
//! for the one case §5.3 leaves — **the daemon is restarting when a hook fires**. Everything
//! else Orca needs around it is gone: no port, no token, no endpoint-indirection file. The
//! endpoint is stable for the daemon's life (§4), so the only thing left to survive is the
//! daemon's own absence.
//!
//! # Who writes and who drains
//!
//! `nysia hook` writes, the daemon drains. That split is not a choice of layering, it is the
//! only one that works: the spool exists for the moment when there is no daemon to tell, so a
//! daemon-side spool would be a spool nothing could reach when it was needed. The daemon
//! therefore never appends — it renames, reads and deletes.
//!
//! # What is on a line, and why it is a row rather than an event
//!
//! One [`AgentStatusRow`] per line, as JSON, in `nysia-proto`'s own camelCase spelling.
//!
//! A [`HookEvent`](nysia_proto::HookEvent) would have been the smaller thing to write, and it
//! is the wrong one. §2.2 says `observed_at` is when the status was **observed**, and the
//! store is explicit that a restored row is not restamped: a drain that stamped rows with its
//! own clock would make every rehydrated row newer than every live one, so
//! [`Store::restore_status`](crate::store::Store::restore_status) could never answer
//! `Superseded` and the 09:50-spooled/09:55-live case in its doc would overwrite the live
//! row with the older one. So the clock that matters is the clock at the moment the hook
//! fired, which is the hook's — and the only way to carry it is to build the row there.
//!
//! Building the row in the hook buys the other rule for free: `to_row` returns `None` for an
//! event §2.1 maps to nothing, so an unmapped event is never written to disk at all, and it
//! applies [`HookEvent::question`](nysia_proto::HookEvent::question), so a `tool_input` that
//! is not a question never reaches the file. That second one is trap 13/14 — this is the one
//! place in Nysia where a `waiting` payload is written to a plain file — and it is why
//! [`append`] creates its file owner-only rather than letting the umask decide.
//!
//! # The pane is in the row, not in the file name
//!
//! §2.3 asks for a per-pane JSONL, and a [`PaneKey`](nysia_proto::PaneKey) is
//! `<tabId>:<leafId>` where each half may contain anything but `:`, `@`, whitespace and
//! control characters — so `/`, `\`, `..` and every character Windows refuses are all legal in
//! one. A file named after a pane key is therefore a path traversal with extra steps. The name
//! is the key's bytes in hex, which cannot contain a separator, and the drain reads the pane
//! back out of the row rather than out of the name it was filed under.
//!
//! Identity puts **no length cap on a pane key** and hex doubles, so a long one is filed under
//! a digest instead — see [`file_stem`], which is also where the reason a digest collision
//! costs nothing is written down.
//!
//! # The drain deletes before it applies, and that is the safe direction
//!
//! Restoring **appends** and is not idempotent — the store's own doc says so, and
//! `an_out_of_order_drain_still_lands` is the test that holds the ordering rule it implies.
//! So a drain that replayed would not merely duplicate a row: after a re-run the newest row
//! by `seq` is whichever entry was handed over last, which for a replay is the *oldest* one.
//!
//! [`drain`] therefore renames each file to a unique `.draining` name, reads it, **unlinks
//! it, and only then hands the rows back**. A daemon that dies inside that window loses the
//! rows it had just read and had not yet served to anybody; one that deleted afterwards would
//! come back and replay them into a history that has no way to recover its order. Losing a
//! status nobody has seen is the cheaper failure, and it is the one this picks.
//!
//! A `.draining` file found on start is the tail of exactly that window from a previous run:
//! its rows were read and never applied, so picking them up is a first delivery and not a
//! replay.
//!
//! # What this does not cover
//!
//! A row spooled while a daemon *is* running — because the store write failed, or the socket
//! broke mid-request — waits for the next daemon start, not for the next hook. The spool is
//! drained once, at start, which is what §2.3 specifies; a drain on every connection would be
//! a disk read on the path of every verb to catch a case that only arises when the daemon is
//! already failing to write.
//!
//! # Proving these rules trip
//!
//! Traps register #12: every gate ships a proof that it trips. Each row below was **run
//! against the mutation beside it** rather than asserted to be capable of failing — apply the
//! edit, run the test from the crate directory, revert. The `hook.rs` rows live in the
//! `nysia` crate and are listed here because the rule they hold is this module's.
//!
//! | Mutate | To | Turns red |
//! |---|---|---|
//! | `file_stem`'s body | `pane.to_owned()` | `a_pane_key_never_becomes_a_path` |
//! | [`drain`]'s `remove_file` | never called | `a_row_spooled_is_a_row_drained_and_the_spool_is_then_empty` |
//! | `hook::spool_it`'s `observed_at` | `UnixMillis(0)` | `a_status_no_daemon_will_take_is_spooled_rather_than_lost` |
//! | `hook`'s `EXIT_FAILED` | `2` | `a_hook_never_exits_with_the_code_claude_reads_as_block` |
//! | `hook::event`'s `trim_start_matches` | dropped | `a_byte_order_mark_in_front_of_the_payload_costs_nothing` |
//! | `file_stem`'s hex-length guard | never taken | `a_pane_key_too_long_to_hex_still_spools_and_still_drains` |
//!
//! The byte-order-mark row is the only one here that was red **before** its fix rather than
//! after it:
//! it was found by running the acceptance test's `pwsh` line by hand, which is why that row is
//! evidence rather than a demonstration.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use nysia_proto::AgentStatusRow;

/// The directory the per-pane files live in, under the runtime directory.
const SPOOL_DIR: &str = "agent-status";

/// The extension a file waiting to be drained carries.
const SPOOLED: &str = "jsonl";

/// The extension a file carries once a drain has claimed it.
///
/// Renaming first is what lets a hook keep appending during a drain: the new writes land in a
/// fresh `.jsonl` that this drain has already stopped looking at, so no line is read twice
/// and none is lost between the read and the unlink.
const DRAINING: &str = "draining";

/// How large one pane's spool may grow before further rows are dropped.
///
/// A bound rather than a rotation. The spool covers a daemon restart, which is seconds; a
/// file that has reached a megabyte is a daemon that has been gone for a very long time, and
/// the rows at the front of it are already past §2.3's thirty minutes. Dropping the newest
/// rather than rewriting the file keeps [`append`] a single atomic append, which is what lets
/// two hooks in the same pane write at once without a lock.
const FILE_CAP_BYTES: u64 = 1 << 20;

/// Why a status could not be spooled or drained.
#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    /// The spool directory or one of its files could not be worked with.
    #[error("could not {action} the agent-status spool at {}: {source}", path.display())]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// Which path it was attempted on.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A row could not be rendered as JSON.
    ///
    /// Reachable only through a `question` payload that will not serialise, which is a value
    /// Claude wrote and this process merely carried.
    #[error("could not render an agent-status row as JSON: {0}")]
    Encode(#[source] serde_json::Error),
    /// The spool file is already at [`FILE_CAP_BYTES`].
    #[error("the agent-status spool for this pane is full at {} bytes", size)]
    Full {
        /// How large the file already was.
        size: u64,
    },
}

/// Where the spool lives for a runtime directory.
#[must_use]
pub fn spool_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SPOOL_DIR)
}

/// Append `row` to its pane's spool, creating the file owner-only if it is new.
///
/// # Errors
///
/// Returns [`SpoolError::Io`] when the directory or file cannot be written,
/// [`SpoolError::Encode`] when the row will not serialise, and [`SpoolError::Full`] when the
/// pane's file has already reached [`FILE_CAP_BYTES`].
pub fn append(runtime_dir: &Path, row: &AgentStatusRow) -> Result<PathBuf, SpoolError> {
    let dir = spool_dir(runtime_dir);
    std::fs::create_dir_all(&dir).map_err(|source| SpoolError::Io {
        action: "create",
        path: dir.clone(),
        source,
    })?;
    restrict_to_owner(&dir);

    // One line, with the newline, so a reader can split on lines and a torn write is one
    // line that will not parse rather than two lines that both do.
    let mut line = serde_json::to_vec(row).map_err(SpoolError::Encode)?;
    line.push(b'\n');

    let path = dir.join(format!("{}.{SPOOLED}", file_stem(row.pane.as_str())));
    if let Ok(existing) = std::fs::metadata(&path)
        && existing.len() >= FILE_CAP_BYTES
    {
        return Err(SpoolError::Full {
            size: existing.len(),
        });
    }

    let mut file = owner_only_append(&path).map_err(|source| SpoolError::Io {
        action: "open",
        path: path.clone(),
        source,
    })?;
    // One `write_all` of one line, on a handle opened for append. That is what makes two
    // hooks in the same pane safe without a lock: both kernels place the whole line at the
    // end of the file, and neither can land inside the other's.
    file.write_all(&line).map_err(|source| SpoolError::Io {
        action: "append to",
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// What a drain found.
#[derive(Debug, Default)]
pub struct Drained {
    /// The rows, in the order they were spooled, oldest file first.
    ///
    /// Order is a courtesy and not a guarantee: §2.3's drain is unsorted by construction, and
    /// `an_out_of_order_drain_still_lands` is the store test that says an out-of-order drain
    /// must land anyway.
    pub rows: Vec<AgentStatusRow>,
    /// Lines that were found and could not be read as a row.
    ///
    /// Counted rather than carried. A line that will not parse is a torn write or a file from
    /// a build that spelled the row differently, and the only useful thing to say about one is
    /// how many there were — the content is a status payload, which trap 13 keeps out of logs.
    pub unreadable: usize,
}

/// Claim every spooled row for this runtime directory, leaving the spool empty.
///
/// Each file is renamed, read and **unlinked before its rows are returned**, so a caller that
/// dies holding them loses them rather than replaying them. See the module docs for why that
/// is the safe direction.
///
/// A missing spool directory is an empty drain and not an error: it is what every first start
/// looks like.
///
/// # Errors
///
/// Returns [`SpoolError::Io`] when the directory exists and cannot be listed. A single file
/// that cannot be renamed or read is skipped and left for the next start rather than failing
/// the whole drain, because one unreadable pane must not cost every other pane its status.
pub fn drain(runtime_dir: &Path) -> Result<Drained, SpoolError> {
    let dir = spool_dir(runtime_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Drained::default());
        }
        Err(source) => {
            return Err(SpoolError::Io {
                action: "list",
                path: dir,
                source,
            });
        }
    };

    // Claimed first, read second. Renaming every file before reading any of them means a hook
    // appending during the drain writes to a fresh `.jsonl` this pass will not look at, so no
    // line can arrive after its file was read and before it was unlinked.
    let mut claimed = Vec::new();
    let mut leftovers = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(std::ffi::OsStr::to_str) {
            // The tail of a previous drain's window: read and never applied, so picking it up
            // is a first delivery rather than a replay.
            Some(DRAINING) => leftovers.push(path),
            Some(SPOOLED) => {
                if let Some(claim) = claim(&path) {
                    claimed.push(claim);
                }
            }
            _ => {}
        }
    }
    leftovers.extend(claimed);

    let mut drained = Drained::default();
    for path in leftovers {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    %err,
                    "could not read a drained agent-status spool file; leaving it for the next start"
                );
                continue;
            }
        };
        // Unlinked before the rows are handed back, never after. See the module docs.
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!(
                path = %path.display(),
                %err,
                "could not remove a drained agent-status spool file; dropping its rows rather \
                 than serving rows a later start would serve again"
            );
            continue;
        }
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<AgentStatusRow>(line) {
                Ok(row) => drained.rows.push(row),
                // The line itself is never logged: it may carry a `waiting` question (trap 13).
                Err(_) => drained.unreadable += 1,
            }
        }
    }
    Ok(drained)
}

/// Rename one spooled file out of the way, returning where it went.
///
/// A unique name rather than a fixed `.draining`, so a leftover from a previous start is
/// never overwritten by this one — those rows were read and never applied, and losing them
/// here would be losing them for good.
fn claim(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let claimed = path.with_file_name(format!("{stem}.{nonce}.{DRAINING}"));
    match std::fs::rename(path, &claimed) {
        Ok(()) => Some(claimed),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                %err,
                "could not claim an agent-status spool file; leaving it for the next start"
            );
            None
        }
    }
}

/// The longest hex stem this will produce before it switches to a digest.
///
/// Every filesystem this ships on caps one path component at 255 — bytes on ext4 and APFS,
/// UTF-16 units on NTFS — and the stem is not the whole name: [`append`] adds `.jsonl` and
/// [`claim`] adds `.<nanoseconds>.draining`, which is thirty more characters at its longest.
/// Two hundred leaves room for both with margin to spare, and it is a cap on the *stem* so
/// the arithmetic is done here rather than at each of the two call sites.
const MAX_HEX_STEM: usize = 200;

/// The prefix a digested stem carries.
///
/// A hex stem cannot contain `-`, so the two forms share no names and a long pane key can
/// never be filed under a short one's stem by coincidence.
const DIGEST_PREFIX: &str = "h-";

/// A pane key as a file name: its bytes in lower-case hex, or a digest when that is too long.
///
/// Not the key itself. See the module docs — a pane key may legally contain `/`, `\` and
/// `..`, so a file named after one is a path traversal. Hex has no separator in it at all,
/// which makes the question "could this escape the spool directory" answerable by looking at
/// the alphabet rather than at the input.
///
/// # Why there is a second form at all
///
/// Hex doubles, and `nysia-proto`'s identity puts **no length cap on a pane key** — the GUI
/// mints short ones and `session create --pane-key` takes whatever it is given. So a key over
/// about 124 bytes produced a name past 255, `append` failed, and the hook reported a refusal
/// and exited 1. Losing a status is the one outcome the spool exists to prevent, so the long
/// case switches to a digest rather than failing.
///
/// FNV-1a, matching `rpc::endpoint`'s `short_digest` and for the same reason: this is a name,
/// not a security boundary, and the workspace dependency table is coordinator-owned. A
/// collision here would file two panes in one file, which costs nothing and is worth saying
/// out loud — the drain reads each row's pane out of the row, so the rows still land under
/// the panes they belong to. That is also why the digest may be short.
fn file_stem(pane: &str) -> String {
    let hex: String = pane.bytes().map(|byte| format!("{byte:02x}")).collect();
    if hex.len() <= MAX_HEX_STEM {
        return hex;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pane.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{DIGEST_PREFIX}{hash:016x}")
}

/// Open `path` for appending, creating it owner-only if it is not there.
///
/// The mode is set **at creation** on Unix rather than afterwards: a file created with the
/// umask's mode and narrowed a moment later is a file that was world-readable for that
/// moment, and this one can hold a `waiting` question (trap 13/14). Windows has no umask and
/// the file inherits the runtime directory's ACL, which is already owner-only.
fn owner_only_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Narrow a directory to its owner, where the platform has a mode to narrow.
fn restrict_to_owner(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the runtime directory's ACL, which is already owner-only for
        // everything under `%LOCALAPPDATA%`.
        let _ = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nysia_proto::{AgentState, PaneKey, UnixMillis};

    fn runtime_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nysia-spool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a runtime directory");
        dir
    }

    fn pane(tab: &str) -> PaneKey {
        PaneKey::new(tab, "leaf_1").expect("a well-formed pane key")
    }

    fn row(pane: PaneKey, at: u64) -> AgentStatusRow {
        AgentStatusRow {
            pane,
            state: AgentState::Done,
            question: None,
            is_interrupt: false,
            session_boundary: false,
            agent_id: None,
            observed_at: UnixMillis(at),
            restored_unconfirmed: false,
        }
    }

    #[test]
    fn a_row_spooled_is_a_row_drained_and_the_spool_is_then_empty() {
        let dir = runtime_dir("round-trip");
        let written = row(pane("tab_1"), 1_000);
        append(&dir, &written).expect("the row spools");

        let first = drain(&dir).expect("the drain runs");
        assert_eq!(first.rows, vec![written]);
        assert_eq!(first.unreadable, 0);

        // The whole point of unlinking inside the drain: a second one finds nothing. A drain
        // that replayed would put the older row back at the head of the history, because
        // restoring appends and is not idempotent.
        let second = drain(&dir).expect("the drain runs again");
        assert!(second.rows.is_empty(), "a drained spool is an empty spool");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_observed_at_the_hook_recorded_survives_the_round_trip() {
        // The field the whole file shape is chosen for. `Store::restore_status` compares it
        // against live rows to decide whether a rehydrated row is already superseded, so a
        // spool that lost or restamped it would make every drained row look like the newest
        // thing in the table.
        let dir = runtime_dir("observed-at");
        append(&dir, &row(pane("tab_1"), 1_757_721_600_123)).expect("the row spools");
        let drained = drain(&dir).expect("the drain runs");
        assert_eq!(
            drained.rows.first().map(|row| row.observed_at),
            Some(UnixMillis(1_757_721_600_123))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_pane_gets_its_own_file_and_a_drain_takes_them_all() {
        let dir = runtime_dir("per-pane");
        append(&dir, &row(pane("tab_1"), 1_000)).expect("the first row spools");
        append(&dir, &row(pane("tab_2"), 2_000)).expect("the second row spools");
        assert_eq!(
            std::fs::read_dir(spool_dir(&dir))
                .expect("the spool exists")
                .count(),
            2,
            "§2.3 asks for a per-pane JSONL"
        );

        let drained = drain(&dir).expect("the drain runs");
        let mut panes: Vec<String> = drained
            .rows
            .iter()
            .map(|row| row.pane.as_str().to_owned())
            .collect();
        panes.sort();
        assert_eq!(panes, ["tab_1:leaf_1", "tab_2:leaf_1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pane_key_never_becomes_a_path() {
        // A pane key may legally contain `/`, `\` and `..`: §identity refuses only `:`, `@`,
        // whitespace and control characters. A file named after one would escape the spool.
        let dir = runtime_dir("traversal");
        let hostile = pane("../../escaped");
        append(&dir, &row(hostile.clone(), 1_000)).expect("the row spools");

        let spool = spool_dir(&dir);
        let names: Vec<String> = std::fs::read_dir(&spool)
            .expect("the spool exists")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "one pane, one file");
        for name in &names {
            assert!(
                !name.contains("..") && !name.contains('/') && !name.contains('\\'),
                "a spool file name carries no path syntax, got {name}"
            );
        }
        assert!(
            !dir.join("escaped.jsonl").exists() && !spool.join("../escaped.jsonl").exists(),
            "nothing was written outside the spool directory"
        );
        // And the pane survives anyway, because it travels in the row rather than in the name.
        let drained = drain(&dir).expect("the drain runs");
        assert_eq!(
            drained.rows.first().map(|row| row.pane.clone()),
            Some(hostile)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_that_will_not_parse_is_counted_and_costs_the_others_nothing() {
        let dir = runtime_dir("torn");
        let good = row(pane("tab_1"), 1_000);
        append(&dir, &good).expect("the row spools");
        let path = spool_dir(&dir).join(format!("{}.{SPOOLED}", file_stem(good.pane.as_str())));
        let mut file = owner_only_append(&path).expect("the file is there");
        file.write_all(b"{\"pane\":\"truncated\n")
            .expect("a torn write");
        drop(file);

        let drained = drain(&dir).expect("the drain runs");
        assert_eq!(drained.rows, vec![good], "the whole line still lands");
        assert_eq!(drained.unreadable, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_spool_refuses_rather_than_growing_without_bound() {
        let dir = runtime_dir("full");
        let path = spool_dir(&dir).join(format!("{}.{SPOOLED}", file_stem(pane("tab_1").as_str())));
        std::fs::create_dir_all(spool_dir(&dir)).expect("the spool directory");
        std::fs::write(&path, vec![b'x'; FILE_CAP_BYTES as usize]).expect("a full file");

        let err = append(&dir, &row(pane("tab_1"), 1_000)).expect_err("a full spool refuses");
        assert!(matches!(err, SpoolError::Full { .. }), "got {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pane_key_too_long_to_hex_still_spools_and_still_drains() {
        // Hex doubles and `nysia-proto`'s identity caps a pane key at nothing, so a key over
        // about 124 bytes used to produce a name past every filesystem's 255 and the append
        // failed — which the hook reports as a refusal and exits 1 on. Losing a status is the
        // one outcome the spool exists to prevent, so the long case digests instead.
        let dir = runtime_dir("long-key");
        let long = pane(&"t".repeat(300));
        assert!(
            long.as_str().len() > 300,
            "the fixture has to be past the hex cap to mean anything"
        );

        let path = append(&dir, &row(long.clone(), 1_000)).expect("a long pane key still spools");
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        assert!(
            name.len() < 255,
            "the whole name has to fit one path component, got {} chars",
            name.len()
        );
        assert!(name.starts_with(DIGEST_PREFIX), "got {name}");
        assert!(
            !name.contains("..") && !name.contains('/') && !name.contains('\\'),
            "the digest form carries no path syntax either, got {name}"
        );

        // And the pane survives, because it travels in the row rather than in the name — which
        // is also why a digest collision would cost nothing.
        let drained = drain(&dir).expect("the drain runs");
        assert_eq!(drained.rows.first().map(|row| row.pane.clone()), Some(long));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_ordinary_pane_key_keeps_its_readable_hex_name() {
        // The common case is unchanged, which is the point of the cap rather than a blanket
        // digest: a uuid-pair key is ~82 bytes, well inside the hex form, and a person looking
        // at the spool directory can still decode the name back to the pane.
        let uuid_pair = pane("tab_9f8a6c12-4b1e-4a77-9d33-0c2f71b5ee90");
        let stem = file_stem(uuid_pair.as_str());
        assert!(!stem.starts_with(DIGEST_PREFIX), "got {stem}");
        assert!(stem.len() <= MAX_HEX_STEM);
        assert_eq!(
            String::from_utf8(
                stem.as_bytes()
                    .chunks(2)
                    .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
                    .collect()
            )
            .expect("the hex decodes back to the key"),
            uuid_pair.as_str(),
            "the hex form stays reversible by eye"
        );
    }

    #[test]
    fn a_leftover_claim_from_a_dead_drain_is_delivered_rather_than_dropped() {
        // The window the module docs describe: a daemon renamed a file, read it, and died
        // before it could unlink it. Those rows were never served to anybody, so picking them
        // up is a first delivery.
        let dir = runtime_dir("leftover");
        let spool = spool_dir(&dir);
        std::fs::create_dir_all(&spool).expect("the spool directory");
        let orphan = row(pane("tab_1"), 1_000);
        let mut line = serde_json::to_vec(&orphan).expect("a row renders");
        line.push(b'\n');
        std::fs::write(
            spool.join(format!("{}.7.{DRAINING}", file_stem(orphan.pane.as_str()))),
            line,
        )
        .expect("a leftover claim");

        let drained = drain(&dir).expect("the drain runs");
        assert_eq!(drained.rows, vec![orphan]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_spool_is_owner_only_because_it_can_hold_a_question() {
        use std::os::unix::fs::PermissionsExt;

        let dir = runtime_dir("owner-only");
        let waiting = AgentStatusRow {
            state: AgentState::Waiting,
            question: Some(serde_json::json!({ "question": "a secret" })),
            ..row(pane("tab_1"), 1_000)
        };
        let path = append(&dir, &waiting).expect("the row spools");
        let mode = std::fs::metadata(&path)
            .expect("the file is there")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "trap 13: a spooled question is owner-only");
        assert_eq!(
            std::fs::metadata(spool_dir(&dir))
                .expect("the directory is there")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
