//! The ≥ 1 KiB coalescing window (§7.3, traps register #4).
//!
//! Tauri routes an IPC payload of **fewer than 1024 bytes through `eval`** and anything
//! larger through its fetch queue. A PTY producing forty-byte writes — which is what a
//! shell prompt, a keystroke echo and a spinner frame all are — would therefore deliver
//! every one of them by building a JavaScript string and evaluating it. That is the slow
//! path, and on a busy session it is the difference between a terminal and a slideshow.
//!
//! So output is packed into one buffer and sent in batches. The two flush triggers §7.3
//! names:
//!
//! - **64 KiB.** A hard ceiling on how much is ever held. Reaching it flushes at once.
//! - **16 ms.** The window's age. Without it a quiet session's last frame — the bell, the
//!   final newline of a command — would sit in the buffer indefinitely waiting for company
//!   that never comes.
//!
//! The 1 KiB figure is the *goal*, not a third trigger, and the distinction matters. A
//! coalescer that refused to flush below 1 KiB would drop bytes on the floor of a quiet
//! session forever; correctness beats the fast path, so a lone twelve-byte bell still goes
//! out after 16 ms and pays for `eval`. What the 16 ms window buys is that any session
//! producing output at a rate worth optimising clears 1 KiB long before the timer fires —
//! which is exactly what `a_burst_of_small_writes_leaves_as_one_frame` asserts.
//!
//! This type is pure and holds no clock of its own: `now` is a parameter on every method
//! that needs one. That is what lets the tests drive sixteen milliseconds of behaviour
//! without sleeping for sixteen milliseconds, and it is why the flush policy is testable at
//! all.

use std::time::{Duration, Instant};

use super::framing::{ChannelFrame, FramingError, encode_into};

/// The payload size at and above which Tauri uses its fetch queue instead of `eval`.
///
/// Not a flush trigger — see the module docs — so nothing in the driver reads it. It is the
/// target the 16 ms window exists to hit, and the number the burst test asserts a delivery
/// clears; naming it here is what keeps that assertion tied to the reason for it.
#[cfg(test)]
pub const COALESCE_TARGET_BYTES: usize = 1024;

/// Reaching this flushes immediately, whatever the window's age.
pub const FLUSH_AT_BYTES: usize = 64 * 1024;

/// The longest a byte waits for company before being sent alone.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// A buffer of encoded frames and the age of the window holding them.
///
/// Frames from every session share one buffer: §7.3 gives the webview exactly one
/// `Channel`, and [`super::framing`] puts the stream id in each header so the far side can
/// tell them apart again.
#[derive(Debug, Default)]
pub struct Coalescer {
    buffer: Vec<u8>,
    /// When the first byte of the current window arrived. `None` when the buffer is empty,
    /// so an idle coalescer has no deadline rather than one permanently in the past.
    opened_at: Option<Instant>,
}

impl Coalescer {
    /// An empty window.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes currently held, headers included.
    #[cfg(test)]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Whether anything is waiting.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Add a frame to the window.
    ///
    /// Returns the buffer to send when this frame took it to [`FLUSH_AT_BYTES`], and `None`
    /// while it should keep filling. A caller that gets `None` must still honour
    /// [`Self::deadline`], or the frame waits forever.
    ///
    /// # Errors
    ///
    /// Propagates [`FramingError::Oversized`] for a payload past the frame ceiling. The
    /// frame is not added, and the window is left exactly as it was — a frame the writer
    /// should never have produced must not also corrupt the frames around it.
    pub fn push(
        &mut self,
        frame: &ChannelFrame,
        now: Instant,
    ) -> Result<Option<Vec<u8>>, FramingError> {
        let before = self.buffer.len();
        if let Err(error) = encode_into(frame, &mut self.buffer) {
            self.buffer.truncate(before);
            return Err(error);
        }
        if self.opened_at.is_none() {
            self.opened_at = Some(now);
        }

        if self.buffer.len() >= FLUSH_AT_BYTES {
            return Ok(Some(self.take()));
        }
        Ok(None)
    }

