//! Claude's hook events, and what one of Nysia's entries looks like in `settings.json`.
//!
//! §5.3. Twelve events, each pointing at **`nysia hook`** — a real subcommand of the daemon
//! binary, not a shell script wrapping `curl`. What that removes, relative to Orca, is the
//! HTTP listener, the port, the token file, the endpoint-indirection file rewritten at every
//! app boot, and the Windows `.cmd`-plus-EncodedCommand fallback. All of that exists in Orca
//! because Orca's listener dies with its UI; Nysia's endpoint is stable for the daemon's
//! life (§4), so none of it is reintroduced here.
//!
//! `nysia hook` itself is wave C's. This module only writes entries that point at it.
//!
//! # Recognising an entry as ours
//!
//! By **shape**, not by exact string: an entry is Nysia's when its command runs a program
//! called `nysia` with `hook` as its first argument. Matching the exact string we last wrote
//! would leave an orphan behind the moment the binary moved — an install from
//! `C:/tools/nysia.exe` followed by one from `C:/Program Files/Nysia/nysia.exe` would leave
//! two live hooks, both firing. See [`is_managed`].
//!
//! The cost of shape-matching is stated rather than hidden: a hook a user wrote themselves
//! that runs `nysia hook` is indistinguishable from one Nysia installed, and uninstall
//! removes it. That is the right trade — it *is* a Nysia hook — but it is a real difference
//! from "removes only what we wrote".

use std::path::{Path, PathBuf};

use crate::agent::hooks::{HookChange, HookError};
use crate::agent::ordered_json::{Document, Json};

/// The twelve events §5.1 lists, in the order it lists them.
///
/// Held as one array so that the count and the list cannot drift: the plan's conventions
/// call a stated total a fact about the list beneath it, and
/// [`tests::the_twelve_events_are_the_twelve_the_design_lists`] is what holds them together.
///
/// `Notification` is deliberately absent — "waiting" comes from `PermissionRequest` and from
/// `PreToolUse` where the tool is `AskUserQuestion`. `PreCompact` is deliberately absent too:
/// §5.1 leaves it unmapped, and an event installed but unmapped is a hook that costs the
/// agent latency for nothing.
pub(in crate::agent) const EVENTS: [&str; 12] = [
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "TeammateIdle",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "SessionStart",
    "PostCompact",
];

/// The events whose groups carry a tool matcher, which §5.1 writes as `Event(*)`.
const MATCHER_EVENTS: [&str; 4] = [
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
];

/// The matcher that means "every tool".
const MATCH_EVERYTHING: &str = "*";

/// Seconds Claude will wait for `nysia hook` before giving up on it.
///
/// The hook is on the agent's critical path. `nysia hook` prints `{}` and writes to a local
/// socket, so it should take milliseconds; the cap is there for the case where it does not,
/// because an agent blocked on Nysia's status reporting is strictly worse than an agent with
/// no status at all.
const TIMEOUT_SECONDS: u64 = 5;

/// The key the hook table lives under.
const HOOKS: &str = "hooks";

/// The subcommand the entries invoke.
const VERB: &str = "hook";

/// What the binary is called, without a platform extension.
const BINARY_STEM: &str = "nysia";

/// Where Claude keeps the user-level settings file.
pub(in crate::agent) fn user_settings_path() -> Result<PathBuf, HookError> {
    let home = home_directory().ok_or(HookError::NoHome("Claude's settings.json"))?;
    Ok(home.join(".claude").join("settings.json"))
}

/// The user's home directory.
///
/// Read from the environment rather than from a crate: `HOME` on Unix, `USERPROFILE` on
/// Windows, which is what Claude Code itself resolves `~/.claude` against.
fn home_directory() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The command string for one event.
///
/// Claude runs a hook command through a POSIX shell on **both** platforms — which is why the
/// hooks a Windows install writes carry forward slashes and `sh` quoting rather than a
/// `cmd.exe` line. So the path is written with forward slashes and single-quoted, and an
/// embedded quote is escaped the way `sh` wants it. A user profile directory containing a
/// space is ordinary; one containing an apostrophe is rare and still has to work.
fn command(nysia: &Path, event: &str) -> String {
    let path = nysia.to_string_lossy().replace('\\', "/");
    format!("{} {VERB} --event {event}", single_quote(&path))
}

/// Wrap in single quotes, escaping any single quote by closing, escaping and reopening.
fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// One `{ "type": "command", "command": … , "timeout": … }` entry.
fn entry(nysia: &Path, event: &str) -> Json {
    Json::Object(vec![
        ("type".to_owned(), Json::String("command".to_owned())),
        ("command".to_owned(), Json::String(command(nysia, event))),
        ("timeout".to_owned(), Json::Number(TIMEOUT_SECONDS.into())),
    ])
}

