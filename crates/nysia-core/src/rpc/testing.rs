//! Scaffolding the rpc tests share, so the awkward parts are got right in one place.
//!
//! Test-only, and compiled out of every other build.

use nysia_proto::ShellProfile as WireProfile;

/// Every wait in an rpc test is bounded; a test that can hang is a test that will.
pub(crate) const DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// The token the chosen shell computes. It appears in no line that is typed.
pub(crate) const TOKEN: &str = "NYSIA-42";

/// The shell these tests drive, and the lines that make it compute a token.
///
/// `pwsh` where it is installed — CI has it on both runners and it is what §9 names — and
/// the platform's own shell where it is not, so a developer machine without PowerShell 7
/// still exercises the pump rather than skipping the test that proves it works.
pub(crate) struct TestShell {
    /// The profile to spawn, or `None` for the platform's default shell.
    pub(crate) profile: Option<WireProfile>,
    /// Lines that make the shell compute [`TOKEN`].
    pub(crate) lines: Vec<&'static str>,
    /// A line that makes the shell print a great deal of output.
    pub(crate) flood: &'static str,
    /// The command that prints a file, followed by a space and the file's name.
    ///
    /// Named relative to the shell's own working directory, which is what makes it a probe:
    /// a file only one folder holds is printed only by a shell that is in that folder, so
    /// the answer is the directory the session actually got rather than the one it was
    /// asked for.
    pub(crate) show_file: &'static str,
}

impl TestShell {
    /// Pick a shell and the commands that make it *compute* `NYSIA-42`.
    ///
    /// Computed, never typed: matching on a token that also appears in the line as typed
    /// means kernel echo plus a redisplay satisfies the assertion with the shell having
    /// run nothing at all. Every line below either arithmetic-expands or expands a
    /// variable, so the token can only come from the shell.
    ///
    /// The flood is sized at roughly 128 KiB, which is well past any per-stream allowance
    /// these tests configure — the point of it is to keep going after the opening credit is
    /// spent, which only an interop that agrees about credit can do.
    pub(crate) fn pick() -> Self {
        if crate::pty::resolve("pwsh").is_ok() {
            return Self {
                profile: Some(WireProfile::Pwsh),
                lines: vec![r#"Write-Output ("NYSIA" + "-" + (6*7))"#],
                flood: r#"1..500 | ForEach-Object { ('{0:d4}' -f $_) + ('X' * 250) }"#,
                show_file: "Get-Content",
            };
        }
        if cfg!(windows) {
            // `cmd` expands `%NYS%` when it *parses* the line, so the assignment has to be
            // a separate command — which also keeps `42` out of everything that is typed.
            return Self {
                profile: Some(WireProfile::Cmd),
                lines: vec!["set /a NYS=6*7", "echo NYSIA-%NYS%"],
                flood: concat!(
                    "for /L %i in (1,1,500) do @echo %i",
                    "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                    "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                    "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                    "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                    "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                ),
                show_file: "type",
            };
        }
        Self {
            profile: None,
            lines: vec![r#"echo "NYSIA-$((6*7))""#],
            flood: concat!(
                r#"i=1; while [ $i -le 500 ]; do printf '%04d%s\n' "$i" "#,
                r#""XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"#,
                "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
                r#"XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"; i=$((i+1)); done"#,
            ),
            show_file: "cat",
        }
    }
}