    /// The buffer, if the window is due to close at `now`.
    ///
    /// Due means the ceiling was reached — which [`Self::push`] normally catches first —
    /// or the window has been open for [`FLUSH_INTERVAL`]. An empty window is never due.
    pub fn poll(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            return None;
        }
        let aged = self
            .opened_at
            .is_some_and(|opened| now.duration_since(opened) >= FLUSH_INTERVAL);
        if aged || self.buffer.len() >= FLUSH_AT_BYTES {
            return Some(self.take());
        }
        None
    }

    /// How long the driver may block before it must call [`Self::poll`] again.
    ///
    /// `None` when nothing is buffered, which is the driver's cue to wait indefinitely for
    /// the next frame rather than spin on a timer. `Some(Duration::ZERO)` means the window
    /// is already overdue.
    pub fn deadline(&self, now: Instant) -> Option<Duration> {
        let opened = self.opened_at?;
        Some(FLUSH_INTERVAL.saturating_sub(now.duration_since(opened)))
    }

    /// Empty the window unconditionally, whatever its age.
    ///
    /// For teardown: a channel that is about to be dropped should deliver what it holds
    /// rather than discard it.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(self.take())
        }
    }

    /// Take the buffer and reopen an empty window.
    fn take(&mut self) -> Vec<u8> {
        self.opened_at = None;
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use nysia_proto::frame::FrameKind;

    use super::super::framing::{FRAME_HEADER_BYTES, FrameDecoder};
    use super::*;

    fn output(stream: u32, len: usize) -> ChannelFrame {
        ChannelFrame::new(FrameKind::Output, stream, vec![b'x'; len])
    }

    /// Every frame the given buffer decodes to.
    fn frames_in(buffer: &[u8]) -> Vec<ChannelFrame> {
        let mut decoder = FrameDecoder::new();
        decoder.push(buffer);
        let mut out = Vec::new();
        while let Some(frame) = decoder
            .next_frame()
            .expect("the coalescer emits valid frames")
        {
            out.push(frame);
        }
        assert_eq!(
            decoder.buffered(),
            0,
            "a flush must end on a frame boundary"
        );
        out
    }

    /// Deliverable 3: small writes are batched rather than forwarded one by one.
    ///
    /// Forty 40-byte writes is a realistic second of a shell echoing keystrokes. Sent
    /// individually every one of them would be under 1024 bytes and would go through
    /// `eval`. The assertion is therefore in two parts, and both matter: **one** delivery,
    /// and that delivery **over the 1 KiB threshold**.
    #[test]
    fn a_burst_of_small_writes_leaves_as_one_frame() {
        let start = Instant::now();
        let mut coalescer = Coalescer::new();

        let mut deliveries = Vec::new();
        for index in 0..40 {
            // A millisecond apart, so the whole burst lands inside one 16 ms window.
            let now = start + Duration::from_micros(index * 200);
            if let Some(bytes) = coalescer.push(&output(1, 40), now).unwrap() {
                deliveries.push(bytes);
            }
        }
        assert!(
            deliveries.is_empty(),
            "nothing should have left the window yet"
        );

        let closed = coalescer
            .poll(start + FLUSH_INTERVAL)
            .expect("the window is 16 ms old");
        deliveries.push(closed);

        assert_eq!(
            deliveries.len(),
            1,
            "40 writes must become 1 delivery, not 40"
        );
        let delivered = &deliveries[0];
        assert_eq!(delivered.len(), 40 * (FRAME_HEADER_BYTES + 40));
        assert!(
            delivered.len() >= COALESCE_TARGET_BYTES,
            "the delivery is {} bytes, under the {COALESCE_TARGET_BYTES}-byte threshold that \
             sends a payload through `eval`",
            delivered.len()
        );
        assert_eq!(
            frames_in(delivered).len(),
            40,
            "no frame may be lost in the packing"
        );
    }

    #[test]
    fn sixty_four_kibibytes_flushes_without_waiting_for_the_timer() {
        let start = Instant::now();
        let mut coalescer = Coalescer::new();

        // §7.3 chunks PTY output at 48 KiB, so two chunks cross the ceiling.
        assert!(
            coalescer
                .push(&output(1, 48 * 1024), start)
                .unwrap()
                .is_none()
        );
        let flushed = coalescer
            .push(&output(1, 48 * 1024), start + Duration::from_micros(1))
            .unwrap()
            .expect("the ceiling forces a flush");

        assert!(flushed.len() >= FLUSH_AT_BYTES);
        assert_eq!(frames_in(&flushed).len(), 2);
        // The window reopened empty, so the next byte starts a fresh 16 ms.
        assert!(coalescer.is_empty());
        assert_eq!(coalescer.deadline(start), None);
    }

    #[test]
    fn a_lone_small_frame_still_leaves_once_the_window_ages() {
        // Correctness beats the fast path: a bell on an otherwise silent session is 9 bytes
        // and must not be held hostage waiting for company that never arrives.
        let start = Instant::now();
        let mut coalescer = Coalescer::new();
        coalescer
            .push(&ChannelFrame::new(FrameKind::Bell, 4, Vec::new()), start)
            .unwrap();

        assert_eq!(coalescer.poll(start + Duration::from_millis(15)), None);
        let flushed = coalescer
            .poll(start + Duration::from_millis(16))
            .expect("16 ms is the trigger");
        assert_eq!(
            frames_in(&flushed),
            vec![ChannelFrame::new(FrameKind::Bell, 4, Vec::new())]
        );
    }

    #[test]
    fn the_deadline_counts_down_from_the_first_byte_not_the_last() {
        // The window's age is what §7.3 bounds. Restarting the clock on every push would
        // let a steady trickle of writes defer the flush indefinitely.
        let start = Instant::now();
        let mut coalescer = Coalescer::new();
        assert_eq!(
            coalescer.deadline(start),
            None,
            "an idle window has no deadline"
        );

        coalescer.push(&output(1, 10), start).unwrap();
        assert_eq!(coalescer.deadline(start), Some(FLUSH_INTERVAL));

        let later = start + Duration::from_millis(10);
        coalescer.push(&output(1, 10), later).unwrap();
        assert_eq!(
            coalescer.deadline(later),
            Some(Duration::from_millis(6)),
            "the second push must not restart the window"
        );

        // Overdue saturates at zero rather than underflowing.
        assert_eq!(
            coalescer.deadline(start + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn every_session_shares_the_window_and_stays_distinguishable() {
        // One Channel, many sessions (§7.3). Packing them together is the point; the stream
        // id in each header is what lets the webview take them apart again.
        let start = Instant::now();
        let mut coalescer = Coalescer::new();
        for stream in [7, 8, 7, 9] {
            coalescer.push(&output(stream, 16), start).unwrap();
        }

        let flushed = coalescer.poll(start + FLUSH_INTERVAL).unwrap();
        let streams: Vec<u32> = frames_in(&flushed)
            .iter()
            .map(|frame| frame.stream)
            .collect();
        assert_eq!(
            streams,
            vec![7, 8, 7, 9],
            "order within the window is preserved"
        );
    }

    #[test]
    fn an_oversized_frame_is_refused_without_corrupting_the_window() {
        let start = Instant::now();
        let mut coalescer = Coalescer::new();
        coalescer.push(&output(1, 32), start).unwrap();
        let held = coalescer.buffered();

        let huge = output(1, nysia_proto::frame::MAX_FRAME_PAYLOAD_BYTES + 1);
        assert!(matches!(
            coalescer.push(&huge, start),
            Err(FramingError::Oversized { .. })
        ));

        assert_eq!(
            coalescer.buffered(),
            held,
            "the window must be left as it was"
        );
        assert_eq!(frames_in(&coalescer.flush().unwrap()), vec![output(1, 32)]);
    }

    #[test]
    fn flush_empties_a_window_that_is_not_yet_due() {
        let start = Instant::now();
        let mut coalescer = Coalescer::new();
        assert_eq!(
            coalescer.flush(),
            None,
            "an empty window flushes to nothing"
        );

        coalescer.push(&output(2, 4), start).unwrap();
        assert!(coalescer.flush().is_some());
        assert!(coalescer.is_empty());
        assert_eq!(coalescer.flush(), None);
    }
}
