//! The `nysia` binary's exit-code contract, exercised through the real executable.
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | the process did what was asked |
//! | 1 | it was asked for something valid and could not do it |
//! | 2 | clap's usage error — you typed it wrong |
//!
//! There used to be a third — "parsed and routed, but this build has no implementation" —
//! and `nysia hook` was the only thing that ever answered with it. Wave C implemented the
//! hook, so nothing answers with it any more, and a code documented for a case that cannot
//! arise is a code somebody writes a branch for. It is gone from the table above, from
//! `main.rs`, and from the binary.
//!
//! **`nysia hook` gets its own tests here**, because it is the one mode whose exit code is
//! read by a machine rather than a person: Claude treats exit 2 from a hook as "block, and
//! feed stderr back to the model", which is precisely the influence §5.2 exists to make
//! impossible. So the interesting assertions about it are negative ones — what it never does,
//! however badly it is invoked — and they belong beside the contract they are about.
//!
//! Nothing here starts a daemon. `--daemon` binds a real endpoint now, and the survival test
//! is where that is exercised, with a runtime directory of its own and a process it owns. The
//! hook tests below run with `--no-spawn` against a runtime directory where no daemon is
//! listening, which is the case the disk spool exists for.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The binary cargo just built for this test target.
const NYSIA: &str = env!("CARGO_BIN_EXE_nysia");

/// The payload Claude writes, as the acceptance test spells it.
const STOP_PAYLOAD: &str =
    r#"{"session_id":"exit-codes","hook_event_name":"Stop","is_interrupt":false}"#;

/// What §5.2 has the hook print before it does anything else.
const EMPTY_DECISION: &str = "{}";

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    // No runtime directory is shared with anything: none of these verbs reaches a socket, and
    // pointing them somewhere harmless means a stray one cannot touch a real daemon.
    run_in(&std::env::temp_dir().join("nysia-exit-codes"), args, None, None)
}

/// Run the binary against `runtime_dir`, optionally writing `stdin` and setting a pane hint.
fn run_in(runtime_dir: &Path, args: &[&str], stdin: Option<&str>, pane_key: Option<&str>) -> Run {
    let mut command = Command::new(NYSIA);
    command
        .args(args)
        .env("NYSIA_RUNTIME_DIR", runtime_dir)
        .env("NYSIA_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match pane_key {
        Some(pane) => command.env("NYSIA_PANE_KEY", pane),
        // Removed rather than left alone: this process may itself be running inside a Nysia
        // session, and inheriting its pane would have the spool tests assert against somebody
        // else's key.
        None => command.env_remove("NYSIA_PANE_KEY"),
    };
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    // `unwrap_or_else`, not `expect`: clippy's allow-expect-in-tests only covers `#[test]`
    // functions and `#[cfg(test)]` modules, and this helper is neither.
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("could not run {NYSIA}: {error}"));
    if let Some(payload) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        // The write is allowed to fail: a process that refused its argv has already exited,
        // and a broken pipe here is that, not a fault in the test.
        let _ = pipe.write_all(payload.as_bytes());
    }
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("could not wait for {NYSIA}: {error}"));
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A runtime directory of this test's own, with no daemon listening on it.
fn empty_runtime_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nysia-exit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|error| panic!("could not make a runtime directory: {error}"));
    dir
}

#[test]
fn the_hook_answers_before_it_does_anything_that_could_fail() {
    // §5.2's first of "four details that matter", asserted at its worst moment: stdin is
    // /dev/null here, so there is no payload and everything after the first line fails. The
    // decision is on stdout anyway, which is the whole claim — the agent has its answer
    // before this process has done anything that could go wrong.
    let run = run(&["hook", "--event", "Stop", "--no-spawn"]);
    assert_eq!(
        run.stdout.trim(),
        EMPTY_DECISION,
        "the hook prints {EMPTY_DECISION} first, whatever happens next; got {:?} / {:?}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        run.code,
        Some(1),
        "and an unreadable payload is still a failure it has to report"
    );
}

