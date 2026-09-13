//! The `nysia` binary's exit-code contract, exercised through the real executable.
//!
//! Three codes, and the difference between them is the whole point:
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | the process did what was asked |
//! | 2 | clap's usage error — you typed it wrong |
//! | 3 | parsed and routed, but this build has no implementation |
//!
//! The v0.1 scaffold implements nothing, so every mode returns 3. `--daemon` used to
//! return 0 while binding no socket and serving nobody, which meant a supervisor that
//! spawned it and checked the status read success from a stub.

use std::process::Command;

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
        .output()
        .unwrap_or_else(|error| panic!("could not run {NYSIA}: {error}"));
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn daemon_mode_reports_failure_rather_than_a_silent_success() {
    let run = run(&["--daemon"]);

    assert_eq!(
        run.code,
        Some(3),
        "a stub daemon must not look like a live one"
    );
    assert!(
        run.stdout.is_empty(),
        "stdout must stay empty so a supervisor watching it for a ready line gets nothing, \
         got {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("wave 2"),
        "the message should name the wave that implements it, got {:?}",
        run.stderr
    );
}

#[test]
fn every_client_verb_reports_the_same_unimplemented_code() {
    for args in [
        ["session", "list"].as_slice(),
        ["session", "create", "pwsh"].as_slice(),
        ["terminal", "read"].as_slice(),
        ["hook"].as_slice(),
    ] {
        let run = run(args);
        assert_eq!(run.code, Some(3), "{args:?} should be unimplemented");
        assert!(
            run.stderr.contains("not implemented in the v0.1 scaffold"),
            "{args:?} should say so on stderr, got {:?}",
            run.stderr
        );
    }
}

#[test]
fn usage_errors_stay_distinguishable_from_unimplemented() {
    // Unknown verb.
    assert_eq!(run(&["orchestration", "ask"]).code, Some(2));
    // `--daemon` becomes the runtime and cannot also run a client verb.
    assert_eq!(run(&["--daemon", "session", "list"]).code, Some(2));
}

#[test]
fn help_and_version_succeed() {
    let help = run(&["--help"]);
    assert_eq!(help.code, Some(0));
    assert!(help.stdout.contains("--daemon"));

    assert_eq!(run(&["--version"]).code, Some(0));
}
