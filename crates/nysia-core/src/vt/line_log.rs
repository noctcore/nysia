//! The logical line log.
//!
//! When a row scrolls out of the viewport its plain text is appended here with a
//! monotonic id. That gives cursor-paged agent reads for free: a caller remembers the id
//! it stopped at and asks for everything after it, without the daemon having to keep a
//! per-caller position or re-render anything.
//!
//! **Logical**, not physical. A row whose last cell carries `WRAPLINE` is the first half
//! of a line the terminal soft-wrapped to fit the window; joining it to its continuation
//! is what makes a read survive a resize, and what stops one shell command's output from
//! being reported as three lines because the window is narrow. A wrapped row is therefore
//! held pending until its unwrapped tail arrives, and only then does the joined line get
//! an id.
//!
//! The log is bounded, and bounded in **bytes** as well as in lines. Counting only
//! committed lines would leave the head unbounded: a stream with no newline in it — `yes |
//! tr -d "\n"`, a minified bundle, `base64 -w0` of a large file — produces nothing but
//! wrapped rows, and a soft-wrapped line that is still waiting for its tail is not a
//! committed line. The grid hands one such row to this log per scroll, at PTY throughput,
//! so the pending head would grow linearly with output forever. Under D-1 the daemon owns
//! every session, so that is not one session leaking: it is one session taking all of them
//! down with it.
//!
//! So a logical line that reaches [`LineLog::max_line_bytes`] is committed as it stands and
//! the rest starts a new one. A line that long is being read by an agent in pages anyway,
//! and reporting it in two pieces is the honest failure: the alternative is reporting it
//! once, eventually, from a process that has been killed.
//!
//! When the log is full the oldest line is discarded, and ids keep climbing, so a caller
//! whose cursor has fallen behind [`LineLog::oldest_id`] can tell that it missed lines
//! rather than silently receiving the wrong ones.

use std::collections::VecDeque;

/// One logical line of a session's scrolled-out output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLine {
    /// Monotonic id, unique for the life of the session and never reused.
    pub id: u64,
    /// The line's plain text, escape sequences already applied by the grid and gone.
    pub text: String,
}

/// A bounded, cursor-addressable log of the lines that have scrolled out of a session's
/// viewport.
#[derive(Debug)]
pub struct LineLog {
    /// Committed lines, oldest first.
    lines: VecDeque<LogicalLine>,
    /// How many lines are retained before the oldest is dropped.
    capacity: usize,
    /// How many bytes of committed line text are retained before the oldest is dropped.
    byte_budget: usize,
    /// The number of bytes currently held in `lines`, kept alongside rather than recomputed
    /// so that eviction does not walk the whole log on every scrolled row.
    bytes: usize,
    /// The id the next committed line will take.
    next_id: u64,
    /// The head of a soft-wrapped line whose continuation has not scrolled out yet.
    pending: Option<String>,
    /// How many lines have been dropped off the front over the log's life.
    dropped: u64,
    /// How many lines were committed because they hit the length cap rather than because
    /// the terminal ended them.
    split: u64,
}

