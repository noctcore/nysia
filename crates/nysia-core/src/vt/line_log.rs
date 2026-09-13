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
//! The log is bounded. When it is full the oldest line is discarded, and ids keep
//! climbing, so a caller whose cursor has fallen behind [`LineLog::oldest_id`] can tell
//! that it missed lines rather than silently receiving the wrong ones.

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
    /// The id the next committed line will take.
    next_id: u64,
    /// The head of a soft-wrapped line whose continuation has not scrolled out yet.
    pending: Option<String>,
    /// How many lines have been dropped off the front over the log's life.
    dropped: u64,
}

impl LineLog {
    /// Build a log retaining at most `capacity` lines. A capacity of zero is clamped to
    /// one, because a log that cannot hold the line it was just handed has no use.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            next_id: 0,
            pending: None,
            dropped: 0,
        }
    }

    /// Append a row that has just scrolled out of the viewport.
    ///
    /// `wrapped` is whether the grid marked this row as continuing on the next one. A
    /// wrapped row does not commit: it is held until the row that ends the logical line
    /// arrives, and the whole thing is then committed under a single id.
    pub fn append_row(&mut self, text: &str, wrapped: bool) {
        match self.pending.as_mut() {
            Some(head) => head.push_str(text),
            None => self.pending = Some(text.to_owned()),
        }
        if !wrapped {
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

    /// Move `pending` into the log under a fresh id, evicting the oldest line if full.
    fn commit(&mut self) {
        let text = self.pending.take().unwrap_or_default();
        self.lines.push_back(LogicalLine {
            id: self.next_id,
            text,
        });
        self.next_id += 1;
        while self.lines.len() > self.capacity {
            self.lines.pop_front();
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(log: &LineLog, cursor: u64, limit: usize) -> Vec<&str> {
        log.since(cursor, limit)
            .map(|line| line.text.as_str())
            .collect()
    }

    #[test]
    fn ids_climb_from_zero_and_never_repeat() {
        let mut log = LineLog::with_capacity(8);
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
        let mut log = LineLog::with_capacity(8);
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
        let mut log = LineLog::with_capacity(8);
        log.append_row("aaa", true);
        log.append_row("bbb", true);
        log.append_row("ccc", false);
        assert_eq!(texts(&log, 0, 10), ["aaabbbccc"]);
        assert_eq!(log.next_id(), 1);
    }

    #[test]
    fn flush_commits_a_line_whose_tail_will_never_arrive() {
        let mut log = LineLog::with_capacity(8);
        log.append_row("truncated", true);
        log.flush();
        assert_eq!(texts(&log, 0, 10), ["truncated"]);
        // A second flush with nothing pending must not invent an empty line.
        log.flush();
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn the_cursor_skips_what_the_caller_already_read() {
        let mut log = LineLog::with_capacity(8);
        for n in 0..5 {
            log.append_row(&format!("line {n}"), false);
        }
        assert_eq!(texts(&log, 3, 10), ["line 3", "line 4"]);
        assert_eq!(texts(&log, 5, 10), Vec::<&str>::new());
        assert_eq!(texts(&log, 0, 2), ["line 0", "line 1"]);
    }

    #[test]
    fn eviction_keeps_ids_monotonic_so_a_gap_is_visible() {
        let mut log = LineLog::with_capacity(3);
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
        let mut log = LineLog::with_capacity(0);
        log.append_row("kept", false);
        assert_eq!(texts(&log, 0, 10), ["kept"]);
    }

    #[test]
    fn a_blank_row_is_a_line_of_its_own() {
        let mut log = LineLog::with_capacity(8);
        log.append_row("above", false);
        log.append_row("", false);
        log.append_row("below", false);
        assert_eq!(texts(&log, 0, 10), ["above", "", "below"]);
    }
}