/// The group that holds it — with a matcher for the four events that take one.
fn group(nysia: &Path, event: &str) -> Json {
    let hooks = Json::Array(vec![entry(nysia, event)]);
    if MATCHER_EVENTS.contains(&event) {
        Json::Object(vec![
            (
                "matcher".to_owned(),
                Json::String(MATCH_EVERYTHING.to_owned()),
            ),
            (HOOKS.to_owned(), hooks),
        ])
    } else {
        Json::Object(vec![(HOOKS.to_owned(), hooks)])
    }
}

/// Whether a hook entry is one of Nysia's.
///
/// True when the command runs a program whose file stem is `nysia` and whose first argument
/// is `hook`. Recognised through the spellings an installed entry can legitimately carry:
/// quoted or bare, forward or backward slashes, with or without a `.exe`, and at any
/// directory depth. Not recognised — deliberately — is a command that merely *mentions*
/// `nysia hook` somewhere later in a longer shell line, because that is somebody else's
/// command that happens to call ours, and removing it would take their line with it.
fn is_managed(entry: &Json) -> bool {
    if entry.get("type").and_then(Json::as_str) != Some("command") {
        return false;
    }
    let Some(command) = entry.get("command").and_then(Json::as_str) else {
        return false;
    };
    let Some((program, rest)) = split_first_word(command) else {
        return false;
    };
    if next_word(&rest) != Some(VERB) {
        return false;
    }
    let stem = program
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    stem == BINARY_STEM || stem == format!("{BINARY_STEM}.exe")
}

/// The first shell word of `command`, with its quoting removed, and whatever follows it.
///
/// A real `sh` word: quoted and unquoted runs concatenate, so
/// `'C:/Users/O'\''Brien/nysia.exe'` is one word naming one file, which is what our own
/// writer emits for a home directory with an apostrophe in it.
///
/// **One deliberate deviation from `sh`.** A backslash escapes only a quote or another
/// backslash here; anywhere else it stays a literal character. In `sh` a backslash escapes
/// whatever follows it, so `C:\tools\nysia.exe` would lex as `C:toolsnysia.exe` — and while
/// that is what `sh` would really do with it, the job here is recognising which program an
/// entry names, not executing it, and a Windows path written with separators is the more
/// likely reading of those bytes by a wide margin. Note which way it fails: the deviation
/// can only make a `nysia hook` entry recognisable, never make somebody else's command look
/// like one, because the stem and the verb both still have to match.
///
/// Returns `None` for an empty command or an unterminated quote.
fn split_first_word(command: &str) -> Option<(String, String)> {
    let chars: Vec<char> = command.chars().collect();
    let mut at = 0;
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    if at >= chars.len() {
        return None;
    }

    let escapes = |c: char| c == '\'' || c == '"' || c == '\\';
    let mut word = String::new();
    while at < chars.len() && !chars[at].is_whitespace() {
        match chars[at] {
            quote @ ('\'' | '"') => {
                at += 1;
                while at < chars.len() && chars[at] != quote {
                    if chars[at] == '\\' && chars.get(at + 1).is_some_and(|c| escapes(*c)) {
                        at += 1;
                    }
                    word.push(chars[at]);
                    at += 1;
                }
                if at >= chars.len() {
                    return None;
                }
                at += 1;
            }
            '\\' if chars.get(at + 1).is_some_and(|c| escapes(*c)) => {
                at += 1;
                word.push(chars[at]);
                at += 1;
            }
            other => {
                word.push(other);
                at += 1;
            }
        }
    }
    Some((word, chars[at..].iter().collect()))
}

/// The next whitespace-delimited word of `rest`.
fn next_word(rest: &str) -> Option<&str> {
    rest.split_whitespace().next()
}

