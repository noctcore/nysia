//! `nysia hook`: a Claude hook payload on stdin, a status row in the daemon, and `{}` first.
//!
//! §5.2, and it is a mode rather than a verb because its output contract is the opposite of
//! every other one's. A verb puts its result on stdout when it has finished; this puts `{}`
//! on stdout **before it has started**, because stdout here is not a result — it is Claude's
//! hook decision, and an empty decision returned immediately is the whole mechanism by which
//! Nysia cannot block or influence the agent.
//!
//! So [`run`] answers on its first line and then does the work. Nothing above it in this file
//! can fail before that line, nothing below it can unsay it, and the acceptance test asserts
//! the pane's own screen carries it — the only place it is observable, and another reason
//! that test runs the hook inside a session.
//!
//! # What Orca needs here and Nysia does not
//!
//! Orca's hook is a shell script around `curl`, and almost all of it exists because its
//! listener dies with its UI (§4): an HTTP server, a port, a shared token, an
//! endpoint-indirection file rewritten at every app start, and — on Windows — a `.cmd`
//! wrapper invoking PowerShell with an EncodedCommand. **None of that is here.** Nysia's
//! endpoint is stable for the daemon's life, authority comes from peer credentials and the
//! process tree (§3.2), and the same code runs on both platforms.
//!
//! What survives the subtraction is the disk spool, and only for the case §5.3 leaves: the
//! daemon is restarting when a hook fires.
//!
//! # `--event` is this file's business and reaches nothing else
//!
//! [`AgentHook`] carries a pane hint and the event, and nothing else — `hook_event_name` is
//! required inside the payload, so the daemon never sees the flag and cannot compare the two.
//! `nysia-proto`'s own docs say the resulting two rules are the CLI's, and this is the CLI:
//!
//! 1. A payload with no event name is filled in from the flag, **before** the typed event is
//!    built, because a payload missing it does not deserialize at all.
//! 2. A payload and a flag that both name an event and disagree is **refused**. Picking a
//!    winner would make it silent, and a hook entry wired to the wrong event name
//!    misclassifies every status it ever reports — a `PreToolUse` entry spelled `Stop` paints
//!    a pane done on every tool call. This is the only place in Nysia where both values exist
//!    at the same instant, so it is the only place the mistake can be caught at all.
//!
//! # Exit codes, and the one Claude reads as an instruction
//!
//! `0` when the status was handed over, when it was spooled for the next daemon, and when
//! §2.1 maps the event to nothing. `1` when the hook was asked to do something it cannot —
//! a payload it cannot read, an event that disagrees with its flag, or a status it can
//! neither deliver nor spool.
//!
//! **Never `2`.** Claude reads exit 2 from a hook as "block, and feed stderr back to the
//! model", which is precisely the influence §5.2 exists to make impossible. Exit 1 is not
//! that: it is a non-blocking failure whose stderr a person can read. The one route to a 2
//! is clap's usage exit, reachable only by a hook entry whose argv is malformed — which the
//! installer does not write and which is worth failing loudly rather than hiding.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use nysia_core::rpc::{Discovered, Endpoint, PANE_KEY_VAR, SpawnPolicy, discover, spool};
use nysia_proto::{
    AgentHook, AgentStatusRow, ClientRole, HookEvent, HookEventName, PaneKey, UnixMillis,
};

use crate::cli::HookArgs;
use crate::verbs::VerbError;

/// What Claude reads as "no decision": the hook has nothing to say about this event.
const EMPTY_DECISION: &str = "{}";

/// The key Claude puts the event name under. Claude's spelling, not Nysia's.
const EVENT_NAME: &str = "hook_event_name";

/// How long the whole hand-off may take before the status is spooled instead.
///
/// §5.2 caps Orca's at ~1.5 seconds because it is an HTTP round trip that can meet a dead
/// listener. Nysia's is a local socket to a process that either exists or does not, so this
/// is not a budget so much as a backstop — and it is a backstop the agent is not waiting on,
/// because `{}` has already been printed. The hook entries Nysia installs give Claude its own
/// five-second cap on top, which this must stay well inside.
const HAND_OFF_TIMEOUT: Duration = Duration::from_millis(1_500);

