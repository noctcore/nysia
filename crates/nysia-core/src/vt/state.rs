//! The authoritative per-session terminal state.
//!
//! Terminal state lives in Rust and the webview is a display cache (D-7). Everything a
//! client or an agent can learn about a session's output is derived here: the rendered
//! screen, the logical line log, the raw replay ring and the shell state OSC 133 and OSC 7
//! report.
//!
//! # Why the grid's scrollback is not the scrollback
//!
//! `alacritty_terminal` exposes no "how many rows scrolled out" counter, so the only way
//! to learn that a row left the viewport is to watch the grid's history grow. That reading
//! is exact only while the history has not saturated. [`TerminalState::feed`] therefore
//! treats the grid history as a *staging area*: input is sliced into [`FEED_SLICE`]-byte
//! pieces, and after each piece the rows that entered history are
//! copied into the line log and the history is cleared. History is zero at every slice
//! boundary, so the reading is always exact.
//!
//! Nothing is lost by that, because nothing reads the grid history: `--screen` is the
//! viewport, scrollback text is the line log, and a reattaching client replays raw bytes.
//! The one visible consequence is that growing the window does not pull previously
//! scrolled rows back down into it, the way a plain terminal emulator would.
//!
//! A slice of `N` bytes can only contain `N` newlines, so a flood of newlines — the
//! realistic case — can never outrun the staging headroom. A deliberately crafted run of
//! `CSI n S` can, since one short sequence scrolls a whole screen; that is counted by
//! [`TerminalState::overflows`] and warned about rather than silently swallowed.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use super::line_log::{LineLog, LogicalLine};
use super::osc::{OscSniffer, ShellState};
use super::replay::ReplayRing;

/// How much input is handed to the parser before the grid's history is drained. See the
/// module docs: this is the bound that makes scroll-out detection exact.
pub const FEED_SLICE: usize = 1024;

/// How many rows the grid may stage before the drain. Four times [`FEED_SLICE`], so that
/// even a slice that is nothing but newlines has headroom to spare.
const STAGING_ROWS: usize = FEED_SLICE * 4;

/// What a read of a session's output should return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadMode {
    /// The rendered screen: what a human looking at the pane would see.
    ///
    /// This is Nysia's default, and a deliberate divergence from Orca, whose default
    /// returns the accumulated escape-stripped stream — so a `clear` typed one key at a
    /// time reads back as `cclclecleaclear` (§7.2).
    #[default]
    Screen,
    /// The logical lines that have scrolled out of the viewport, from a cursor, followed
    /// by what is still on screen.
    Stream,
}

/// The result of a read, with the cursor a caller should present next time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRead {
    /// Which projection produced [`TerminalRead::text`].
    pub mode: ReadMode,
    /// The text itself, newline-separated and free of escape sequences.
    pub text: String,
    /// The cursor to pass to the next [`ReadMode::Stream`] read to continue where this one
    /// stopped.
    pub next_cursor: u64,
    /// The oldest line id still retained. A caller whose cursor was below this missed
    /// lines, and this is how it finds out.
    pub oldest_cursor: u64,
}

/// How much state a session retains.
///
/// Every field is a ceiling, and between them they are the whole of a session's memory
/// footprint. That matters more here than it would in a per-window terminal: under D-1 one
/// daemon owns every session, so an unbounded one does not leak its own process, it takes
/// all the others down with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtConfig {
    /// How many logical lines the line log keeps before evicting the oldest.
    pub line_log_lines: usize,
    /// How many bytes of logical-line text the line log keeps, the uncommitted head
    /// included. Without this, a stream containing no newline at all — which is every row
    /// soft-wrapped and so no line ever finished — grows the log forever.
    pub line_log_bytes: usize,
    /// How many raw bytes the replay ring keeps.
    pub replay_bytes: usize,
}

impl Default for VtConfig {
    fn default() -> Self {
        Self {
            line_log_lines: 10_000,
            line_log_bytes: 4 * 1024 * 1024,
            replay_bytes: 256 * 1024,
        }
    }
}

/// A terminal's dimensions in cells.
///
/// `alacritty_terminal` asks for a [`Dimensions`], and its own implementation of one is
/// test-gated, so this is the crate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    /// Columns, at least one.
    pub cols: u16,
    /// Rows, at least one.
    pub rows: u16,
}

