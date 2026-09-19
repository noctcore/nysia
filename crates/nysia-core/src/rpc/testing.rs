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

/// A `session_create` as a CLI from before `cwd` became an object sends it: the folder as a
/// bare string, which this daemon cannot read. The folder's last component is distinctive so
/// a leak check cannot pass merely because the name was short or ordinary.
///
/// **Valid JSON, and it has to stay valid.** A frame that is not JSON at all fails as a
/// syntax error, whose message quotes nothing, so a leak check fed one passes whatever the
/// daemon does with serde's message. The first draft of this constant lost a level of
/// backslashes and was exactly that. `the_old_shape_fixture_is_json_in_the_wrong_shape` holds
/// it to the case that can leak.
pub(crate) const OLD_SHAPE_CREATE: &str = concat!(
    r#"{"type":"session_create","requestId":"req_11111111-1111-4111-8111-111111111111","#,
    r#""retryRequest":null,"kind":"shell","paneKey":null,"profile":null,"#,
    r#""cwd":"C:\\Users\\kacpe\\Projekty\\a-clients-private-repo","#,
    r#""envOverrides":{},"cols":80,"rows":24}"#,
    "\n"
);

/// Every event `tracing` emits on this thread while it is installed, one line per event.
///
/// Written out against `tracing` itself rather than taken from `tracing-subscriber`, which
/// this crate does not depend on. It records every field of every event at every level —
/// `debug` included, which is the level a person raises the log to when they are about to
/// send the file to somebody — and nothing about spans, which no check here reads.
///
/// Install it with `tracing::subscriber::set_default`, which is scoped to the thread. That
/// is enough for a current-thread tokio test: every task it spawns runs on that one thread.
#[derive(Clone, Default)]
pub(crate) struct Recorder {
    lines: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Recorder {
    /// Everything recorded so far.
    pub(crate) fn text(&self) -> String {
        self.lines
            .lock()
            .map(|lines| lines.clone())
            .unwrap_or_default()
    }
}

impl tracing::Subscriber for Recorder {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields<'a>(&'a mut String);

        impl tracing::field::Visit for Fields<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                let _ = write!(self.0, "{}={value:?} ", field.name());
            }
        }

        if let Ok(mut lines) = self.lines.lock() {
            lines.push_str(event.metadata().level().as_str());
            lines.push(' ');
            event.record(&mut Fields(&mut lines));
            lines.push('\n');
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
