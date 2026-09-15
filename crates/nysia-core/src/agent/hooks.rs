//! Installing and removing the agent's status hooks, without damaging the file they live in.
//!
//! This is the neutral half: reading, writing and the three properties §5.3 names. Which
//! events are installed, what the entries look like and how one of ours is recognised is
//! [`super::claude`]'s business and stays there.
//!
//! The three properties, and where each is enforced:
//!
//! - **Atomic** — [`write_atomically`]. Temp file in the same directory, flushed, then
//!   renamed over the original. A settings file half-written because the process died mid-`write`
//!   is a user locked out of their own configuration.
//! - **Preserves foreign hook entries** — [`super::claude::hooks`] edits only entries it can
//!   prove are Nysia's, and [`super::ordered_json`] keeps every other key exactly where the
//!   user left it, down to the indent.
//! - **Idempotent** — installing removes what a previous install left before adding anything,
//!   so twice leaves one, and a Nysia that moved to a new path leaves no orphan behind.
//!
//! Uninstall is the Settings toggle *"Agent status hooks — turn off to remove managed
//! hooks"*, and it has to leave the file as if Nysia had never touched it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::ordered_json::Document;

/// Why the hooks could not be installed or removed.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// The settings file exists but could not be read.
    #[error("{path} could not be read: {source}")]
    Read {
        /// The file in question.
        path: PathBuf,
        /// What the filesystem said.
        #[source]
        source: io::Error,
    },
    /// The settings file is not JSON.
    ///
    /// Reported rather than overwritten: a file that fails to parse is either corrupt or
    /// newer than this build, and replacing it with a fresh one would delete a user's
    /// configuration to fix a problem they did not have.
    #[error("{path} is not valid JSON: {source}")]
    Parse {
        /// The file in question.
        path: PathBuf,
        /// What the parser said.
        #[source]
        source: serde_json::Error,
    },
    /// The settings file parsed, but its top level is not an object.
    #[error("{path} does not contain a JSON object")]
    NotAnObject {
        /// The file in question.
        path: PathBuf,
    },
    /// The settings file could not be written.
    #[error("{path} could not be written: {source}")]
    Write {
        /// The file in question.
        path: PathBuf,
        /// What the filesystem said.
        #[source]
        source: io::Error,
    },
    /// The settings file has a `hooks` key that is not an object.
    ///
    /// Reported rather than replaced, for the same reason a parse failure is: the file is
    /// shaped in a way this code does not understand, and the safe move is to say so.
    #[error("the settings file has a `hooks` key that is not an object")]
    HooksNotAnObject,
    /// One of the agent's events holds something other than an array of hook groups.
    ///
    /// The same policy as [`HookError::HooksNotAnObject`], one level down, and it is here
    /// because it was not: installing used to replace such a value outright, so a user whose
    /// `hooks.Stop` held an object lost it without being told. Reachable only when the file
    /// already disagrees with the agent's own schema, but it is still their file.
    #[error("the settings file has a `hooks.{event}` that is not an array of hook groups")]
    EventNotAnArray {
        /// The event whose value could not be read.
        event: &'static str,
    },
    /// The settings file begins with a byte-order mark.
    ///
    /// Its own variant because the parser's message for it — "expected value at line 1
    /// column 1" — says where but not what, and a file saved by Notepad is a plausible way to
    /// arrive here. Refused rather than stripped: `JSON.parse` rejects a BOM too, so the file
    /// is already unreadable by the agent that owns it.
    #[error("{path} begins with a UTF-8 byte-order mark, which is not valid JSON")]
    ByteOrderMark {
        /// The file in question.
        path: PathBuf,
    },
    /// The home directory could not be determined, so there is no settings file to edit.
    #[error("no home directory, so {0} cannot be located")]
    NoHome(&'static str),
}

/// What an install or uninstall did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookChange {
    /// How many of the agent's events now carry a managed hook.
    pub installed: usize,
    /// How many managed entries were removed on the way.
    pub removed: usize,
    /// Whether the file was actually rewritten.
    ///
    /// False when the settings on disk already said exactly this, which is what makes a
    /// second install a no-op rather than a no-op-shaped rewrite.
    pub wrote: bool,
}