impl TerminalSize {
    /// A size clamped to at least one cell in each direction, because a zero-sized grid
    /// panics inside the grid's index arithmetic.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

/// Collects the replies `alacritty_terminal` wants written back to the pty.
///
/// Device-status and device-attribute queries (`CSI 6 n`, `CSI c`, …) are answered by the
/// emulator, not by the shell, and a program that asks one waits for the answer. Dropping
/// them — which is what a void listener does — hangs `vim` and anything else that measures
/// the terminal before drawing.
#[derive(Debug, Clone, Default)]
struct ReplySink {
    pending: Arc<Mutex<Vec<u8>>>,
}

impl ReplySink {
    /// Take everything queued since the last call.
    fn take(&self) -> Vec<u8> {
        let mut guard = self.pending.lock().unwrap_or_else(|err| err.into_inner());
        std::mem::take(&mut *guard)
    }
}

impl EventListener for ReplySink {
    fn send_event(&self, event: Event) {
        // Only `PtyWrite` has to reach the child. Titles, bells and clipboard requests are
        // the UI's business and W4 routes them; they are deliberately dropped here rather
        // than queued into the child's input.
        if let Event::PtyWrite(text) = event {
            let mut guard = self.pending.lock().unwrap_or_else(|err| err.into_inner());
            guard.extend_from_slice(text.as_bytes());
        }
    }
}

/// One session's terminal state: the grid, the replay ring, the line log and the shell
/// state derived from OSC marks.
pub struct TerminalState {
    term: Term<ReplySink>,
    processor: Processor,
    replies: ReplySink,
    osc: OscSniffer,
    replay: ReplayRing,
    lines: LineLog,
    size: TerminalSize,
    overflows: u64,
}

impl TerminalState {
    /// Build the state for a session of `size`.
    #[must_use]
    pub fn new(size: TerminalSize, config: VtConfig) -> Self {
        let replies = ReplySink::default();
        let term_config = Config {
            scrolling_history: STAGING_ROWS,
            ..Config::default()
        };
        Self {
            term: Term::new(term_config, &size, replies.clone()),
            processor: Processor::new(),
            replies,
            osc: OscSniffer::new(),
            replay: ReplayRing::with_capacity(config.replay_bytes),
            lines: LineLog::with_capacity(config.line_log_lines, config.line_log_bytes),
            size,
            overflows: 0,
        }
    }

    /// Apply a chunk of a session's raw output.
    ///
    /// The chunk reaches the replay ring whole, the OSC sniffer whole, and the grid in
    /// [`FEED_SLICE`]-byte pieces so that scroll-out detection stays exact.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.replay.push(chunk);
        self.osc.feed(chunk);

        for slice in chunk.chunks(FEED_SLICE) {
            self.processor.advance(&mut self.term, slice);
            self.drain_scrolled_rows();
        }

