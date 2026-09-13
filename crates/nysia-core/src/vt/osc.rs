//! OSC 133 and OSC 7 interception.
//!
//! `vte`'s ANSI layer routes a fixed set of OSC numbers to [`vte::ansi::Handler`] and logs
//! the rest as unhandled — 7 and 133 are both in the "rest". Wrapping the grid in a
//! `Handler` therefore cannot see them, so the sniffer here runs a second, bare
//! [`vte::Parser`] over the same bytes *before* they reach the grid, with a [`Perform`]
//! whose only interesting method is `osc_dispatch`.
//!
//! Running a real parser rather than scanning for `\x1b]` by hand is what buys correct
//! handling of chunk boundaries, `BEL` versus `ST` termination, and the DCS sequences that
//! would otherwise look like an OSC with a different introducer.
//!
//! The sniffer only observes. The bytes still go to the replay ring and still reach the
//! grid, which ignores them.
//!
//! - **OSC 133** is the semantic-prompt protocol: `A` prompt start, `B` prompt end and
//!   therefore input start, `C` command start, `D` command end with an optional exit code.
//!   This is the "block" model for a human typing. Anything Nysia runs on an agent's
//!   behalf takes its exit status from `wait()` instead (§7.2).
//! - **OSC 7** reports the shell's working directory as a `file://` URL.

use std::fmt;
use std::path::PathBuf;

use alacritty_terminal::vte::{Parser, Perform};

/// Where the shell is in the prompt/command cycle, as reported by OSC 133.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandState {
    /// Nothing has been reported yet, or the shell does not emit OSC 133 at all.
    #[default]
    Unknown,
    /// The shell is drawing its prompt (`OSC 133;A`).
    Prompt,
    /// The prompt is drawn and the user is typing (`OSC 133;B`).
    Input,
    /// A command is executing (`OSC 133;C`).
    Running,
    /// The last command finished (`OSC 133;D`), with its exit code when one was reported.
    Finished {
        /// The exit code the shell attached to the mark, if any.
        exit_code: Option<i32>,
    },
}

/// What the OSC sniffer has learned about the shell running in a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellState {
    /// The prompt/command state from OSC 133.
    pub command: CommandState,
    /// The working directory from OSC 7, when the shell reports one.
    pub cwd: Option<PathBuf>,
}

/// Intercepts OSC 133 and OSC 7 from a session's raw output.
///
/// Feed every byte that goes to the grid, in order. The parser is stateful across calls,
/// so a sequence split across two reads is still recognised.
#[derive(Default)]
pub struct OscSniffer {
    parser: Parser,
    sink: OscSink,
}

impl OscSniffer {
    /// A sniffer that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe a chunk of raw output.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.parser.advance(&mut self.sink, chunk);
    }

    /// What the shell has reported so far.
    #[must_use]
    pub fn state(&self) -> &ShellState {
        &self.sink.state
    }
}

// `vte::Parser` is not `Debug`, and a dump of its state machine would be noise anyway.
// What a reader of a session dump wants is what the sniffer concluded.
impl fmt::Debug for OscSniffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OscSniffer")
            .field("state", &self.sink.state)
            .finish_non_exhaustive()
    }
}

/// The `Perform` half of [`OscSniffer`]. Every method except `osc_dispatch` keeps the
/// trait's default no-op body, because this parser exists only to watch.
#[derive(Debug, Default)]
struct OscSink {
    state: ShellState,
}

impl Perform for OscSink {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params.first().copied() {
            Some(b"133") => self.semantic_prompt(&params[1..]),
            Some(b"7") => {
                if let Some(url) = params.get(1) {
                    self.state.cwd = parse_file_url(url);
                }
            }
            _ => {}
        }
    }
}

impl OscSink {
    /// Apply an `OSC 133` mark. `rest` is everything after the `133` parameter.
    fn semantic_prompt(&mut self, rest: &[&[u8]]) {
        let Some(kind) = rest.first().and_then(|p| p.first()) else {
            return;
        };
        self.state.command = match kind {
            b'A' => CommandState::Prompt,
            b'B' => CommandState::Input,
            b'C' => CommandState::Running,
            b'D' => CommandState::Finished {
                exit_code: rest.get(1).and_then(|code| parse_exit_code(code)),
            },
            // `P;k=…` and friends carry shell metadata that does not move the state
            // machine. Leave the state alone rather than guessing.
            _ => return,
        };
    }
}

/// Read the decimal exit code an `OSC 133;D` mark carries, rejecting anything else so a
/// malformed mark reports "no code" instead of a wrong one.
fn parse_exit_code(raw: &[u8]) -> Option<i32> {
    let text = std::str::from_utf8(raw).ok()?;
    text.parse().ok()
}