/// Install the agent's status hooks into `settings`, pointing them at `nysia`.
///
/// `nysia` is the path to this binary, and the caller passes it — usually from
/// [`std::env::current_exe`]. It is never derived here: baking a compile-time path into
/// something path-shaped is traps register #9, and a `CARGO_MANIFEST_DIR` in a user's
/// settings file would survive every reinstall pointing at a build tree.
///
/// # Errors
///
/// See [`HookError`]. A file that does not exist yet is created; a file that does not parse
/// is reported rather than replaced.
pub fn install(settings: &Path, nysia: &Path) -> Result<HookChange, HookError> {
    edit(settings, |document| {
        super::claude::hooks::install(document, nysia)
    })
}

/// Remove every managed hook from `settings`, leaving the file as if Nysia had never
/// touched it.
///
/// # Errors
///
/// See [`HookError`].
pub fn uninstall(settings: &Path) -> Result<HookChange, HookError> {
    edit(settings, super::claude::hooks::uninstall)
}

/// How many of the agent's events currently carry a managed hook.
///
/// # Errors
///
/// See [`HookError`]. A settings file that does not exist yet reports zero rather than
/// failing — nothing is installed, which is a fact, not an error.
pub fn installed_count(settings: &Path) -> Result<usize, HookError> {
    let Some(document) = read(settings)? else {
        return Ok(0);
    };
    Ok(super::claude::hooks::installed_count(&document))
}

/// Read, apply `change`, and write back only if it made a difference.
fn edit(
    settings: &Path,
    change: impl FnOnce(&mut Document) -> Result<HookChange, HookError>,
) -> Result<HookChange, HookError> {
    let existing = read(settings)?;
    let before = existing.clone();
    let mut document = existing.unwrap_or_else(Document::empty);

    let mut outcome = change(&mut document)?;

    if before.as_ref() == Some(&document) {
        outcome.wrote = false;
        return Ok(outcome);
    }

    let rendered = document.render().map_err(|source| HookError::Write {
        path: settings.to_path_buf(),
        source: io::Error::other(source),
    })?;
    write_atomically(settings, &rendered)?;
    outcome.wrote = true;
    Ok(outcome)
}

/// Read and parse the settings file, or `None` if there is not one yet.
fn read(settings: &Path) -> Result<Option<Document>, HookError> {
    let text = match fs::read_to_string(settings) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(HookError::Read {
                path: settings.to_path_buf(),
                source,
            });
        }
    };
    // An empty file is what an interrupted writer leaves. Treat it as absent rather than as
    // a parse error, since there is nothing in it to preserve.
    if text.trim().is_empty() {
        return Ok(None);
    }
    if text.starts_with('\u{feff}') {
        return Err(HookError::ByteOrderMark {
            path: settings.to_path_buf(),
        });
    }
    let document = Document::parse(&text).map_err(|source| HookError::Parse {
        path: settings.to_path_buf(),
        source,
    })?;
    if !matches!(document.value, super::ordered_json::Json::Object(_)) {
        return Err(HookError::NotAnObject {
            path: settings.to_path_buf(),
        });
    }
    Ok(Some(document))
}

/// Distinguishes two temp files written by the same process at the same moment.
static WRITE_NONCE: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to `path` as a temp file plus a rename.
///
/// The temp file is made in the **same directory** deliberately: a rename is only atomic
/// within one filesystem, and `%TEMP%` is routinely on a different volume from a user's
/// profile. `sync_all` before the rename is what makes the atomicity survive a power cut
/// rather than only a crash — without it the rename can land while the bytes have not.
///
/// # What this does and does not make safe
///
/// The temp name carries a process id **and a per-process counter**, so two threads in one
/// daemon writing the same settings file do not share a scratch file and clobber each
/// other's half-written bytes.
///
/// That is not the same as making concurrent edits correct, and the difference matters to
/// whoever wires this up. Each writer still reads, edits and renames independently, so two
/// overlapping installs are a last-writer-wins race on the *file*: both succeed, and the
/// loser's changes are gone. **Serialising calls is the caller's job** — for wave C, that
/// means the daemon holding one lock across read-edit-write rather than assuming this
/// function is a critical section. Nothing here can enforce it, because the thing to lock is
/// the whole sequence and this function only sees the last step of it.
fn write_atomically(path: &Path, contents: &str) -> Result<(), HookError> {
    use std::io::Write;

    let failed = |source: io::Error| HookError::Write {
        path: path.to_path_buf(),
        source,
    };

    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory).map_err(failed)?;

    let temporary = directory.join(format!(
        ".{}.nysia-{}-{}.tmp",
        path.file_name()
            .map_or_else(|| "settings.json".into(), |name| name.to_string_lossy()),
        std::process::id(),
        WRITE_NONCE.fetch_add(1, Ordering::Relaxed)
    ));

    let mut file = create_private(&temporary).map_err(failed)?;
    let result = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(failed(source));
    }

    // Keep whatever mode the user had. A settings file they had made owner-only must not
    // come back world-readable because Nysia rewrote it.
    #[cfg(unix)]
    if let Ok(existing) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, existing.permissions());
    }

    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(failed(source));
    }
    Ok(())
}