        // A `BSU` with no matching `ESU` would otherwise buffer output indefinitely.
        if self
            .processor
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|deadline| deadline <= std::time::Instant::now())
        {
            self.processor.stop_sync(&mut self.term);
            self.drain_scrolled_rows();
        }
    }

    /// Take the bytes the emulator owes the child in reply to its queries.
    ///
    /// The caller writes these to the pty. They are not part of the child's output and
    /// never reach the replay ring.
    #[must_use]
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.replies.take()
    }

    /// Resize the grid. The caller is responsible for resizing the pty itself.
    ///
    /// Narrowing the window reflows soft-wrapped content, which pushes rows into the
    /// staging history exactly as scrolling does, so the drain runs here too. Without it
    /// those rows would be logged on the next [`TerminalState::feed`], out of order with
    /// whatever that feed scrolled.
    pub fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.term.resize(size);
        self.drain_scrolled_rows();
    }

    /// The current grid size.
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// Whether the child has switched to the alternate screen.
    ///
    /// The alternate grid has no scrollback by construction, so nothing that happens while
    /// it is active reaches the line log — a full-screen editor does not fill an agent's
    /// read with redraw frames.
    #[must_use]
    pub fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// What the shell has reported through OSC 133 and OSC 7.
    #[must_use]
    pub fn shell_state(&self) -> &ShellState {
        self.osc.state()
    }

    /// The raw bytes a reattaching client should be replayed.
    #[must_use]
    pub fn replay(&self) -> Vec<u8> {
        self.replay.snapshot()
    }

    /// The logical lines that have scrolled out of the viewport.
    #[must_use]
    pub fn line_log(&self) -> &LineLog {
        &self.lines
    }

    /// How many slices saturated the staging history and may therefore have lost rows.
    #[must_use]
    pub fn overflows(&self) -> u64 {
        self.overflows
    }

    /// The rendered viewport: what a human looking at the pane would see, with trailing
    /// blank rows removed.
    #[must_use]
    pub fn screen(&self) -> String {
        let last_row = Line(i32::from(self.size.rows) - 1);
        let last_column = Column(self.size.columns().saturating_sub(1));
        let rendered = self.term.bounds_to_string(
            Point::new(Line(0), Column(0)),
            Point::new(last_row, last_column),
        );
        trim_trailing_blank_lines(&rendered)
    }

    /// Read a session's output.
    ///
    /// [`ReadMode::Screen`] ignores `cursor` and `limit` and renders the viewport.
    /// [`ReadMode::Stream`] returns the logical lines at or after `cursor`, at most `limit`
    /// of them, and appends the current viewport once the log is exhausted — so a short
    /// command's output is complete even before it has scrolled anywhere.
    ///
    /// [`TerminalRead::next_cursor`] is where the *returned* page stopped, not where the
    /// log currently ends: a caller that pages with it sees every line exactly once.
    #[must_use]
    pub fn read(&self, mode: ReadMode, cursor: u64, limit: usize) -> TerminalRead {
        let (text, next_cursor) = match mode {
            ReadMode::Screen => (self.screen(), self.lines.next_id()),
            ReadMode::Stream => self.stream(cursor, limit),
        };
        TerminalRead {
            mode,
            text,
            next_cursor,
            oldest_cursor: self.lines.oldest_id(),
        }
    }

    /// The `--stream` projection: scrolled-out lines from `cursor`, then the viewport.
    ///
    /// Returns the text and the cursor the next read should present. The viewport is
    /// appended only when the page was not cut short by `limit`, because a caller that is
    /// still paging through the log has not caught up with the screen yet.
    fn stream(&self, cursor: u64, limit: usize) -> (String, u64) {
        if limit == 0 {
            return (String::new(), cursor);
        }
        let scrolled: Vec<&LogicalLine> = self.lines.since(cursor, limit).collect();
        let cut_short = scrolled.len() == limit;
        let mut text = scrolled
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        if cut_short {
            // Resume from just after the last line handed over, never from the end of the
            // log: the lines in between have not been read yet.
            let next = scrolled.last().map_or(cursor, |line| line.id + 1);
            return (text, next);
        }

        let screen = self.screen();
        if !screen.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&screen);
        }
        (text, self.lines.next_id())
    }

    /// Copy every row that entered the grid's history into the line log, then clear the
    /// history so the next slice's reading starts from zero again.
    fn drain_scrolled_rows(&mut self) {
        let history = self.term.grid().history_size();
        if history == 0 {
            return;
        }
        if history >= STAGING_ROWS {
            self.overflows += 1;
            tracing::warn!(
                staged = history,
                "terminal staging history saturated; scrolled rows may be missing from the \
                 line log"
            );
        }

        // History is indexed upwards from the viewport: `Line(-1)` is the row that scrolled
        // out most recently, so walking down to it replays them in the order they left.
        for offset in (1..=history).rev() {
            let line = Line(-(offset as i32));
            let (text, wrapped) = self.row_text(line);
            self.lines.append_row(&text, wrapped);
        }
        self.term.grid_mut().clear_history();
    }

    /// One row's plain text, and whether the grid marked it as continuing on the next row.
    fn row_text(&self, line: Line) -> (String, bool) {
        let last_column = Column(self.term.grid().columns().saturating_sub(1));
        let text = self
            .term
            .bounds_to_string(Point::new(line, Column(0)), Point::new(line, last_column));
        let wrapped = self.term.grid()[line][last_column]
            .flags
            .contains(Flags::WRAPLINE);
        (text, wrapped)
    }
}