/// What running a hook came to.
#[derive(Debug)]
enum Outcome {
    /// The daemon has it.
    Delivered,
    /// No daemon would take it, and it is on disk for the next one to drain.
    Spooled(PathBuf),
    /// §2.1 maps the event to no state, so there was never a row (`PreCompact` and friends).
    Unmapped(HookEventName),
    /// The hook could not do what it was asked.
    Refused(VerbError),
}

/// Answer Claude, then get the event to the daemon.
///
/// Returns `false` when the hook was asked for something it could not do, which the caller
/// turns into exit 1. It never returns a value that would have Claude block.
pub async fn run(args: &HookArgs) -> bool {
    // First. Before stdin is read, before an endpoint is resolved, before anything below can
    // fail. §5.2's first of "four details that matter".
    answer_now();

    let observed_at = now();
    let outcome = match payload() {
        Ok(payload) => match event(&payload, args.event.as_deref()) {
            Ok(event) => hand_off(&event, observed_at).await,
            Err(refusal) => Outcome::Refused(refusal),
        },
        Err(refusal) => Outcome::Refused(refusal),
    };
    report(&outcome, args.json)
}

/// Print the empty decision and flush it, so it is on its way before anything else runs.
///
/// Not `println!` alone: stdout is line-buffered when it is a terminal and **block**-buffered
/// when it is a pipe, which is exactly what Claude gives a hook. Without the flush the
/// decision would sit in this process's buffer for the whole hand-off — which is the one
/// thing printing it first was for.
fn answer_now() {
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{EMPTY_DECISION}");
    let _ = stdout.flush();
}

/// Read the whole payload off stdin.
///
/// Unbounded on purpose. A `PreToolUse` for a `Write` carries the file being written and can
/// be far larger than the control line cap — and the wire drops a `tool_input` that is not a
/// question anyway, so the large payload is read, mapped to `working`, and never sent. A cap
/// here would refuse exactly the events that are cheapest to serve.
fn payload() -> Result<String, VerbError> {
    let mut payload = String::new();
    std::io::stdin()
        .read_to_string(&mut payload)
        .map_err(|err| {
            refusal(
                format!("could not read the hook payload on stdin: {err}"),
                "check the hook entry pipes Claude's JSON payload to `nysia hook` on stdin",
            )
        })?;
    Ok(payload)
}

/// A byte-order mark, which is not content and must not reach the JSON parser.
///
/// Stripped because a hook's stdin is whatever the shell in front of it produced, and at
/// least one shell in Nysia's own tests produces one: PowerShell writes the pipe using
/// `$OutputEncoding`, and a UTF-8 encoding with a preamble puts `U+FEFF` at the head of it.
///
/// `str::trim` does **not** remove it — `U+FEFF` is not `char::is_whitespace` — so without
/// this the parser reports "expected value at line 1 column 1" for a payload that is
/// otherwise perfect, and the status is lost to a character nobody wrote. It cost a
/// debugging session to find from that message; the acceptance test's `pwsh` branch is where
/// it appears, which is the branch both CI legs take.
const BYTE_ORDER_MARK: char = '\u{feff}';

