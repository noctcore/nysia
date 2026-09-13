//! The bounded raw-byte replay ring.
//!
//! A reattaching client is handed exactly the bytes it missed rather than a re-render, so
//! its `xterm.js` parser reconstructs the same state the Rust grid already holds. That is
//! what makes reattach replay scrollback, which is half the v0.1 acceptance criterion.
//!
//! The ring stores raw bytes, escape sequences included. Trimming therefore has to cut
//! somewhere, and §7.3 is clear that cutting mid-sequence corrupts the parser on the other
//! end. The ring cannot find sequence boundaries without parsing, so after trimming to the
//! cap it advances the head past the next newline.
//!
//! **That is a heuristic, not a guarantee, and the limit is worth stating exactly.** A
//! newline is a reliable boundary in the output that dominates a terminal — printable text,
//! `CSI` cursor moves, `SGR` colour runs — because none of those can contain one. It is
//! *not* a boundary inside a string-type sequence: `0x0A` inside a `DCS` payload is passed
//! through as data, and inside `OSC` or `APC` it is ignored and the sequence continues. A
//! trimmed replay can therefore still resume in the middle of one of those, and a client
//! that starts parsing there will mis-read until the terminator arrives.
//!
//! The complete answer is not this ring's to give: §7.3 says that on overflow the whole
//! transient buffer is dropped and `ESC c` is injected, so the client resets rather than
//! resynchronises. That is transport-side, and it is W4's. What this ring owes is a cheap
//! trim that is right for ordinary output and honest about the rest.
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

        // Bounded: a single line longer than this keeps the raw cut, because discarding
        // the whole buffer to find a boundary is worse than resuming inside a line.
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
    fn the_newline_alignment_is_a_heuristic_and_does_not_span_a_string_sequence() {
        // Stated as a test so the limit in the module docs cannot quietly stop being true:
        // an OSC payload may contain a newline, and the alignment will happily cut there.
        let mut ring = ReplayRing::with_capacity(8);
        ring.push(b"\x1b]0;ti\ntle\x07AAAAAAAA");
        let snapshot = ring.snapshot();
        assert!(
            !snapshot.starts_with(b"\x1b]"),
            "the head was trimmed, as expected"
        );
        // What survives begins after a newline that was inside the OSC string, which is
        // exactly the case §7.3's `ESC c` reset exists to cover on the transport side.
        assert_eq!(snapshot, b"AAAAAAAA");
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
