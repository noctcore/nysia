//! The `nysia` binary's exit-code contract, exercised through the real executable.
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | the process did what was asked |
//! | 1 | it was asked for something valid and could not do it |
//! | 2 | clap's usage error — you typed it wrong |
//! | 3 | parsed and routed, but this build has no implementation |
//!
//! Three and one are worth keeping apart. "This build cannot do that" is a fact about the
//! build that no retry will change; "that did not work" is a fact about one attempt, and the
//! error envelope that comes with it says whether retrying would help. A caller that saw one
//! code for both would have to read prose to tell them apart.
//!
//! Nothing here starts a daemon. `--daemon` binds a real endpoint now, and the survival test
//! is where that is exercised, with a runtime directory of its own and a process it owns.

use std::process::{Command, Stdio};

/// The binary cargo just built for this test target.
const NYSIA: &str = env!("CARGO_BIN_EXE_nysia");

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    // `unwrap_or_else`, not `expect`: clippy's allow-expect-in-tests only covers `#[test]`
    // functions and `#[cfg(test)]` modules, and this helper is neither.
    let output = Command::new(NYSIA)
        .args(args)
        // No runtime directory is shared with anything: none of these verbs reaches a socket,
        // and pointing them somewhere harmless means a stray one cannot touch a real daemon.
        .env(
            "NYSIA_RUNTIME_DIR",
            std::env::temp_dir().join("nysia-exit-codes"),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("could not run {NYSIA}: {error}"));
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn a_verb_this_build_does_not_serve_says_which_version_does() {
    let run = run(&["hook"]);

    assert_eq!(
        run.code,
        Some(3),
        "an unimplemented verb is not a usage error and is not a failure"
    );
    assert!(
        run.stdout.is_empty(),
        "stdout carries results; there is no result here, got {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("v0.2"),
        "the message should name the version that implements it, got {:?}",
        run.stderr
    );
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
