//! Resolving the Claude CLI — traps register #8, and the first code in this repository to
//! exercise it.
//!
//! `Command::new("claude")` fails on Windows with `NotFound` even when `claude` is plainly
//! on `PATH`, because npm's `cmd-shim` installs three files side by side — `claude` (a `sh`
//! script, for Git Bash), `claude.cmd` and `claude.ps1` — and none of them is a real
//! executable. `CreateProcess` launches real executables only. The resolution is a
//! `which`-style lookup that tries `PATHEXT` **before** the bare name and pre-validates what
//! it finds, which is [`crate::pty::resolve`]'s job; this module is the one that names the
//! program.
//!
//! The lookup already existed for shell profiles. What did not exist, until the tests below,
//! was anything that ran it against the shim layout it was written for: v0.1 ships no agent
//! sessions, so trap #8 sat in the register for the whole of v0.1 with nothing exercising
//! it. See [`tests`] for which leg proves what.

use std::ffi::OsStr;

use crate::agent::launch::{AgentLaunch, LaunchError};
use crate::pty::resolve;

/// What the Claude CLI is called on `PATH`.
///
/// A bare name on purpose: resolving it is the point. Nothing here may grow into a
/// configurable path without also growing the validation that `resolve` performs, and a
/// path baked in at compile time would be traps register #9.
pub(super) const PROGRAM: &str = "claude";

/// The agent's name, for messages.
pub(super) const AGENT: &str = "claude";

/// Resolve the Claude CLI and build the argument vector to launch it with.
///
/// # Errors
///
/// Returns [`LaunchError::Unavailable`] when `claude` is not on `PATH`, is not a file, or is
/// not something this platform can launch — reported *before* the spawn, because neither
/// platform reports it usefully afterwards.
pub(in crate::agent) fn launch<I, S>(args: I) -> Result<AgentLaunch, LaunchError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let resolved = resolve(PROGRAM).map_err(|source| LaunchError::Unavailable {
        agent: AGENT,
        source,
    })?;
    let argv = resolved
        .argv(args)
        .map_err(|source| LaunchError::Unavailable {
            agent: AGENT,
            source,
        })?;
    Ok(AgentLaunch::new(AGENT, resolved.program.clone(), argv))
}

