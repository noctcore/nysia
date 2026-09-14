//! Environment hygiene for a spawned session.
//!
//! Nysia is itself frequently launched from inside a Claude Code session, and Claude Code
//! marks its own environment. A `claude` launched with that environment still in place is
//! classified as a *child* session rather than a fresh one, which is how an agent ends up
//! read-only and confused about which conversation it belongs to. The three
//! `CLAUDE_CODE_*SESSION*` variables are the ones that actually do it; `CLAUDECODE` and
//! `CLAUDE_CODE_ENTRYPOINT` describe the launch, and the two `ANTHROPIC_*` variables would
//! silently redirect an agent's credentials and endpoint (§7.1).
//!
//! Removal is by name, never by clearing the block. `env_clear` would take
//! `SSH_AUTH_SOCK`, the Windows credential-manager variables and everything else a git
//! credential helper needs with it, and the failure would surface much later as an
//! unexplained authentication prompt.

use portable_pty::CommandBuilder;

/// The variables scrubbed from every session Nysia spawns.
///
/// `portable-pty` matches environment keys case-insensitively on Windows, so one spelling
/// covers the registry-derived block too.
pub const SCRUBBED_VARS: &[&str] = &[
    // Claude Code's own launch markers.
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    // The three that make a launched `claude` think it is somebody's child session.
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    // Credentials and endpoint: inheriting these silently reroutes an agent.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
];

/// The terminal type every Nysia session reports.
///
/// Forced rather than inherited: the grid is a full `alacritty_terminal`, and a session
/// that inherited `TERM=dumb` from a CI runner would have its colours and cursor
/// addressing stripped by the child for no reason.
pub const FORCED_TERM: &str = "xterm-256color";

/// The colour depth every Nysia session reports.
pub const FORCED_COLORTERM: &str = "truecolor";

/// Scrub the inherited environment and force the terminal description.
///
/// Applied to every session, agent or shell, **after** any caller-supplied override, so
/// that layering cannot undo it. A scrub a caller can reintroduce a variable through is not
/// a scrub, it is a default — and three of these decide whether a launched `claude` is
/// treated as somebody's child session while two are a credential and an endpoint.
///
/// `SessionSpec::with_env_overriding_the_scrub` is the one deliberate way past this, and it
/// is named so the call site is greppable. It is also the only one: the list it appends to
/// is a private field, so there is no struct literal or direct `push` that reaches around
/// the name.
pub fn sanitize(command: &mut CommandBuilder) {
    for name in SCRUBBED_VARS {
        command.env_remove(name);
    }
    command.env("TERM", FORCED_TERM);
    command.env("COLORTERM", FORCED_COLORTERM);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_with(vars: &[(&str, &str)]) -> CommandBuilder {
        let mut command = CommandBuilder::new("does-not-matter");
        for (key, value) in vars {
            command.env(key, value);
        }
        command
    }

    #[test]
    fn every_listed_variable_is_removed() {
        let mut command = command_with(
            &SCRUBBED_VARS
                .iter()
                .map(|name| (*name, "inherited"))
                .collect::<Vec<_>>(),
        );
        for name in SCRUBBED_VARS {
            assert!(command.get_env(name).is_some(), "{name} should be set");
        }

        sanitize(&mut command);

        for name in SCRUBBED_VARS {
            assert!(command.get_env(name).is_none(), "{name} should be scrubbed");
        }
    }

    #[test]
    fn the_three_child_session_markers_are_covered() {
        // Called out separately because these are the ones that misclassify a launched
        // `claude`; the rest are hygiene.
        for name in [
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_BRIDGE_SESSION_ID",
        ] {
            assert!(SCRUBBED_VARS.contains(&name), "{name} must be scrubbed");
        }
    }

    #[test]
    fn term_and_colorterm_are_forced_over_whatever_was_inherited() {
        let mut command = command_with(&[("TERM", "dumb"), ("COLORTERM", "")]);
        sanitize(&mut command);
        assert_eq!(command.get_env("TERM"), Some(FORCED_TERM.as_ref()));
        assert_eq!(
            command.get_env("COLORTERM"),
            Some(FORCED_COLORTERM.as_ref())
        );
    }

    #[test]
    fn unrelated_variables_survive_because_credential_helpers_need_them() {
        // The reason this is `env_remove` and not `env_clear`.
        let mut command = command_with(&[
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
            ("HOME", "/home/kacper"),
            ("PATH", "/usr/bin"),
        ]);
        sanitize(&mut command);
        assert_eq!(
            command.get_env("SSH_AUTH_SOCK"),
            Some("/tmp/agent.sock".as_ref())
        );
        assert_eq!(command.get_env("HOME"), Some("/home/kacper".as_ref()));
        assert_eq!(command.get_env("PATH"), Some("/usr/bin".as_ref()));
    }

    #[test]
    #[cfg(windows)]
    fn scrubbing_is_case_insensitive_on_windows() {
        // The base environment on Windows is merged from the registry in whatever casing
        // the registry used, so a case-sensitive removal would miss it.
        let mut command = command_with(&[("claude_code_session_id", "inherited")]);
        sanitize(&mut command);
        assert!(command.get_env("CLAUDE_CODE_SESSION_ID").is_none());
        assert!(command.get_env("claude_code_session_id").is_none());
    }
}