impl std::fmt::Debug for TerminalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalState")
            .field("size", &self.size)
            .field("alt_screen", &self.is_alt_screen())
            .field("lines", &self.lines.len())
            .field("replay_bytes", &self.replay.len())
            .field("overflows", &self.overflows)
            .finish_non_exhaustive()
    }
}

/// Drop the blank rows at the bottom of a rendered screen, which are padding rather than
/// content and would otherwise dominate a read of a mostly-empty pane.
fn trim_trailing_blank_lines(rendered: &str) -> String {
    let mut lines: Vec<&str> = rendered.split('\n').collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> TerminalState {
        TerminalState::new(TerminalSize::new(20, 5), VtConfig::default())
    }

    fn logged(state: &TerminalState) -> Vec<String> {
        state
            .line_log()
            .since(0, usize::MAX)
            .map(|line| line.text.clone())
            .collect()
    }

    #[test]
    fn renders_what_the_child_wrote() {
        let mut state = state();
        state.feed(b"hello\r\nworld\r\n");
        assert_eq!(state.screen(), "hello\nworld");
    }

    #[test]
    fn the_screen_projection_shows_the_result_not_the_keystrokes() {
        // The screen is a projection of the grid, so it cannot accumulate the way an
        // echo-stripped byte stream does: `clear` typed one key at a time, each keystroke
        // arriving as its own echo, still reads back as one word and then as nothing.
        //
        // Orca's default read returns its accumulated stream and famously reports that as
        // `cclclecleaclear`. Nysia has no such mode to compare against — `--stream` here is
        // the logical line log plus the viewport, both of which are projections too — so
        // this is a regression test on the screen projection, not a reproduction of that
        // artefact. Which mode is the *default* is a separate guarantee with its own test.
        let mut state = state();
        state.feed(b"$ ");
        for byte in b"clear" {
            state.feed(std::slice::from_ref(byte));
        }
        assert_eq!(state.screen(), "$ clear");

        // Now the command runs: erase the display, home the cursor, redraw the prompt.
        state.feed(b"\r\n\x1b[2J\x1b[H$ ");

        // The prompt's trailing space is padding on an otherwise empty row, so the
        // rendered screen is the prompt alone.
        assert_eq!(state.screen(), "$");
        assert!(!state.screen().contains("cclcl"));
    }

    #[test]
    fn the_grid_follows_a_resize() {
        let mut state = state();
        state.feed(b"0123456789");
        assert_eq!(state.screen(), "0123456789");

        state.resize(TerminalSize::new(5, 3));
        assert_eq!(state.size(), TerminalSize::new(5, 3));
        // Twenty columns of content reflowed into five: the tail is what is on screen, and
        // the head has moved above it.
        assert_eq!(state.screen(), "56789");

        // New output lands at the new width.
        state.feed(
            b"
abcdefg",
        );
        assert!(state.screen().ends_with("abcdefg"));

        // And once the whole reflowed line has scrolled out it is logged as one logical
        // line again, not as the two rows it was displayed over.
        state.feed(
            b"
1
2
3
4
",
        );
        assert!(
            logged(&state).contains(&"0123456789".to_owned()),
            "logged: {:?}",
            logged(&state)
        );
    }

    #[test]
    fn a_zero_sized_resize_is_clamped_rather_than_panicking() {
        let mut state = state();
        state.resize(TerminalSize::new(0, 0));
        assert_eq!(state.size(), TerminalSize::new(1, 1));
        state.feed(b"x");
        assert_eq!(state.screen(), "x");
    }

    #[test]
    fn rows_that_scroll_out_land_in_the_line_log() {
        let mut state = state();
        for n in 0..8 {
            state.feed(format!("line {n}\r\n").as_bytes());
        }
        // Five rows fit on screen; the rest have scrolled out.
        assert_eq!(logged(&state), ["line 0", "line 1", "line 2", "line 3"]);
        assert_eq!(state.screen(), "line 4\nline 5\nline 6\nline 7");
    }

    #[test]
    fn a_soft_wrapped_row_is_logged_as_one_logical_line() {
        let mut state = TerminalState::new(TerminalSize::new(5, 2), VtConfig::default());
        // Ten columns of text in a five-column window wraps once, then three more rows
        // push the whole thing out of the viewport.
        state.feed(b"abcdefghij\r\n1\r\n2\r\n3\r\n");
        assert_eq!(logged(&state)[0], "abcdefghij");
    }

    #[test]
    fn a_newline_flood_larger_than_a_feed_slice_keeps_every_id_in_order() {
        // The proof that slicing the feed is load-bearing. A single call carrying far more
        // newlines than the staging history could hold at once still produces one id per
        // line, in order, with no gap: remove the `chunks(FEED_SLICE)` in `feed` and this
        // trips.
        let count = 100_000;
        let mut state = TerminalState::new(
            TerminalSize::new(20, 5),
            VtConfig {
                line_log_lines: count + 16,
                ..VtConfig::default()
            },
        );
        let flood: Vec<u8> = std::iter::repeat_n(b'\n', count).collect();
        state.feed(&flood);

        assert_eq!(state.overflows(), 0);
        let ids: Vec<u64> = state
            .line_log()
            .since(0, usize::MAX)
            .map(|l| l.id)
            .collect();
        assert_eq!(ids.len(), count - (state.size().rows as usize) + 1);
        assert!(
            ids.iter()
                .enumerate()
                .all(|(index, id)| *id == index as u64)
        );
    }

    #[test]
    fn a_newline_free_stream_does_not_grow_the_session_without_bound() {
        // The whole path, not just the log: bytes arrive at the grid, every row fills the
        // width and is marked wrapped, every scrolled row is handed to the line log with
        // `wrapped = true`, and nothing ever ends a logical line. This is `yes | tr -d
        // "\n"`, a minified bundle, `base64 -w0` of a large file.
        let budget = 64 * 1024;
        let mut state = TerminalState::new(
            TerminalSize::new(80, 24),
            VtConfig {
                line_log_lines: 8,
                line_log_bytes: budget,
                replay_bytes: 4 * 1024,
            },
        );

        let chunk: Vec<u8> = std::iter::repeat_n(b'x', 64 * 1024).collect();
        for _ in 0..64 {
            state.feed(&chunk);
        }

        // Four megabytes of newline-free output went in.
        let log = state.line_log();
        assert!(
            log.retained_bytes() <= log.byte_ceiling(),
            "the line log retained {} bytes against a ceiling of {}",
            log.retained_bytes(),
            log.byte_ceiling()
        );
        assert!(log.byte_ceiling() <= budget * 2);
        assert!(state.replay().len() <= 4 * 1024);
        assert!(
            log.split_lines() > 0,
            "a line this long must have been committed in pieces"
        );
    }

    #[test]
    fn the_alternate_screen_does_not_reach_the_line_log() {
        let mut state = state();
        state.feed(b"primary\r\n");
        state.feed(b"\x1b[?1049h");
        assert!(state.is_alt_screen());
        for n in 0..20 {
            state.feed(format!("alt {n}\r\n").as_bytes());
        }
        assert!(logged(&state).is_empty());

        state.feed(b"\x1b[?1049l");
        assert!(!state.is_alt_screen());
        assert_eq!(state.screen(), "primary");
    }

    #[test]
    fn the_stream_cursor_pages_and_ends_with_the_viewport() {
        let mut state = state();
        for n in 0..8 {
            state.feed(format!("line {n}\r\n").as_bytes());
        }
        // Four lines have scrolled out; the other four are still on screen.
        let first = state.read(ReadMode::Stream, 0, 2);
        assert_eq!(first.text, "line 0\nline 1");
        // Not `next_id()`: lines 2 and 3 have not been handed over yet, and a caller that
        // followed a cursor pointing past them would never see them.
        assert_eq!(first.next_cursor, 2);
        assert_eq!(first.oldest_cursor, 0);

        let second = state.read(ReadMode::Stream, first.next_cursor, 2);
        assert_eq!(second.text, "line 2\nline 3");
        assert_eq!(second.next_cursor, 4);

        let rest = state.read(ReadMode::Stream, second.next_cursor, usize::MAX);
        assert_eq!(rest.text, "line 4\nline 5\nline 6\nline 7");
        assert_eq!(rest.next_cursor, 4);
    }

    #[test]
    fn paging_the_stream_yields_every_line_exactly_once() {
        let mut state = state();
        for n in 0..20 {
            state.feed(format!("line {n}\r\n").as_bytes());
        }
        let mut seen = Vec::new();
        let mut cursor = 0;
        loop {
            let page = state.read(ReadMode::Stream, cursor, 3);
            if page.text.is_empty() {
                break;
            }
            seen.extend(page.text.lines().map(str::to_owned));
            if page.next_cursor == cursor {
                break;
            }
            cursor = page.next_cursor;
            if cursor >= state.line_log().next_id() {
                break;
            }
        }
        // Fifteen scrolled-out lines plus the five still on screen, each exactly once.
        let expected: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_stream_read_of_a_session_that_never_scrolled_is_just_the_viewport() {
        let mut state = state();
        state.feed(b"only line\r\n");
        let read = state.read(ReadMode::Stream, 0, usize::MAX);
        assert_eq!(read.text, "only line");
        assert_eq!(read.next_cursor, 0);
    }

    #[test]
    fn a_stale_cursor_is_visible_as_a_gap() {
        let mut state = TerminalState::new(
            TerminalSize::new(20, 2),
            VtConfig {
                line_log_lines: 3,
                ..VtConfig::default()
            },
        );
        for n in 0..10 {
            state.feed(format!("line {n}\r\n").as_bytes());
        }
        let read = state.read(ReadMode::Stream, 0, usize::MAX);
        assert!(read.oldest_cursor > 0, "the log should have evicted lines");
        assert!(!read.text.starts_with("line 0"));
    }

    #[test]
    fn a_read_with_no_mode_given_is_the_screen_and_not_the_stream() {
        // §7.2's divergence from Orca is about the *default*, so the test has to exercise
        // the default rather than name it. The two projections are made to differ first —
        // four lines have scrolled out, four are still on screen — so moving `#[default]`
        // to `Stream` turns this red instead of leaving it green.
        let mut state = state();
        for n in 0..8 {
            state.feed(format!("line {n}\r\n").as_bytes());
        }
        let screen = state.read(ReadMode::Screen, 0, usize::MAX).text;
        let stream = state.read(ReadMode::Stream, 0, usize::MAX).text;
        assert_ne!(screen, stream, "the two projections must differ here");

        let defaulted = state.read(ReadMode::default(), 0, usize::MAX);
        assert_eq!(defaulted.mode, ReadMode::Screen);
        assert_eq!(defaulted.text, screen);
        assert_ne!(defaulted.text, stream);
    }

    #[test]
    fn device_status_queries_are_answered_back_to_the_child() {
        let mut state = state();
        state.feed(b"\x1b[6n");
        let reply = state.take_replies();
        assert!(!reply.is_empty(), "CSI 6 n must be answered, or vim hangs");
        assert_eq!(&reply[..2], b"\x1b[");
        // Taking twice does not repeat the reply.
        assert!(state.take_replies().is_empty());
    }

    #[test]
    fn raw_bytes_are_retained_for_replay() {
        let mut state = state();
        state.feed(b"\x1b[31mred\x1b[0m");
        assert_eq!(state.replay(), b"\x1b[31mred\x1b[0m");
        // The escape sequences are applied, not printed.
        assert_eq!(state.screen(), "red");
    }

    #[test]
    fn osc_marks_reach_the_shell_state_through_feed() {
        let mut state = state();
        state.feed(b"\x1b]133;C\x07running\r\n\x1b]133;D;3\x07");
        assert_eq!(
            state.shell_state().command,
            super::super::CommandState::Finished { exit_code: Some(3) }
        );
        // The marks are not printed.
        assert_eq!(state.screen(), "running");
    }

    #[test]
    fn trailing_blank_rows_are_trimmed_but_interior_ones_are_kept() {
        assert_eq!(trim_trailing_blank_lines("a\n\nb\n\n  \n"), "a\n\nb");
        assert_eq!(trim_trailing_blank_lines("\n\n"), "");
    }
}