/// Read the payload as an event, applying the flag rules this module's docs set out.
fn event(payload: &str, flag: Option<&str>) -> Result<HookEvent, VerbError> {
    let payload = payload.trim_start_matches(BYTE_ORDER_MARK).trim();
    let mut document: serde_json::Value = serde_json::from_str(payload).map_err(|err| {
        // The error, never the payload: it can carry a `waiting` question (trap 13).
        refusal(
            format!("the hook payload on stdin is not JSON: {err}"),
            "check the hook entry passes Claude's payload through unmodified",
        )
    })?;
    let object = document.as_object_mut().ok_or_else(|| {
        refusal(
            "a hook payload is a JSON object",
            "check the hook entry passes Claude's payload through unmodified",
        )
    })?;

    // Absent, `null`, blank and "not a string" all count as missing. A payload that spelled
    // the name as a number has not named an event, and letting the flag fill it in is the
    // same answer as for a payload that omitted it.
    let named = object
        .get(EVENT_NAME)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let flag = flag.map(str::trim).filter(|name| !name.is_empty());

    let name = match (named, flag) {
        (Some(payload_name), Some(flag_name)) if payload_name != flag_name => {
            return Err(refusal(
                format!(
                    "the hook payload says {payload_name:?} and --event says {flag_name:?}; a \
                     hook entry wired to the wrong event misclassifies every status it reports"
                ),
                "fix the hook entry so --event matches the event it is installed under, or \
                 reinstall the hooks",
            ));
        }
        (Some(name), _) => name.to_owned(),
        (None, Some(name)) => name.to_owned(),
        (None, None) => {
            return Err(refusal(
                "the hook payload names no event and --event was not given",
                "pass --event <EventName>, as Claude's installed hook entries do",
            ));
        }
    };
    object.insert(EVENT_NAME.to_owned(), serde_json::Value::String(name));

    serde_json::from_value(document).map_err(|err| {
        // The error's *category*, never its text. Unlike the syntax error above — which only
        // ever names a line and a column — serde's data errors quote the value that was
        // wrong: a payload carrying `"is_interrupt": "<a secret>"` refuses with
        // `invalid type: string "<a secret>", expected a boolean`, and stderr is somewhere
        // Claude can capture. Trap 13 is about a `waiting` question, and this is the same
        // payload by another route.
        refusal(
            format!(
                "the hook payload is not one this build can read ({:?} error)",
                err.classify()
            ),
            "check the hook entry passes Claude's payload through unmodified",
        )
    })
}

/// Get `event` to the daemon, spooling it when no daemon will take it.
async fn hand_off(event: &HookEvent, observed_at: UnixMillis) -> Outcome {
    let endpoint = match Endpoint::from_env() {
        Ok(endpoint) => endpoint,
        Err(err) => {
            // Without a runtime directory there is nowhere to send *and* nowhere to spool,
            // so this is the one transport failure the hook cannot absorb.
            return Outcome::Refused(refusal(
                format!("could not resolve the nysia runtime directory: {err}"),
                "set NYSIA_RUNTIME_DIR to a writable directory",
            ));
        }
    };
    let hint = pane_hint();
    let hook = AgentHook {
        pane_hint: hint.clone(),
        event: event.clone(),
    };

    match tokio::time::timeout(HAND_OFF_TIMEOUT, deliver(&endpoint, hook)).await {
        Ok(Ok(())) => return Outcome::Delivered,
        Ok(Err(err)) => tracing::debug!(%err, "the daemon did not take this hook; spooling it"),
        Err(_) => tracing::debug!(
            "the daemon did not answer within {HAND_OFF_TIMEOUT:?}; spooling this hook"
        ),
    }
    spool_it(endpoint.runtime_dir(), event, hint.as_ref(), observed_at)
}

/// Send the event over the socket.
///
/// **Never spawns a daemon**, whatever `--no-spawn` says, and the reason is not politeness
/// about the critical path. A daemon this hook started owns no sessions, so the ancestry walk
/// §3.2 requires would find nothing and the hook would be refused by the very daemon it had
/// just paid to start. Spooling reaches the same place — the next real daemon — without
/// starting a process from inside somebody's pty.
async fn deliver(endpoint: &Endpoint, hook: AgentHook) -> Result<(), Box<dyn std::error::Error>> {
    let Discovered { client, .. } = discover(
        endpoint,
        &crate::verbs::client_id(),
        ClientRole::Control,
        &SpawnPolicy::Never,
    )
    .await?;
    let mut client = client;
    client.agent_hook(hook).await?;
    Ok(())
}