/// Install a managed hook for every event, replacing anything a previous install left.
///
/// # Errors
///
/// Returns [`HookError::HooksNotAnObject`] when `hooks` is present but is not an object, and
/// [`HookError::EventNotAnArray`] when one of the twelve events holds something other than
/// the array of groups the schema calls for — so that a settings file shaped in a way this
/// code does not understand is reported rather than quietly overwritten.
pub(in crate::agent) fn install(
    document: &mut Document,
    nysia: &Path,
) -> Result<HookChange, HookError> {
    // Before anything is touched. `purge` mutates the document in place, so a refusal that
    // came after it would leave a half-edited tree behind for the caller to write back.
    refuse_unrecognised_events(&document.value)?;
    let Purged { removed, positions } = purge(document)?;

    let root = &mut document.value;
    if root.get(HOOKS).is_none() {
        root.set(HOOKS, Json::object());
    }
    let Some(table) = root.get_mut(HOOKS) else {
        return Err(HookError::HooksNotAnObject);
    };

    for event in EVENTS {
        let fresh = group(nysia, event);
        match table.get_mut(event) {
            // The event already has groups — put ours back where the old one was, so that a
            // reinstall does not silently reorder a user's hooks against each other.
            Some(Json::Array(groups)) => {
                let was = positions
                    .iter()
                    .find(|(name, _)| name == event)
                    .map_or(usize::MAX, |(_, at)| *at);
                let at = groups.len().min(was);
                groups.insert(at, fresh);
            }
            None => table.set(event, Json::Array(vec![fresh])),
            // Refused above, and `purge` leaves a value it does not recognise alone, so this
            // is unreachable. It is an error rather than a fallthrough because the
            // fallthrough is what destroyed the value: `set` replaced whatever was there.
            Some(_) => return Err(HookError::EventNotAnArray { event }),
        }
    }
    Ok(HookChange {
        installed: EVENTS.len(),
        removed,
        wrote: false,
    })
}

/// Refuse a settings file whose events hold something this code would have to overwrite.
///
/// Only the twelve this module writes. An event Nysia knows nothing about may hold anything
/// at all — it is none of our business and nothing here touches it.
///
/// Install refuses; uninstall does not, and the asymmetry is deliberate. Removing hooks from
/// a value we do not recognise means removing nothing from it, which is both correct and
/// harmless, and refusing there would leave a user unable to turn the setting off because of
/// a key we were never going to edit.
fn refuse_unrecognised_events(root: &Json) -> Result<(), HookError> {
    let Some(table) = root.get(HOOKS) else {
        return Ok(());
    };
    if !matches!(table, Json::Object(_)) {
        return Err(HookError::HooksNotAnObject);
    }
    for event in EVENTS {
        match table.get(event) {
            None | Some(Json::Array(_)) => {}
            Some(_) => return Err(HookError::EventNotAnArray { event }),
        }
    }
    Ok(())
}

/// Remove every managed hook, and every container that only existed to hold one.
///
/// # Errors
///
/// See [`install`].
pub(in crate::agent) fn uninstall(document: &mut Document) -> Result<HookChange, HookError> {
    let Purged { removed, .. } = purge(document)?;
    Ok(HookChange {
        installed: 0,
        removed,
        wrote: false,
    })
}

/// How many of the twelve events currently carry a managed hook.
pub(in crate::agent) fn installed_count(document: &Document) -> usize {
    let Some(table) = document.value.get(HOOKS) else {
        return 0;
    };
    EVENTS
        .iter()
        .filter(|event| {
            table
                .get(event)
                .and_then(Json::as_array)
                .is_some_and(|groups| groups.iter().any(has_managed_entry))
        })
        .count()
}

/// Whether a group holds at least one managed entry.
fn has_managed_entry(group: &Json) -> bool {
    group
        .get(HOOKS)
        .and_then(Json::as_array)
        .is_some_and(|entries| entries.iter().any(is_managed))
}

/// What [`purge`] took out, and where it was.
struct Purged {
    /// How many managed entries were removed.
    removed: usize,
    /// For each event, the index the first managed group held before removal — so that a
    /// reinstall can put ours back in the same place rather than at the end, which would
    /// silently reorder a user's hooks against each other.
    positions: Vec<(String, usize)>,
}