/// Create the scratch file owner-only from the first byte.
///
/// On Unix, `File::create` applies the process umask, so a file that is about to hold a
/// user's settings exists as 0644 for the window between creation and the mode being copied
/// from the original. Nothing in a hook command is a secret today, but the window is free to
/// close and a settings file is exactly the kind of thing that grows one.
#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Windows has no mode to set here; the directory's ACL is what governs.
#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<fs::File> {
    fs::File::create(path)
}

/// Where the agent keeps the settings file these hooks go into.
///
/// # Errors
///
/// Returns [`HookError::NoHome`] when there is no home directory to look in.
pub fn user_settings_path() -> Result<PathBuf, HookError> {
    super::claude::hooks::user_settings_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::scratch::Scratch;

    #[test]
    fn an_atomic_write_leaves_no_temp_file_behind() {
        let scratch = Scratch::new("hooks-atomic");
        let target = scratch.join("settings.json");
        write_atomically(&target, "{\n  \"a\": 1\n}\n").expect("writes");
        assert_eq!(
            fs::read_to_string(&target).expect("reads"),
            "{\n  \"a\": 1\n}\n"
        );

        let leftovers: Vec<_> = fs::read_dir(scratch.path())
            .expect("lists")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "settings.json")
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn an_atomic_write_replaces_an_existing_file() {
        // `fs::rename` over an existing path is an error on some platforms and a replace on
        // others; this pins that the replace is what happens on the ones we ship.
        let scratch = Scratch::new("hooks-replace");
        let target = scratch.join("settings.json");
        fs::write(&target, "old").expect("seed");
        write_atomically(&target, "new").expect("writes");
        assert_eq!(fs::read_to_string(&target).expect("reads"), "new");
    }

    #[test]
    fn a_settings_file_that_is_not_json_is_reported_not_replaced() {
        let scratch = Scratch::new("hooks-corrupt");
        let target = scratch.join("settings.json");
        fs::write(&target, "{ this is not json").expect("seed");

        let err = install(&target, Path::new("/opt/nysia")).expect_err("must refuse");
        assert!(matches!(err, HookError::Parse { .. }), "{err:?}");
        // And the user's file is untouched, which is the half that matters.
        assert_eq!(
            fs::read_to_string(&target).expect("reads"),
            "{ this is not json"
        );
    }

    #[test]
    fn a_settings_file_whose_top_level_is_not_an_object_is_refused() {
        let scratch = Scratch::new("hooks-array");
        let target = scratch.join("settings.json");
        fs::write(&target, "[1, 2, 3]").expect("seed");
        let err = install(&target, Path::new("/opt/nysia")).expect_err("must refuse");
        assert!(matches!(err, HookError::NotAnObject { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_settings_file_reports_nothing_installed_rather_than_failing() {
        let scratch = Scratch::new("hooks-absent");
        assert_eq!(
            installed_count(&scratch.join("nope.json")).expect("reports"),
            0
        );
    }

    /// A settings file shaped like a real one, and deliberately awkward in four ways.
    ///
    /// Its top-level keys are not in alphabetical order, so a round trip through a sorted
    /// map shows up immediately. `hooks` holds an event Nysia knows nothing about; `Stop`
    /// holds two foreign groups, so ours has somewhere to go *between* other people's work;
    /// and `PreToolUse` holds a group whose `hooks` array Nysia will end up sharing with a
    /// foreign entry, which is the case where removing a group wholesale would take a user's
    /// hook with it.
    const REALISTIC: &str = concat!(
        "{\n",
        "  \"model\": \"opus\",\n",
        "  \"hooks\": {\n",
        "    \"SomeFutureEvent\": [\n",
        "      {\n",
        "        \"hooks\": [\n",
        "          {\n",
        "            \"type\": \"command\",\n",
        "            \"command\": \"echo mine\"\n",
        "          }\n",
        "        ]\n",
        "      }\n",
        "    ],\n",
        "    \"Stop\": [\n",
        "      {\n",
        "        \"hooks\": [\n",
        "          {\n",
        "            \"type\": \"command\",\n",
        "            \"command\": \"/usr/bin/first-foreign\"\n",
        "          }\n",
        "        ]\n",
        "      },\n",
        "      {\n",
        "        \"hooks\": [\n",
        "          {\n",
        "            \"type\": \"command\",\n",
        "            \"command\": \"/usr/bin/second-foreign\"\n",
        "          }\n",
        "        ]\n",
        "      }\n",
        "    ]\n",
        "  },\n",
        "  \"theme\": \"dark\"\n",
        "}\n"
    );

    /// A settings file seeded with [`REALISTIC`], and the path to it.
    fn seeded(tag: &str) -> (Scratch, PathBuf) {
        let scratch = Scratch::new(tag);
        let target = scratch.join("settings.json");
        fs::write(&target, REALISTIC).expect("seed");
        (scratch, target)
    }

    /// Every `command` string anywhere in the file, in order.
    fn commands(text: &str) -> Vec<String> {
        let document = Document::parse(text).expect("valid json");
        let mut found = Vec::new();
        collect_commands(&document.value, &mut found);
        found
    }

    fn collect_commands(value: &super::super::ordered_json::Json, into: &mut Vec<String>) {
        use super::super::ordered_json::Json;
        match value {
            Json::Object(entries) => {
                if let Some(Json::String(command)) = value.get("command") {
                    into.push(command.clone());
                }
                for (_, child) in entries {
                    collect_commands(child, into);
                }
            }
            Json::Array(items) => {
                for item in items {
                    collect_commands(item, into);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn install_then_uninstall_leaves_the_file_exactly_as_it_was() {
        // The property in its sharpest form: byte-for-byte, foreign hooks and key order and
        // indentation included. Anything the installer disturbs and does not put back shows
        // up here as a diff.
        let (_scratch, target) = seeded("hooks-roundtrip");

        let installed = install(&target, Path::new("/opt/nysia/nysia")).expect("installs");
        assert_eq!(installed.installed, 12);
        assert!(installed.wrote);
        assert_ne!(
            fs::read_to_string(&target).expect("reads"),
            REALISTIC,
            "the install must actually have changed the file"
        );

        let removed = uninstall(&target).expect("uninstalls");
        assert_eq!(removed.removed, 12);
        assert_eq!(fs::read_to_string(&target).expect("reads"), REALISTIC);
    }

    #[test]
    fn a_foreign_hook_on_either_side_of_ours_survives_both_ways() {
        // The property most likely to be got wrong quietly, so it is asserted on the
        // commands themselves rather than on a count.
        let (_scratch, target) = seeded("hooks-foreign");
        install(&target, Path::new("/opt/nysia/nysia")).expect("installs");

        let after = fs::read_to_string(&target).expect("reads");
        let found = commands(&after);
        for foreign in [
            "echo mine",
            "/usr/bin/first-foreign",
            "/usr/bin/second-foreign",
        ] {
            assert!(found.iter().any(|c| c == foreign), "{foreign} was lost");
        }
        // And ours went in behind the two that were already on `Stop`, rather than in front
        // of them: a hook the user had put first stays first.
        let stop = Document::parse(&after).expect("valid json");
        let groups = stop
            .value
            .get("hooks")
            .and_then(|table| table.get("Stop"))
            .and_then(super::super::ordered_json::Json::as_array)
            .expect("Stop has groups");
        assert_eq!(groups.len(), 3);
        assert_eq!(
            commands(&groups[0].to_string_pretty("  ").expect("renders")),
            vec!["/usr/bin/first-foreign"]
        );

        uninstall(&target).expect("uninstalls");
        assert_eq!(
            commands(&fs::read_to_string(&target).expect("reads")),
            vec![
                "echo mine",
                "/usr/bin/first-foreign",
                "/usr/bin/second-foreign"
            ]
        );
    }

    #[test]
    fn a_foreign_entry_sharing_a_group_with_ours_keeps_its_group() {
        // Uninstall removes entries, and only removes the group when nothing else was in it.
        // Dropping the whole group would be the easy implementation and would delete a hook
        // the user wrote.
        let scratch = Scratch::new("hooks-shared-group");
        let target = scratch.join("settings.json");
        fs::write(
            &target,
            concat!(
                "{\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n",
                "          {\n            \"type\": \"command\",\n",
                "            \"command\": \"'/opt/nysia/nysia' hook --event Stop\"\n          },\n",
                "          {\n            \"type\": \"command\",\n",
                "            \"command\": \"keep-me\"\n          }\n",
                "        ]\n      }\n    ]\n  }\n}\n"
            ),
        )
        .expect("seed");

        let removed = uninstall(&target).expect("uninstalls");
        assert_eq!(removed.removed, 1);
        assert_eq!(
            commands(&fs::read_to_string(&target).expect("reads")),
            vec!["keep-me"]
        );
    }

    #[test]
    fn installing_twice_leaves_exactly_one_set_and_does_not_rewrite() {
        let (_scratch, target) = seeded("hooks-idempotent");
        install(&target, Path::new("/opt/nysia/nysia")).expect("first install");
        let once = fs::read_to_string(&target).expect("reads");

        let again = install(&target, Path::new("/opt/nysia/nysia")).expect("second install");
        assert_eq!(again.installed, 12);
        assert_eq!(again.removed, 12, "the second install replaces the first");
        assert!(
            !again.wrote,
            "nothing changed, so the file must not be rewritten at all"
        );
        assert_eq!(fs::read_to_string(&target).expect("reads"), once);
        assert_eq!(installed_count(&target).expect("counts"), 12);

        // And one uninstall is still enough to take it all back out.
        uninstall(&target).expect("uninstalls");
        assert_eq!(fs::read_to_string(&target).expect("reads"), REALISTIC);
    }

    #[test]
    fn reinstalling_from_a_new_path_leaves_no_orphan_pointing_at_the_old_one() {
        // The reason entries are matched by shape rather than by the exact string we last
        // wrote. An install that moved would otherwise leave both hooks live, both firing.
        let (_scratch, target) = seeded("hooks-moved");
        install(&target, Path::new("/opt/nysia/nysia")).expect("first install");
        install(&target, Path::new("/usr/local/bin/nysia")).expect("second install");

        let found = commands(&fs::read_to_string(&target).expect("reads"));
        assert_eq!(
            found.iter().filter(|c| c.contains("/opt/nysia")).count(),
            0,
            "an orphan from the old path is still installed: {found:?}"
        );
        assert_eq!(
            found
                .iter()
                .filter(|c| c.contains("/usr/local/bin/nysia"))
                .count(),
            12
        );
        assert_eq!(installed_count(&target).expect("counts"), 12);
    }

    #[test]
    fn install_creates_a_settings_file_when_there_is_not_one() {
        let scratch = Scratch::new("hooks-fresh");
        let target = scratch.join("nested").join("settings.json");
        let change = install(&target, Path::new("/opt/nysia/nysia")).expect("installs");
        assert_eq!(change.installed, 12);
        assert_eq!(installed_count(&target).expect("counts"), 12);

        // And uninstalling leaves an empty object rather than a `hooks: {}` husk.
        uninstall(&target).expect("uninstalls");
        assert_eq!(fs::read_to_string(&target).expect("reads"), "{}\n");
    }

    #[test]
    fn every_installed_event_points_at_nysia_hook_with_its_own_name() {
        let (_scratch, target) = seeded("hooks-events");
        install(&target, Path::new("/opt/nysia/nysia")).expect("installs");
        let found = commands(&fs::read_to_string(&target).expect("reads"));
        for event in super::super::claude::hooks::EVENTS {
            let expected = format!("'/opt/nysia/nysia' hook --event {event}");
            assert!(found.contains(&expected), "{event} is missing its hook");
        }
    }

    #[test]
    fn an_event_value_this_code_cannot_read_is_refused_not_overwritten() {
        // The policy `HookError::HooksNotAnObject` states, applied one level down. Installing
        // used to replace such a value with a fresh array, so a user whose `hooks.Stop` held
        // an object lost it and was told nothing — `Ok(removed: 0, wrote: true)`.
        let scratch = Scratch::new("hooks-odd-event");
        let target = scratch.join("settings.json");
        let seed = "{\n  \"hooks\": {\n    \"Stop\": {\n      \"x\": 1\n    },\n    \"Keep\": \"me\"\n  }\n}\n";
        fs::write(&target, seed).expect("seed");

        let err = install(&target, Path::new("/opt/nysia/nysia")).expect_err("must refuse");
        assert!(
            matches!(err, HookError::EventNotAnArray { event: "Stop" }),
            "{err:?}"
        );
        assert_eq!(fs::read_to_string(&target).expect("reads"), seed);

        // Uninstall does not refuse: there is nothing of ours in a value we cannot read, so
        // removing nothing from it is both correct and the only answer that lets a user turn
        // the setting off at all.
        let change = uninstall(&target).expect("uninstalls");
        assert_eq!(change.removed, 0);
        assert!(!change.wrote);
        assert_eq!(fs::read_to_string(&target).expect("reads"), seed);
    }

    #[test]
    fn an_event_nysia_does_not_write_may_hold_anything() {
        // The refusal is scoped to the twelve this module installs. A key belonging to
        // somebody else is none of our business whatever shape it is in, and refusing on it
        // would block an install over a value we were never going to touch.
        let scratch = Scratch::new("hooks-foreign-event");
        let target = scratch.join("settings.json");
        fs::write(
            &target,
            "{\n  \"hooks\": {\n    \"SomeFutureEvent\": \"whatever\"\n  }\n}\n",
        )
        .expect("seed");

        assert_eq!(
            install(&target, Path::new("/opt/nysia/nysia"))
                .expect("installs")
                .installed,
            12
        );
        uninstall(&target).expect("uninstalls");
        assert_eq!(
            fs::read_to_string(&target).expect("reads"),
            "{\n  \"hooks\": {\n    \"SomeFutureEvent\": \"whatever\"\n  }\n}\n"
        );
    }

    #[test]
    fn a_byte_order_mark_is_named_rather_than_reported_as_a_column_number() {
        // A Notepad-saved settings file. The parser's own message for this is "expected value
        // at line 1 column 1", which says where and not what.
        let scratch = Scratch::new("hooks-bom");
        let target = scratch.join("settings.json");
        fs::write(&target, "\u{feff}{\n  \"a\": 1\n}\n").expect("seed");

        let err = install(&target, Path::new("/opt/nysia/nysia")).expect_err("must refuse");
        assert!(matches!(err, HookError::ByteOrderMark { .. }), "{err:?}");
        assert!(err.to_string().contains("byte-order mark"));
    }

    #[test]
    fn a_crlf_settings_file_survives_install_and_uninstall_unchanged() {
        // CRLF is what an editor on the primary development platform writes. "As if Nysia had
        // never touched it" has to mean the line endings too, or every line shows as changed.
        let scratch = Scratch::new("hooks-crlf");
        let target = scratch.join("settings.json");
        let seed = REALISTIC.replace('\n', "\r\n");
        fs::write(&target, &seed).expect("seed");

        install(&target, Path::new("/opt/nysia/nysia")).expect("installs");
        let installed = fs::read_to_string(&target).expect("reads");
        assert!(
            !installed.contains("\n\n") && installed.contains("\r\n"),
            "the install rewrote CRLF as LF"
        );
        assert_eq!(
            installed.matches('\n').count(),
            installed.matches("\r\n").count()
        );

        uninstall(&target).expect("uninstalls");
        assert_eq!(fs::read_to_string(&target).expect("reads"), seed);
    }

    #[test]
    fn uninstalling_a_file_with_nothing_of_ours_in_it_changes_nothing() {
        let (_scratch, target) = seeded("hooks-noop");
        let change = uninstall(&target).expect("uninstalls");
        assert_eq!(change.removed, 0);
        assert!(!change.wrote);
        assert_eq!(fs::read_to_string(&target).expect("reads"), REALISTIC);
    }
}