/// Turn the `file://<host>/<path>` URL of an `OSC 7` report into a path.
///
/// The host half is discarded: under D-17 WSL is a plain shell with no path translation,
/// so a URL naming another host is still only useful as the local-looking path it carries.
/// Percent escapes are decoded, and the leading slash Windows URLs put before the drive
/// letter (`file:///C:/src`) is dropped.
fn parse_file_url(raw: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(raw).ok()?;
    let rest = text.strip_prefix("file://")?;
    // Everything up to the first `/` is the host.
    let path = &rest[rest.find('/')?..];
    let decoded = percent_decode(path)?;
    let trimmed = if cfg!(windows) {
        decoded
            .strip_prefix('/')
            .filter(|rest| looks_like_a_drive_path(rest))
            .unwrap_or(&decoded)
            .to_owned()
    } else {
        decoded
    };
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Whether a URL path with its leading slash removed starts with a `C:` style drive.
fn looks_like_a_drive_path(rest: &str) -> bool {
    let mut chars = rest.chars();
    matches!((chars.next(), chars.next()), (Some(c), Some(':')) if c.is_ascii_alphabetic())
}

/// Decode `%XX` escapes. Returns `None` for a truncated or non-hex escape, because a cwd
/// that cannot be read exactly is worse than no cwd at all.
fn percent_decode(text: &str) -> Option<String> {
    if !text.contains('%') {
        return Some(text.to_owned());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(bytes: &[u8]) -> ShellState {
        let mut sniffer = OscSniffer::new();
        sniffer.feed(bytes);
        sniffer.state().clone()
    }

    #[test]
    fn nothing_reported_leaves_the_state_unknown() {
        let state = sniff(b"plain output\r\n");
        assert_eq!(state.command, CommandState::Unknown);
        assert_eq!(state.cwd, None);
    }

    #[test]
    fn walks_the_osc_133_prompt_cycle() {
        let mut sniffer = OscSniffer::new();
        sniffer.feed(b"\x1b]133;A\x07$ ");
        assert_eq!(sniffer.state().command, CommandState::Prompt);
        sniffer.feed(b"\x1b]133;B\x07ls");
        assert_eq!(sniffer.state().command, CommandState::Input);
        sniffer.feed(b"\x1b]133;C\x07");
        assert_eq!(sniffer.state().command, CommandState::Running);
        sniffer.feed(b"\x1b]133;D;0\x07");
        assert_eq!(
            sniffer.state().command,
            CommandState::Finished { exit_code: Some(0) }
        );
    }

    #[test]
    fn a_command_end_without_a_code_still_finishes() {
        assert_eq!(
            sniff(b"\x1b]133;D\x07").command,
            CommandState::Finished { exit_code: None }
        );
        assert_eq!(
            sniff(b"\x1b]133;D;nonsense\x07").command,
            CommandState::Finished { exit_code: None }
        );
    }

    #[test]
    fn st_terminated_marks_are_recognised_too() {
        assert_eq!(
            sniff(b"\x1b]133;D;130\x1b\\").command,
            CommandState::Finished {
                exit_code: Some(130)
            }
        );
    }

    #[test]
    fn a_mark_split_across_two_feeds_is_still_recognised() {
        let mut sniffer = OscSniffer::new();
        sniffer.feed(b"\x1b]13");
        sniffer.feed(b"3;C");
        assert_eq!(sniffer.state().command, CommandState::Unknown);
        sniffer.feed(b"\x07");
        assert_eq!(sniffer.state().command, CommandState::Running);
    }

    #[test]
    fn unknown_osc_133_subcommands_do_not_move_the_state() {
        let mut sniffer = OscSniffer::new();
        sniffer.feed(b"\x1b]133;C\x07");
        sniffer.feed(b"\x1b]133;P;k=i\x07");
        assert_eq!(sniffer.state().command, CommandState::Running);
    }

    #[test]
    fn osc_7_reports_the_working_directory() {
        let state = sniff(b"\x1b]7;file://host/home/kacper/src\x07");
        assert_eq!(state.cwd, Some(PathBuf::from("/home/kacper/src")));
    }

    #[test]
    fn osc_7_decodes_percent_escapes() {
        let state = sniff(b"\x1b]7;file://host/home/a%20b\x07");
        assert_eq!(state.cwd, Some(PathBuf::from("/home/a b")));
    }

    #[test]
    fn osc_7_rejects_a_truncated_escape_rather_than_guessing() {
        assert_eq!(sniff(b"\x1b]7;file://host/home/a%2\x07").cwd, None);
        assert_eq!(sniff(b"\x1b]7;not-a-url\x07").cwd, None);
        assert_eq!(sniff(b"\x1b]7;file://host\x07").cwd, None);
    }

    #[test]
    #[cfg(windows)]
    fn osc_7_drops_the_slash_before_a_windows_drive_letter() {
        let state = sniff(b"\x1b]7;file:///C:/Users/kacper\x07");
        assert_eq!(state.cwd, Some(PathBuf::from("C:/Users/kacper")));
    }

    #[test]
    #[cfg(windows)]
    fn osc_7_keeps_the_slash_when_the_path_is_not_a_drive() {
        let state = sniff(b"\x1b]7;file:///mnt/c/src\x07");
        assert_eq!(state.cwd, Some(PathBuf::from("/mnt/c/src")));
    }

    #[test]
    fn other_osc_numbers_are_ignored() {
        // OSC 0 is a title set; it must not be mistaken for a cwd or a prompt mark.
        let state = sniff(b"\x1b]0;some title\x07");
        assert_eq!(state, ShellState::default());
    }
}