#[test]
fn a_hook_never_exits_with_the_code_claude_reads_as_block() {
    // The one assertion this file exists for as much as any other. Claude treats exit 2 from
    // a hook as "block, and feed stderr back to the model" — so every way the hook can be
    // invoked badly has to stay away from it, or Nysia's status reporting acquires the power
    // over the agent that §5.2 is written to deny it.
    let dir = empty_runtime_dir("never-two");
    let cases: [(&[&str], Option<&str>); 4] = [
        // No event, no payload.
        (&["hook", "--no-spawn"], None),
        // A payload naming no event, and no flag to fill it in.
        (&["hook", "--no-spawn"], Some(r#"{"session_id":"x"}"#)),
        // A payload that is not JSON at all.
        (&["hook", "--event", "Stop", "--no-spawn"], Some("not json")),
        // A payload and a flag that disagree.
        (
            &["hook", "--event", "Stop", "--no-spawn"],
            Some(r#"{"hook_event_name":"PreToolUse"}"#),
        ),
    ];
    for (args, stdin) in cases {
        let run = run_in(&dir, args, stdin, None);
        assert_ne!(
            run.code,
            Some(2),
            "{args:?} with {stdin:?} must never block the agent, got {:?}",
            run.stderr
        );
        assert_eq!(
            run.stdout.trim(),
            EMPTY_DECISION,
            "{args:?} with {stdin:?} still answers first"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_status_no_daemon_will_take_is_spooled_rather_than_lost() {
    // §2.3's insurance, end to end through the real binary: no daemon is listening on this
    // runtime directory and `--no-spawn` keeps it that way, so the only place the status can
    // go is the disk.
    let dir = empty_runtime_dir("spool");
    let run = run_in(
        &dir,
        &["hook", "--event", "Stop", "--no-spawn"],
        Some(STOP_PAYLOAD),
        Some("tab_1:leaf_1"),
    );
    assert_eq!(
        run.code,
        Some(0),
        "a spooled status is not a failure: {:?}",
        run.stderr
    );
    assert_eq!(run.stdout.trim(), EMPTY_DECISION);

    let spooled: Vec<PathBuf> = std::fs::read_dir(dir.join("agent-status"))
        .unwrap_or_else(|error| panic!("the spool directory should exist: {error}"))
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(spooled.len(), 1, "one pane, one file: {spooled:?}");
    let line = std::fs::read_to_string(&spooled[0])
        .unwrap_or_else(|error| panic!("the spool file should be readable: {error}"));
    let row: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|error| panic!("a spool line is a row: {error}\n{line}"));
    assert_eq!(row["pane"], "tab_1:leaf_1");
    assert_eq!(row["state"], "done", "§2.1 maps Stop to done");
    assert!(
        row["observedAt"].as_u64().is_some_and(|at| at > 0),
        "the row carries the clock of the moment the hook fired, got {row}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hook_outside_a_session_with_no_daemon_says_so_rather_than_inventing_a_pane() {
    // Nothing proved the pane and nothing hinted at one, so there is no row to write. Saying
    // so beats filing the status under a guess: §2.2 makes the pane the row's identity, and a
    // row under the wrong pane is a dot on somebody else's tab.
    let dir = empty_runtime_dir("no-pane");
    let run = run_in(
        &dir,
        &["hook", "--event", "Stop", "--no-spawn", "--json"],
        Some(STOP_PAYLOAD),
        None,
    );
    assert_eq!(run.code, Some(1));
    assert_eq!(run.stdout.trim(), EMPTY_DECISION);
    assert!(
        run.stderr.contains("nextSteps"),
        "--json puts the envelope on stderr, got {:?}",
        run.stderr
    );
    assert!(
        !dir.join("agent-status").exists(),
        "nothing was filed under a pane nobody named"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_event_the_plan_maps_to_nothing_is_dropped_without_failing() {
    // §2.1 calls `PreCompact` deliberately unmapped, and dropping it is the specified
    // behaviour rather than an oversight. The hook succeeded; there was simply no row.
    let dir = empty_runtime_dir("unmapped");
    let run = run_in(
        &dir,
        &["hook", "--event", "PreCompact", "--no-spawn"],
        Some(r#"{"hook_event_name":"PreCompact"}"#),
        Some("tab_1:leaf_1"),
    );
    assert_eq!(run.code, Some(0), "a specified drop is not a failure");
    assert!(
        !dir.join("agent-status").exists(),
        "and an event with no state has nothing to spool"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn typing_it_wrong_is_a_usage_error_and_not_a_failure() {
    for argv in [
        // A verb that does not exist.
        &["orchestration", "ask"][..],
        // The runtime and a client verb at once.
        &["--daemon", "session", "list"][..],
        // A flag that only means something in daemon mode.
        &["--no-idle-retire", "session", "list"][..],
        // Both projections of a read at once.
        &["terminal", "read", "sess_x", "--screen", "--stream"][..],
        // A required argument left out.
        &["terminal", "resize", "sess_x"][..],
    ] {
        assert_eq!(
            run(argv).code,
            Some(2),
            "{argv:?} should be clap's usage exit"
        );
    }
}

#[test]
fn help_and_version_succeed_and_print_to_stdout() {
    for argv in [&["--help"][..], &["--version"][..]] {
        let run = run(argv);
        assert_eq!(run.code, Some(0), "{argv:?} should succeed");
        assert!(!run.stdout.is_empty(), "{argv:?} should print something");
    }

    // Every verb's help mentions --json, because the contract only holds if it holds
    // everywhere: a verb without it is one an agent has to scrape.
    for argv in [
        &["session", "create", "--help"][..],
        &["session", "list", "--help"][..],
        &["session", "close", "--help"][..],
        &["terminal", "read", "--help"][..],
        &["terminal", "send", "--help"][..],
        &["terminal", "resize", "--help"][..],
        &["terminal", "wait", "--help"][..],
        &["agent", "status", "--help"][..],
        &["hook", "--help"][..],
    ] {
        let run = run(argv);
        assert_eq!(run.code, Some(0), "{argv:?} should succeed");
        assert!(
            run.stdout.contains("--json"),
            "{argv:?} should offer --json, got {:?}",
            run.stdout
        );
    }
}

#[test]
fn a_handle_that_is_not_one_fails_before_any_daemon_is_needed() {
    // `--no-spawn` so nothing is started: the argument is wrong, and finding that out should
    // not cost a process. Exit 1 rather than 2 because the shape of the argv was fine — it is
    // the value that was not, and the error envelope is where that is explained.
    let run = run(&["terminal", "read", "not-a-handle", "--no-spawn", "--json"]);
    assert_eq!(run.code, Some(1));
    assert!(
        run.stdout.trim().is_empty(),
        "the result stream stays empty on failure, got {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("nextSteps"),
        "--json should put the error envelope on stderr, got {:?}",
        run.stderr
    );
}
