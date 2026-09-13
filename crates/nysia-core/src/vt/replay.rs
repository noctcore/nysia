//! The bounded raw-byte replay ring.
//!
//! A reattaching client is handed exactly the bytes it missed rather than a re-render, so
//! its `xterm.js` parser reconstructs the same state the Rust grid already holds. That is
//! what makes reattach replay scrollback, which is half the v0.1 acceptance criterion.
//!
//! The ring stores raw bytes, escape sequences included. Trimming therefore has to be
//! careful: cutting mid-sequence corrupts the parser on the other end (§7.3). The ring
//! cannot know where sequence boundaries are without parsing, so it does the next best
//! thing — after trimming to the cap it advances the head past the next newline, which is
//! a boundary no escape sequence spans. The scan is bounded so that a single very long
//! line cannot cause the ring to discard everything.

use std::collections::VecDeque;

/// How far past the raw trim point the ring will look for a newline to align on. A line
/// longer than this keeps the raw cut; the alternative is dropping the whole buffer.
const ALIGN_SCAN_LIMIT: usize = 8 * 1024;

/// A bounded FIFO of the raw bytes most recently written by a session's child.
#[derive(Debug)]
pub struct ReplayRing {
    /// Bytes, oldest first.
    bytes: VecDeque<u8>,
    /// The high-water mark `bytes` is trimmed back to.
    capacity: usize,
    /// How many bytes have been discarded over the ring's life, so a client can tell that
    /// its replay is a tail rather than the whole session.
    dropped: u64,
}

impl ReplayRing {
    /// Build a ring that retains at most `capacity` bytes.
    ///
    /// A zero capacity is legal and makes the ring discard everything, which is what a
    /// session with replay disabled wants.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            capacity,
            dropped: 0,
        }
    }

    /// Append raw output, trimming the oldest bytes back to the cap.
    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk.iter().copied());
        self.trim();
    }

    /// The bytes a reattaching client should be replayed, oldest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    /// How many bytes are currently retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the ring is currently empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The high-water mark this ring trims back to.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many bytes have been discarded over the ring's whole life.
    #[must_use]
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped
    }

    /// Forget everything, as a `RIS` reset or a fresh attach does.
    pub fn clear(&mut self) {
        self.dropped += self.bytes.len() as u64;
        self.bytes.clear();
    }

    /// Drop from the front until the ring is within its cap, then align the new head to
    /// just past a newline when one is close enough to be worth the extra loss.
    fn trim(&mut self) {
        if self.bytes.len() <= self.capacity {
            return;
        }
        let over = self.bytes.len() - self.capacity;
        self.discard_front(over);

        let scan = ALIGN_SCAN_LIMIT.min(self.bytes.len());
        if let Some(offset) = self.bytes.iter().take(scan).position(|&b| b == b'\n') {
            self.discard_front(offset + 1);
        }
    }

    /// Remove `count` bytes from the front, counting them as dropped.
    fn discard_front(&mut self, count: usize) {
        let count = count.min(self.bytes.len());
        self.bytes.drain(..count);
        self.dropped += count as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_everything_below_the_cap() {
        let mut ring = ReplayRing::with_capacity(64);
        ring.push(b"hello ");
        ring.push(b"world");
        assert_eq!(ring.snapshot(), b"hello world");
        assert_eq!(ring.dropped_bytes(), 0);
        assert!(!ring.is_empty());
    }

    #[test]
    fn trims_to_the_cap_and_counts_what_it_dropped() {
        let mut ring = ReplayRing::with_capacity(4);
        ring.push(b"abcdefgh");
        assert_eq!(ring.len(), 4);
        assert_eq!(ring.snapshot(), b"efgh");
        assert_eq!(ring.dropped_bytes(), 4);
    }

    #[test]
    fn aligns_the_head_past_a_nearby_newline() {
        let mut ring = ReplayRing::with_capacity(10);
        // The raw cut would leave "b\ncccccccc"; the alignment pass drops through the
        // newline so a client never resumes in the middle of what preceded it.
        ring.push(b"aaaaaaaaab\ncccccccc");
        assert_eq!(ring.snapshot(), b"cccccccc");
    }

    #[test]
    fn keeps_the_raw_cut_when_no_newline_is_within_reach() {
        let cap = 4;
        let mut ring = ReplayRing::with_capacity(cap);
        let long: Vec<u8> = std::iter::repeat_n(b'x', ALIGN_SCAN_LIMIT + cap + 16).collect();
        ring.push(&long);
        assert_eq!(ring.len(), cap);
        assert_eq!(ring.snapshot(), b"xxxx");
    }

    #[test]
    fn a_zero_capacity_ring_discards_everything() {
        let mut ring = ReplayRing::with_capacity(0);
        ring.push(b"anything at all");
        assert!(ring.is_empty());
        assert_eq!(ring.dropped_bytes(), 15);
    }

    #[test]
    fn clear_counts_as_dropped() {
        let mut ring = ReplayRing::with_capacity(64);
        ring.push(b"12345");
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.dropped_bytes(), 5);
    }
}