/// Put the row on disk for the next daemon to drain (§2.3).
///
/// The row is built **here**, with this process's clock, rather than by the daemon that will
/// later read it. `Store::restore_status` compares a rehydrated row's `observed_at` against
/// the live rows already stored, and a row restamped at drain time would be newer than all of
/// them — so a status that failed at 09:50 would overwrite the one that succeeded at 09:55.
///
/// §2.2 says `observed_at` is when the *daemon* received the event. For a spooled row there
/// was no daemon to receive it, and the closest honest answer is when the hook fired.
fn spool_it(
    runtime_dir: &std::path::Path,
    event: &HookEvent,
    hint: Option<&PaneKey>,
    observed_at: UnixMillis,
) -> Outcome {
    let Some(pane) = hint else {
        return Outcome::Refused(refusal(
            "no daemon took this status and there is no pane to spool it under: \
             NYSIA_PANE_KEY is unset, so this hook is not running inside a Nysia session",
            "run `nysia hook` from inside a Nysia session — Claude's installed hook entries \
             already do",
        ));
    };
    // `to_row` is §2.1's mapping and its `tool_input` confinement in one call, so an event
    // that maps to nothing never reaches the disk and a payload that is not a question never
    // does either (trap 13).
    let Some(row) = event.to_row(pane.clone(), observed_at) else {
        return Outcome::Unmapped(event.hook_event_name.clone());
    };
    spool_row(runtime_dir, &row)
}

/// Append one row, naming what happened.
fn spool_row(runtime_dir: &std::path::Path, row: &AgentStatusRow) -> Outcome {
    match spool::append(runtime_dir, row) {
        Ok(path) => Outcome::Spooled(path),
        Err(err) => Outcome::Refused(refusal(
            format!("no daemon took this status and it could not be spooled: {err}"),
            "check the nysia runtime directory is writable, then start a daemon with \
             `nysia session list`",
        )),
    }
}

/// The pane this process believes it is in, from the environment.
///
/// A **hint** (§3.2) and never the proof: the daemon resolves the pane from the process tree
/// and only logs a hint that disagrees. It matters in exactly one place — the spool, where
/// the daemon that could have proved anything is the daemon that was not there — and a
/// rehydrated row lands `restored_unconfirmed`, which §2.3 says never counts as fresh.
///
/// A malformed value is dropped rather than refused. It is a hint; refusing one would lose a
/// status to protect a field the daemon overrules anyway.
fn pane_hint() -> Option<PaneKey> {
    let raw = std::env::var(PANE_KEY_VAR).ok()?;
    match raw.parse() {
        Ok(pane) => Some(pane),
        Err(err) => {
            tracing::debug!(%err, "ignoring a malformed pane hint");
            None
        }
    }
}

/// Say what happened on stderr, and answer whether the hook did what it was asked.
///
/// Never on stdout: stdout carried the decision and a second document after it is a parse
/// error for whoever reads one.
fn report(outcome: &Outcome, json: bool) -> bool {
    match outcome {
        Outcome::Delivered => true,
        Outcome::Spooled(path) => {
            tracing::info!(
                path = %path.display(),
                "no daemon took this status; it is spooled for the next daemon start"
            );
            true
        }
        Outcome::Unmapped(event) => {
            tracing::debug!(%event, "§2.1 maps this event to no state; dropping it");
            true
        }
        Outcome::Refused(error) => {
            crate::verbs::print_error(error, json);
            false
        }
    }
}

/// This process's clock, in Unix milliseconds.
fn now() -> UnixMillis {
    UnixMillis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
    )
}