impl LineLog {
    /// Build a log retaining at most `capacity` lines and `byte_budget` bytes of text.
    ///
    /// Both are clamped upwards to something usable: a log that cannot hold the line it was
    /// just handed has no use, and neither has one whose budget cannot hold a single row.
    #[must_use]
    pub fn with_capacity(capacity: usize, byte_budget: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            byte_budget: byte_budget.max(MIN_BYTE_BUDGET),
            bytes: 0,
            next_id: 0,
            pending: None,
            dropped: 0,
            split: 0,
        }
    }

    /// The longest a single logical line may grow before it is committed as it stands.
    ///
    /// Half the byte budget, so that the split line and the budget it is charged against
    /// cannot be in conflict, and never more than [`MAX_LINE_BYTES`].
    #[must_use]
    pub fn max_line_bytes(&self) -> usize {
        (self.byte_budget / 2).clamp(1, MAX_LINE_BYTES)
    }

    /// Append a row that has just scrolled out of the viewport.
    ///
    /// `wrapped` is whether the grid marked this row as continuing on the next one. A
    /// wrapped row does not normally commit: it is held until the row that ends the logical
    /// line arrives, and the whole thing is then committed under a single id. A line that
    /// grows past [`LineLog::max_line_bytes`] commits anyway — see the module docs.
    pub fn append_row(&mut self, text: &str, wrapped: bool) {
        match self.pending.as_mut() {
            Some(head) => head.push_str(text),
            None => self.pending = Some(text.to_owned()),
        }
        let overlong = self
            .pending
            .as_ref()
            .is_some_and(|head| head.len() >= self.max_line_bytes());
        if !wrapped || overlong {
            if wrapped && overlong {
                self.split += 1;
            }
            self.commit();
        }
    }

    /// Commit whatever is pending even though no unwrapped row has arrived.
    ///
    /// Used when a session ends or its grid is reset: the tail of a soft-wrapped line will
    /// never arrive, and losing it would be worse than reporting it short.
    pub fn flush(&mut self) {
        if self.pending.is_some() {
            self.commit();
        }
    }

    /// Lines with an id at or after `cursor`, oldest first, at most `limit` of them.
    ///
    /// A cursor below [`LineLog::oldest_id`] starts from the oldest line still retained
    /// rather than failing; the caller detects the gap by comparing the two.
    pub fn since(&self, cursor: u64, limit: usize) -> impl Iterator<Item = &LogicalLine> {
        self.lines
            .iter()
            .skip_while(move |line| line.id < cursor)
            .take(limit)
    }

    /// The id the next committed line will take, which is also the cursor a caller that
    /// has read everything should present next time.
    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// The oldest id still retained, or [`LineLog::next_id`] when the log is empty.
    #[must_use]
    pub fn oldest_id(&self) -> u64 {
        self.lines.front().map_or(self.next_id, |line| line.id)
    }

    /// How many lines are currently retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the log holds no committed lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// How many committed lines have been dropped off the front over the log's life.
    #[must_use]
    pub fn dropped_lines(&self) -> u64 {
        self.dropped
    }

    /// How many logical lines were committed early because they hit the length cap.
    #[must_use]
    pub fn split_lines(&self) -> u64 {
        self.split
    }

    /// Every byte of text this log is holding, the uncommitted head included.
    ///
    /// This is the number the bound is stated in, so it is the number a test asserting the
    /// bound has to read.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.bytes + self.pending.as_ref().map_or(0, String::len)
    }

    /// The ceiling [`LineLog::retained_bytes`] is held under.
    #[must_use]
    pub fn byte_ceiling(&self) -> usize {
        self.byte_budget + self.max_line_bytes()
    }

    /// Move `pending` into the log under a fresh id, evicting from the front until both
    /// the line count and the byte budget hold again.
    fn commit(&mut self) {
        let text = self.pending.take().unwrap_or_default();
        self.bytes += text.len();
        self.lines.push_back(LogicalLine {
            id: self.next_id,
            text,
        });
        self.next_id += 1;

        while self.lines.len() > self.capacity
            || (self.bytes > self.byte_budget && self.lines.len() > 1)
        {
            let Some(evicted) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= evicted.text.len();
            self.dropped += 1;
        }
    }
}

/// The smallest byte budget a log will accept, so that `max_line_bytes` cannot collapse to
/// something that splits every row.
const MIN_BYTE_BUDGET: usize = 8 * 1024;