/// Traps register #8, proved on the leg that has it.
///
/// # Which leg proves what
///
/// **Windows is where the trap lives**, and
/// [`tests::windows_a_bare_claude_resolves_to_its_cmd_shim_rather_than_notfound`] is the
/// proof: it puts the full npm shim triple on `PATH`, asserts that `Command::new("claude")`
/// still fails with `NotFound` — the trap, live, in this repository — and that the resolved
/// launch runs the `.cmd` and comes back with the shim's own output.
///
/// **macOS does nothing here, and that is the honest answer.** npm on Unix installs `claude`
/// as a single executable file with a shebang, so there is no shim to resolve past and
/// `Command::new("claude")` works. The Unix test is named
/// [`tests::unix_there_is_no_shim_so_a_bare_claude_is_already_the_executable`] so that it
/// reads as the asymmetry it is rather than as a second copy of the coverage. It still earns
/// its place: it asserts that `PATHEXT` handling stays Windows-only, which is what would
/// break if the extension search ever ran unconditionally, and it pins the claim that the
/// bare name is launchable there rather than leaving it as prose.
///
/// # How the `PATH` is arranged
///
/// By re-exec, not by [`std::env::set_var`]. `cargo test` runs tests on threads in one
/// process, `resolve` reads `PATH` on every call, and mutating the environment while another
/// thread reads it is a data race that POSIX does not defend against — which is why the
/// 2024 edition made `set_var` `unsafe`. The outer test therefore builds the shim directory,
/// spawns *this same test binary* at the `#[ignore]`d child test with `PATH` prepended, and
/// asserts on its output. No shared state, and nothing to serialise against the pty tests
/// next door.
///
/// # What it does not cover
///
/// The spawn here is [`std::process::Command`], not a pty. That is deliberate and it is
/// already on the record: `pty/resolve.rs` notes that a `cmd /c` one-shot inside a ConPTY
/// never reports its exit through `try_wait` and its output never reaches the master. So
/// this proves resolution-and-launch, which is the whole of trap #8; it does not prove a
/// pty-hosted agent session, which needs a `ShellProfile` that does not exist yet and is
/// wave C's to add.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::scratch::Scratch;
    use std::path::Path;
    use std::process::Command;

    /// Set by the outer test; carries the directory it wrote the shims into.
    const SHIM_DIR: &str = "NYSIA_TRAP8_SHIM_DIR";

    /// What the fake CLI prints, so a pass means the shim actually ran.
    const MARKER: &str = "NYSIA-TRAP8-SHIM-RAN";

    /// Printed by a child test that got all the way through, so the outer test can tell
    /// "passed" from "was filtered out and never ran".
    const CHILD_OK: &str = "NYSIA-TRAP8-CHILD-OK";

    /// Write the three files npm's `cmd-shim` installs side by side.
    ///
    /// All three on both platforms, deliberately: on Windows the extensionless `sh` script
    /// is the wrong answer that a bare-name-first search would return, and on Unix the two
    /// Windows shims are the files that must be ignored because `PATHEXT` is not a thing
    /// there. Each leg's wrong answer is present for the other leg's right one to be a
    /// result rather than a coincidence.
    fn write_npm_shims(dir: &Path) {
        std::fs::write(dir.join(PROGRAM), format!("#!/bin/sh\necho {MARKER}\n"))
            .expect("the sh shim");
        std::fs::write(
            dir.join(format!("{PROGRAM}.cmd")),
            format!("@echo off\r\necho {MARKER}\r\n"),
        )
        .expect("the cmd shim");
        std::fs::write(
            dir.join(format!("{PROGRAM}.ps1")),
            format!("Write-Output '{MARKER}'\r\n"),
        )
        .expect("the ps1 shim");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join(PROGRAM), std::fs::Permissions::from_mode(0o755))
                .expect("the sh shim is executable");
        }
    }

    /// This test's own libtest name, which is its module path minus the crate.
    fn child_test_name(leaf: &str) -> String {
        let path = module_path!();
        let without_crate = path.split_once("::").map_or(path, |(_, rest)| rest);
        format!("{without_crate}::{leaf}")
    }

    /// Run `leaf` in a fresh copy of this test binary, with `dir` as the **whole** of `PATH`.
    ///
    /// The whole of it, not prepended to the parent's. Prepending leaves the developer's own
    /// `claude` reachable behind the fixture, so "it resolved" would not say which one it
    /// resolved and "it was not found" could not be asserted at all.
    fn drive_child(leaf: &str, dir: &Path) -> (bool, String) {
        let exe = std::env::current_exe().expect("the test binary");
        let path =
            std::env::join_paths([dir.to_path_buf()]).expect("a PATH without a separator in it");

        let output = Command::new(exe)
            .args([
                "--exact",
                &child_test_name(leaf),
                "--ignored",
                "--nocapture",
            ])
            .env("PATH", path)
            .env(SHIM_DIR, dir)
            .output()
            .expect("the child test binary runs");

        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    }

    /// The directory the outer test put the shims in.
    fn shim_dir() -> std::path::PathBuf {
        std::env::var_os(SHIM_DIR)
            .map(std::path::PathBuf::from)
            .expect("the outer test sets the shim directory")
    }

    // -----------------------------------------------------------------------------------
    // Windows: the leg the trap is on.
    // -----------------------------------------------------------------------------------

    #[test]
    #[cfg(windows)]
    fn windows_a_bare_claude_resolves_to_its_cmd_shim_rather_than_notfound() {
        let scratch = Scratch::new("trap8-windows");
        write_npm_shims(scratch.path());
        let (ok, output) = drive_child("windows_child_resolves_the_shim", scratch.path());
        assert!(ok, "the child test failed:\n{output}");
        assert!(
            output.contains(CHILD_OK),
            "the child was filtered out instead of running:\n{output}"
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "driven by its outer test, which writes the npm shims and prepends PATH"]
    fn windows_child_resolves_the_shim() {
        let dir = shim_dir();

        // 1. The trap itself, live. `claude` is on PATH — all three npm files are sitting in
        //    that directory — and the bare name still cannot be spawned, because
        //    `CreateProcess` launches real executables and a `.cmd` is a script.
        let err = Command::new(PROGRAM)
            .output()
            .expect_err("a bare `claude` must not spawn on Windows");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "traps register #8 says this is NotFound; it was {err:?}"
        );

        // 2. Resolution finds the `.cmd`, not the extensionless `sh` script beside it.
        let launch = launch(["--version"]).expect("`claude` must resolve through its shim");
        assert!(
            launch
                .program()
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe")),
            "a batch shim is launched through cmd.exe, not directly: {:?}",
            launch.program()
        );
        assert!(
            launch
                .argv()
                .iter()
                .any(|arg| Path::new(arg) == dir.join(format!("{PROGRAM}.cmd"))),
            "the resolved argv must name the .cmd shim: {:?}",
            launch.argv()
        );

        // 3. And it actually runs. Resolution that produces an unlaunchable argv would
        //    satisfy the two assertions above and still be the same bug.
        let output = launch
            .command()
            .output()
            .expect("the resolved launch spawns");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(MARKER),
            "the shim did not run; stdout was {stdout:?}"
        );

        println!("{CHILD_OK}");
    }

    // -----------------------------------------------------------------------------------
    // Unix: no shim, so nothing to resolve past. Named to say so.
    // -----------------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn unix_there_is_no_shim_so_a_bare_claude_is_already_the_executable() {
        let scratch = Scratch::new("trap8-unix");
        write_npm_shims(scratch.path());
        let (ok, output) = drive_child("unix_child_launches_the_bare_name", scratch.path());
        assert!(ok, "the child test failed:\n{output}");
        assert!(
            output.contains(CHILD_OK),
            "the child was filtered out instead of running:\n{output}"
        );
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "driven by its outer test, which writes the npm shims and prepends PATH"]
    fn unix_child_launches_the_bare_name() {
        let dir = shim_dir();

        // 1. The asymmetry, asserted rather than asserted-in-prose: the spawn that fails on
        //    Windows succeeds here, because npm installs a real executable with a shebang
        //    and there is no shim in the way.
        let direct = Command::new(PROGRAM)
            .output()
            .expect("a bare `claude` spawns on Unix, which is why trap #8 is Windows-only");
        assert!(String::from_utf8_lossy(&direct.stdout).contains(MARKER));

        // 2. Resolution agrees: the bare file itself, launched directly, with no interpreter
        //    in front of it. `claude.cmd` and `claude.ps1` are sitting in the same directory
        //    and must be ignored — PATHEXT is a Windows idea and running the extension
        //    search unconditionally would pick one of them here.
        let launch = launch(["--version"]).expect("`claude` must resolve");
        assert_eq!(launch.program(), dir.join(PROGRAM));
        assert_eq!(
            launch.argv().len(),
            2,
            "no interpreter args: {:?}",
            launch.argv()
        );

        let output = launch
            .command()
            .output()
            .expect("the resolved launch spawns");
        assert!(String::from_utf8_lossy(&output.stdout).contains(MARKER));

        println!("{CHILD_OK}");
    }

    // -----------------------------------------------------------------------------------
    // Both legs.
    // -----------------------------------------------------------------------------------

    #[test]
    fn a_missing_claude_is_reported_before_the_spawn_rather_than_after() {
        let scratch = Scratch::new("trap8-missing");
        // An empty directory as the whole of PATH: `claude` is definitively not there.
        let (ok, output) = drive_child("child_reports_a_missing_claude", scratch.path());
        assert!(ok, "the child test failed:\n{output}");
        assert!(output.contains(CHILD_OK), "the child never ran:\n{output}");
    }

    #[test]
    #[ignore = "driven by its outer test, which points PATH at an empty directory"]
    fn child_reports_a_missing_claude() {
        // PATH is exactly one empty directory, so `claude` is definitively absent — this is
        // the error path, not a maybe. What it pins is that absence is a typed error from
        // resolution rather than a spawn that fails later, which on Unix is a child that
        // dies silently inside the pty (wezterm#7893) and on Windows is a bare `NotFound`
        // with nothing saying which program.
        let Err(LaunchError::Unavailable { agent, source }) = launch(["--version"]) else {
            panic!("`claude` must not resolve against an empty PATH");
        };
        assert_eq!(agent, AGENT);
        assert!(
            matches!(&source, crate::pty::ResolveError::NotFound { program } if program == PROGRAM),
            "a missing CLI is NotFound naming the program, not {source:?}"
        );
        println!("{CHILD_OK}");
    }
}