/// Something the hook was asked to do and could not.
///
/// [`VerbError::argument`] rather than a hand-built [`ErrorEnvelope`], and the reason is a
/// rule rather than a preference. `NextSteps::new` is fallible — it refuses a blank first
/// step — and it has no infallible sibling, so a function here that had to return an envelope
/// whatever happened needed an arm for a case its own literals make impossible. That arm was
/// an `unreachable!`, which is a panic outside tests and `main`, and the rule has no
/// exemption for a provably dead one.
///
/// Routing through the error type every other verb already uses removes the arm rather than
/// silencing it, and it means a hook failure and a verb failure reach a caller through the
/// same function — which is what §6.2 is about anyway.
fn refusal(message: impl Into<String>, next_step: &'static str) -> VerbError {
    VerbError::argument(message, next_step)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The event, or the envelope a caller would actually be shown.
    ///
    /// The refusals are asserted through `envelope()` rather than through the `VerbError`
    /// itself, because the envelope is what reaches stderr — and trap 13 is a claim about
    /// what a person can read, not about which type held it on the way there.
    fn read(payload: &str, flag: Option<&str>) -> Result<HookEvent, nysia_proto::ErrorEnvelope> {
        event(payload, flag).map_err(|err| err.envelope())
    }

    #[test]
    fn a_payload_that_names_its_own_event_is_taken_as_it_is() {
        let parsed = read(r#"{"hook_event_name":"Stop"}"#, None).expect("a named event reads");
        assert_eq!(parsed.hook_event_name, HookEventName::Stop);
        assert_eq!(parsed.state(), Some(nysia_proto::AgentState::Done));
    }

    #[test]
    fn a_missing_event_name_is_filled_in_from_the_flag() {
        // Rule 1 from `nysia-proto`'s doc. It has to happen before the typed event is built,
        // because `hook_event_name` is required and a payload without one does not
        // deserialize at all — so this is not a convenience, it is the only order that works.
        let parsed = read(r#"{"session_id":"abc"}"#, Some("Stop")).expect("the flag fills it in");
        assert_eq!(parsed.hook_event_name, HookEventName::Stop);

        for blank in [r#"{"hook_event_name":null}"#, r#"{"hook_event_name":"  "}"#] {
            assert_eq!(
                read(blank, Some("Stop"))
                    .expect("a blank name is a missing name")
                    .hook_event_name,
                HookEventName::Stop
            );
        }
    }

    #[test]
    fn a_payload_and_a_flag_that_disagree_are_refused() {
        // Rule 2, and the reason it is a refusal rather than a preference: a `PreToolUse`
        // entry spelled `Stop` paints the pane done on every tool call the agent makes, and
        // picking a winner would make that silent. This is the only place both values exist.
        let err = read(r#"{"hook_event_name":"PreToolUse"}"#, Some("Stop"))
            .expect_err("a disagreement is refused");
        assert!(
            err.message().contains("PreToolUse"),
            "got {}",
            err.message()
        );
        assert!(err.message().contains("Stop"), "got {}", err.message());
        assert!(!err.next_steps().is_empty());
    }

    #[test]
    fn a_payload_and_a_flag_that_agree_are_taken() {
        assert_eq!(
            read(r#"{"hook_event_name":"Stop"}"#, Some("Stop"))
                .expect("agreement is not a conflict")
                .hook_event_name,
            HookEventName::Stop
        );
    }

    #[test]
    fn naming_no_event_at_all_is_refused_rather_than_guessed() {
        let err = read(r#"{"session_id":"abc"}"#, None).expect_err("there is no event to report");
        assert!(!err.next_steps().is_empty());
    }

    #[test]
    fn the_fields_claude_writes_and_nysia_does_not_read_are_tolerated() {
        // The payload the acceptance test sends, which is what actually arrives. A hook that
        // failed because Claude added a field would stop reporting status at exactly the
        // moment the version changed.
        let parsed = read(
            r#"{"session_id":"acceptance","transcript_path":"/dev/null","cwd":".",
                "hook_event_name":"Stop","is_interrupt":false,"permission_mode":"default"}"#,
            Some("Stop"),
        )
        .expect("extra fields are ignored");
        assert_eq!(parsed.hook_event_name, HookEventName::Stop);
        assert!(!parsed.is_interrupt);
    }

    #[test]
    fn an_unknown_event_name_reads_rather_than_failing_the_hook() {
        // §5.2 puts this on the agent's critical path: a hook that refused an event name it
        // had not met would stop reporting status the day Claude adds one.
        let parsed = read(r#"{"hook_event_name":"SomethingNew"}"#, None).expect("it still reads");
        assert_eq!(parsed.state(), None, "and maps to nothing, which is a drop");
    }

    #[test]
    fn a_byte_order_mark_in_front_of_the_payload_costs_nothing() {
        // Found by running the acceptance test's `pwsh` line by hand rather than by reading
        // it: PowerShell writes a native command's stdin using `$OutputEncoding`, and a UTF-8
        // encoding with a preamble puts `U+FEFF` at the head of the pipe. `str::trim` leaves
        // it — it is not `char::is_whitespace` — so the parser blamed column 1 of a payload
        // that was perfect, and the pane's dot never moved.
        let parsed = read("\u{feff}{\"hook_event_name\":\"Stop\"}", None)
            .expect("a byte-order mark is not content");
        assert_eq!(parsed.hook_event_name, HookEventName::Stop);

        // And with the newline PowerShell adds after it, which is the real shape.
        let parsed = read("\u{feff}{\"hook_event_name\":\"Stop\"}\r\n", None)
            .expect("a mark and a trailing newline are both not content");
        assert_eq!(parsed.hook_event_name, HookEventName::Stop);
    }

    #[test]
    fn a_payload_that_is_not_an_object_is_refused_without_being_echoed() {
        for bad in ["[]", "\"Stop\"", "7"] {
            let err = read(bad, Some("Stop")).expect_err("a hook payload is an object");
            assert!(!err.next_steps().is_empty());
        }
        // Trap 13: what could not be read is never repeated back.
        let err = read(r#"{"hook_event_name":"Stop","#, None).expect_err("truncated JSON");
        assert!(
            !err.message().contains("hook_event_name\":\"Stop"),
            "the payload must not be echoed, got {}",
            err.message()
        );
    }

    #[test]
    fn a_field_of_the_wrong_type_is_refused_without_quoting_what_was_in_it() {
        // The second echo route, and the one that is easy to miss: serde's *syntax* errors
        // name a line and a column, but its *data* errors quote the offending value. A
        // payload carrying `"is_interrupt": "<a secret>"` refused with
        // `invalid type: string "<a secret>", expected a boolean` until this was narrowed to
        // the error's category — and stderr is somewhere Claude can capture.
        let secret = "sk-ant-not-a-real-key";
        let err = read(
            &format!(r#"{{"hook_event_name":"Stop","is_interrupt":"{secret}"}}"#),
            None,
        )
        .expect_err("a boolean field holding a string is not readable");
        assert!(
            !err.message().contains(secret),
            "the payload must not be echoed, got {}",
            err.message()
        );
        assert!(!err.next_steps().is_empty());

        // The same rule for `tool_input`, which is the field trap 13 is actually named for.
        let err = read(
            &format!(r#"{{"hook_event_name":"Stop","agent_id":{{"nested":"{secret}"}}}}"#),
            None,
        )
        .expect_err("an object where a string belongs is not readable");
        assert!(
            !err.message().contains(secret),
            "the payload must not be echoed, got {}",
            err.message()
        );
    }

    #[test]
    fn a_spooled_row_carries_the_hooks_own_clock() {
        // The reason the row is built here rather than at drain time: the store compares a
        // rehydrated row's `observed_at` against the live rows it already holds.
        let parsed = read(r#"{"hook_event_name":"Stop"}"#, None).expect("a named event reads");
        let pane = PaneKey::new("tab_1", "leaf_1").expect("a well-formed pane key");
        let row = parsed
            .to_row(pane, UnixMillis(1_757_721_600_000))
            .expect("Stop maps to done");
        assert_eq!(row.observed_at, UnixMillis(1_757_721_600_000));
        assert!(
            !row.restored_unconfirmed,
            "the store flags it, not the hook"
        );
    }
}