/// The longest a single logical line may grow, however large the byte budget is. A line
/// past this is not a line any more; it is a stream with no newlines in it.
///
/// The cap is checked once per row, so a committed piece can overshoot it by up to the
/// width of one row — which is one grid row, never more.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// A budget large enough that nothing in these tests splits unless it means to.
    const ROOMY: usize = 1024 * 1024;

    fn log_of(capacity: usize) -> LineLog {
        LineLog::with_capacity(capacity, ROOMY)
    }

    fn texts(log: &LineLog, cursor: u64, limit: usize) -> Vec<&str> {
        log.since(cursor, limit)
            .map(|line| line.text.as_str())
            .collect()
    }

    #[test]
    fn ids_climb_from_zero_and_never_repeat() {
        let mut log = log_of(8);
        assert_eq!(log.next_id(), 0);
        assert!(log.is_empty());
        log.append_row("first", false);
        log.append_row("second", false);
        assert_eq!(log.len(), 2);
        assert_eq!(log.next_id(), 2);
        assert_eq!(log.oldest_id(), 0);
        assert_eq!(texts(&log, 0, 10), ["first", "second"]);
    }

    #[test]
    fn a_soft_wrapped_row_joins_its_continuation_under_one_id() {
        let mut log = log_of(8);
        log.append_row("the quick brown ", true);
        // Still pending: the logical line is not finished.
        assert!(log.is_empty());
        log.append_row("fox", false);
        assert_eq!(log.len(), 1);
        assert_eq!(texts(&log, 0, 10), ["the quick brown fox"]);
        assert_eq!(log.next_id(), 1);
    }

    #[test]
    fn a_line_wrapped_more_than_once_still_gets_one_id() {
        let mut log = log_of(8);
        log.append_row("aaa", true);
        log.append_row("bbb", true);
        log.append_row("ccc", false);
        assert_eq!(texts(&log, 0, 10), ["aaabbbccc"]);
        assert_eq!(log.next_id(), 1);
    }

    #[test]
    fn flush_commits_a_line_whose_tail_will_never_arrive() {
        let mut log = log_of(8);
        log.append_row("truncated", true);
        log.flush();
        assert_eq!(texts(&log, 0, 10), ["truncated"]);
        // A second flush with nothing pending must not invent an empty line.
        log.flush();
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn the_cursor_skips_what_the_caller_already_read() {
        let mut log = log_of(8);
        for n in 0..5 {
            log.append_row(&format!("line {n}"), false);
        }
        assert_eq!(texts(&log, 3, 10), ["line 3", "line 4"]);
        assert_eq!(texts(&log, 5, 10), Vec::<&str>::new());
        assert_eq!(texts(&log, 0, 2), ["line 0", "line 1"]);
    }

    #[test]
    fn eviction_keeps_ids_monotonic_so_a_gap_is_visible() {
        let mut log = log_of(3);
        for n in 0..6 {
            log.append_row(&format!("line {n}"), false);
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.oldest_id(), 3);
        assert_eq!(log.next_id(), 6);
        assert_eq!(log.dropped_lines(), 3);
        // A stale cursor gets what survives, and `oldest_id` is how the caller learns it
        // lost the rest.
        assert_eq!(texts(&log, 0, 10), ["line 3", "line 4", "line 5"]);
    }

    #[test]
    fn a_zero_capacity_log_still_holds_one_line() {
        let mut log = log_of(0);
        log.append_row("kept", false);
        assert_eq!(texts(&log, 0, 10), ["kept"]);
    }

    #[test]
    fn a_blank_row_is_a_line_of_its_own() {
        let mut log = log_of(8);
        log.append_row("above", false);
        log.append_row("", false);
        log.append_row("below", false);
        assert_eq!(texts(&log, 0, 10), ["above", "", "below"]);
    }

    #[test]
    fn a_stream_with_no_newline_in_it_cannot_grow_the_pending_head() {
        // The defect this bound exists for. Every row is wrapped, so nothing ever ends a
        // logical line, and counting only committed lines left the head unbounded: ten
        // thousand rows of a thousand bytes used to sit in `pending` with `len() == 0`.
        // Remove the `overlong` branch in `append_row` and this trips at the first assert.
        let budget = 64 * 1024;
        let mut log = LineLog::with_capacity(8, budget);
        let row = "x".repeat(1000);
        for _ in 0..10_000 {
            log.append_row(&row, true);
            assert!(
                log.retained_bytes() <= log.byte_ceiling(),
                "retained {} bytes against a ceiling of {}",
                log.retained_bytes(),
                log.byte_ceiling()
            );
        }
        // Ten megabytes went in; the ceiling is a small multiple of the budget.
        assert!(log.byte_ceiling() <= budget * 2);
        assert!(log.split_lines() > 0, "the long line must have been split");
        assert!(!log.is_empty(), "and the tail of it must still be readable");
    }

    #[test]
    fn a_line_that_hits_the_cap_is_split_rather_than_dropped() {
        // Six thousand bytes of one logical line, against a cap of four thousand and a
        // budget of eight: enough to force a split, not enough to force an eviction, so
        // every byte written must still be readable — in two pieces instead of one.
        let mut log = LineLog::with_capacity(64, MIN_BYTE_BUDGET);
        assert_eq!(log.max_line_bytes(), MIN_BYTE_BUDGET / 2);
        let row = "y".repeat(1000);
        for _ in 0..6 {
            log.append_row(&row, true);
        }
        log.flush();

        let total: usize = log.since(0, usize::MAX).map(|line| line.text.len()).sum();
        assert_eq!(total, row.len() * 6);
        assert_eq!(log.dropped_lines(), 0, "nothing should have been evicted");
        assert!(
            log.len() > 1,
            "the line should have been committed in pieces"
        );
        assert!(log.split_lines() > 0);
    }

    #[test]
    fn the_byte_budget_evicts_even_when_the_line_count_would_not() {
        let mut log = LineLog::with_capacity(1000, MIN_BYTE_BUDGET);
        let row = "z".repeat(1024);
        for _ in 0..64 {
            log.append_row(&row, false);
        }
        assert!(log.len() < 64, "the byte budget must have evicted lines");
        assert!(log.retained_bytes() <= log.byte_ceiling());
        assert!(log.dropped_lines() > 0);
    }

    #[test]
    fn a_single_line_over_the_whole_budget_is_still_readable() {
        // The degenerate case: eviction must not empty the log trying to satisfy a budget
        // one line cannot fit under.
        let mut log = LineLog::with_capacity(4, MIN_BYTE_BUDGET);
        log.append_row(&"q".repeat(MIN_BYTE_BUDGET * 2), false);
        assert_eq!(log.len(), 1);
        assert!(!log.is_empty());
    }
}