/// Strip every managed entry, tidying away whatever is left empty.
///
/// Tidying is what makes uninstall leave the file as if Nysia had never touched it: a group
/// whose only entry was ours, an event array whose only group was ours, and a `hooks` table
/// whose only events were ours all go, rather than staying behind as empty scaffolding the
/// user never wrote. A group we *shared* with a foreign entry keeps its place and its entry.
fn purge(document: &mut Document) -> Result<Purged, HookError> {
    let mut removed = 0;
    let mut positions = Vec::new();

    let Some(table) = document.value.get_mut(HOOKS) else {
        return Ok(Purged { removed, positions });
    };
    if !matches!(table, Json::Object(_)) {
        return Err(HookError::HooksNotAnObject);
    }

    // Every event in the file, not only our twelve: an install from an older Nysia that knew
    // a thirteenth must still be removable by this one.
    let events: Vec<String> = table.keys().iter().map(|key| (*key).to_owned()).collect();

    for event in events {
        let Some(Json::Array(groups)) = table.get_mut(&event) else {
            continue;
        };
        let mut first_managed: Option<usize> = None;
        let mut index = 0;
        while index < groups.len() {
            let Some(Json::Array(entries)) = groups[index].get_mut(HOOKS) else {
                index += 1;
                continue;
            };
            let before = entries.len();
            entries.retain(|entry| !is_managed(entry));
            let taken = before - entries.len();
            if taken > 0 {
                removed += taken;
                first_managed.get_or_insert(index);
            }
            if entries.is_empty() && taken > 0 {
                groups.remove(index);
                continue;
            }
            index += 1;
        }
        if let Some(at) = first_managed {
            positions.push((event.clone(), at));
        }
        if groups.is_empty() {
            table.remove(&event);
        }
    }

    if table.is_empty_container() {
        document.value.remove(HOOKS);
    }
    Ok(Purged { removed, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_twelve_events_are_the_twelve_the_design_lists() {
        // The plan's conventions: a stated total is a fact about the list beneath it, so the
        // two are held together here rather than trusted to stay in step. This list is
        // §5.1's, in §5.1's order.
        assert_eq!(EVENTS.len(), 12);
        assert_eq!(
            EVENTS.to_vec(),
            vec![
                "UserPromptSubmit",
                "Stop",
                "StopFailure",
                "SubagentStart",
                "SubagentStop",
                "TeammateIdle",
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "PermissionRequest",
                "SessionStart",
                "PostCompact",
            ]
        );
        // The two §5.1 names and leaves out, which are as load-bearing as the twelve it
        // includes: `Notification` because waiting comes from `PermissionRequest`, and
        // `PreCompact` because it is deliberately unmapped.
        assert!(!EVENTS.contains(&"Notification"));
        assert!(!EVENTS.contains(&"PreCompact"));
        // And every matcher event is one of the twelve.
        assert!(MATCHER_EVENTS.iter().all(|event| EVENTS.contains(event)));
    }

    #[test]
    fn a_matcher_event_carries_one_and_the_rest_do_not() {
        let nysia = Path::new("/opt/nysia");
        assert_eq!(
            group(nysia, "PreToolUse")
                .get("matcher")
                .and_then(Json::as_str),
            Some("*")
        );
        assert!(group(nysia, "Stop").get("matcher").is_none());
    }

    #[test]
    fn the_command_is_sh_quoted_with_forward_slashes_on_both_legs() {
        // Claude runs hook commands through a POSIX shell even on Windows, which is why a
        // backslash path would be read as escapes and a spaced path unquoted would split.
        let command = command(Path::new(r"C:\Program Files\Nysia\nysia.exe"), "Stop");
        assert_eq!(
            command,
            "'C:/Program Files/Nysia/nysia.exe' hook --event Stop"
        );
        // An apostrophe in a home directory is rare and still has to survive.
        let odd = command_for(r"C:\Users\O'Brien\nysia.exe");
        assert_eq!(odd, r"'C:/Users/O'\''Brien/nysia.exe' hook --event Stop");
    }

    fn command_for(path: &str) -> String {
        command(Path::new(path), "Stop")
    }

    #[test]
    fn an_entry_is_recognised_through_every_spelling_an_install_can_write() {
        for spelling in [
            "'C:/Program Files/Nysia/nysia.exe' hook --event Stop",
            "\"C:/tools/nysia.exe\" hook --event Stop",
            "/usr/local/bin/nysia hook --event Stop",
            r"C:\tools\nysia.exe hook --event Stop",
            "nysia hook",
            "  'C:/x/NYSIA.EXE'   hook   --event Stop",
            r"'C:/Users/O'\''Brien/nysia.exe' hook --event Stop",
        ] {
            assert!(
                is_managed(&command_entry(spelling)),
                "{spelling:?} should be recognised as ours"
            );
        }
    }

    #[test]
    fn a_foreign_entry_is_never_recognised_as_ours() {
        for spelling in [
            // Somebody else's program, whatever it is called.
            "/usr/local/bin/orca hook --event Stop",
            "curl -s localhost:9000/hook",
            // Our name, their verb.
            "/usr/local/bin/nysia session list",
            // Their line, which happens to call ours later. Removing this would delete a
            // command the user wrote, so the shape match is anchored at the first word.
            "echo hi && nysia hook --event Stop",
            // A program whose name merely starts the same way.
            "/usr/local/bin/nysiad hook",
            // Not a command hook at all.
            "",
        ] {
            assert!(
                !is_managed(&command_entry(spelling)),
                "{spelling:?} is not ours and must survive"
            );
        }
        // A non-command hook type, whatever its command says.
        let other = Json::Object(vec![
            ("type".to_owned(), Json::String("prompt".to_owned())),
            (
                "command".to_owned(),
                Json::String("nysia hook --event Stop".to_owned()),
            ),
        ]);
        assert!(!is_managed(&other));
    }

    /// A `{type: command, command: …}` entry.
    fn command_entry(command: &str) -> Json {
        Json::Object(vec![
            ("type".to_owned(), Json::String("command".to_owned())),
            ("command".to_owned(), Json::String(command.to_owned())),
        ])
    }
}
