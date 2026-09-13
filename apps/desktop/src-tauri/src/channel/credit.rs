//! Credit-window backpressure, as the window's client keeps it (§7.3).
//!
//! A PTY produces output faster than a webview renders it. Left alone, `yes` fills the
//! daemon's buffers, then the IPC queue, then memory, and the window dies holding a
//! gigabyte of text nobody will ever read. The brake is a credit window, and the chain it
//! forms is the whole answer:
//!
//! ```text
//!   child ──write──▶ kernel PTY buffer ──▶ daemon ──socket──▶ THIS PROCESS ──Channel──▶ xterm
//!                                             ▲                    │
//!                                             └──── CreditGrant ◀──┘  (after write() returned)
//! ```
//!
//! The daemon may only send bytes it has been granted. This process grants more once the
//! webview says it has *rendered* them — after xterm's `write()` callback fires, not when
//! the message arrives, because a message sitting in a queue has not been rendered. At zero
//! credit this process stops reading the socket; the socket buffer fills, the daemon's
//! write blocks, the daemon stops reading the PTY, the kernel buffer fills, and the child
//! blocks in `write(2)`. Not one byte is dropped and nothing grows without bound.
//!
//! ## What this ledger guarantees
//!
//! Unrendered bytes are bounded by [`CreditWindow::per_stream_initial`] for any one session
//! and by [`CreditWindow::total_initial`] across all of them — 512 KiB and 2 MiB at the
//! defaults. That is the memory ceiling, and it holds no matter how fast the child writes,
//! because credit is only ever returned by rendering.
//!
//! ## Why the numbers are not here
//!
//! [`CreditWindow`] comes from `nysia-proto` and a [`CreditGrant`] carries the window it
//! was issued under, so the client never holds its own copy of the constants (D-13). A
//! daemon that narrows the window for a slow renderer is obeyed on the next grant, with no
//! protocol change and no chance of the two ends disagreeing about what is in force.

use std::collections::HashMap;

use nysia_proto::credit::{CreditGrant, CreditWindow};
use nysia_proto::identity::SessionHandle;

use super::framing::StreamId;

/// Why the ledger refused an operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreditError {
    /// The daemon sent more than it was granted.
    ///
    /// A protocol violation rather than congestion, and the only honest response is to drop
    /// the connection: a peer that ignores the window has already made the memory ceiling
    /// unenforceable, and continuing would mean pretending otherwise.
    #[error("stream {stream} sent {sent} bytes with {credit} credit remaining")]
    Overrun {
        /// The offending stream.
        stream: StreamId,
        /// Bytes the frame carried.
        sent: u32,
        /// What was left to spend.
        credit: u32,
    },
    /// The webview acknowledged rendering more bytes than it was ever sent.
    ///
    /// Also unrecoverable: returning that credit upstream would raise the ceiling by
    /// exactly the amount of the lie.
    #[error("stream {stream} acknowledged {acked} bytes, {unrendered} were outstanding")]
    PhantomAck {
        /// The offending stream.
        stream: StreamId,
        /// Bytes the webview claimed.
        acked: u32,
        /// Bytes actually awaiting acknowledgement.
        unrendered: u32,
    },
    /// A frame or an ack named a stream that was never attached.
    #[error("stream {0} is not attached")]
    UnknownStream(StreamId),
}

/// One session's balance.
#[derive(Debug, Clone)]
struct StreamCredit {
    /// Which session, so a grant can name it on the wire.
    handle: SessionHandle,
    /// Bytes the daemon may still send. Falls as output arrives, rises as it is rendered.
    credit: u32,
    /// Delivered to the webview, not yet reported rendered. The memory ceiling.
    unrendered: u32,
    /// Rendered but not yet returned upstream, batched to [`CreditWindow::ack_batch`] so a
    /// busy stream does not spend its bandwidth on acknowledgements.
    ungranted: u32,
}

/// Every attached stream's credit, plus the budget they share.
#[derive(Debug)]
pub struct CreditLedger {
    window: CreditWindow,
    streams: HashMap<StreamId, StreamCredit>,
    /// The shared balance. A single stream cannot exhaust it alone — that is what
    /// per-stream credit is for — but thirty of them together can, and §7.3's separate
    /// total is what stops thirty idle-but-noisy sessions from costing thirty times the
    /// per-stream ceiling.
    total_credit: u32,
    /// Rendered across all streams, awaiting a grant. Batched like the per-stream figure.
    total_ungranted: u32,
}

impl CreditLedger {
    /// A ledger with no streams, operating the given window.
    ///
    /// The window is the daemon's to choose; [`CreditWindow::DEFAULT`] is what §7.3 adopts
    /// until one arrives on a grant.
    pub fn new(window: CreditWindow) -> Self {
        Self {
            window,
            streams: HashMap::new(),
            total_credit: window.total_initial,
            total_ungranted: 0,
        }
    }

    /// The window in force.
    #[cfg(test)]
    pub fn window(&self) -> CreditWindow {
        self.window
    }

    /// Adopt a window the daemon sent on a grant.
    ///
    /// An incoherent window is ignored rather than adopted: §7.3's parameters deadlock if
    /// the ack batch exceeds the pending cap, and honouring such a window would stall every
    /// stream at once with no way to tell why. The one in force stays in force.
    pub fn adopt(&mut self, window: CreditWindow) -> bool {
        if !window.is_coherent() {
            return false;
        }
        self.window = window;
        true
    }

    /// Open a stream with its initial credit.
    ///
    /// Re-attaching a stream that is already open leaves it untouched: a reconnect must not
    /// silently re-grant credit the daemon has already spent.
    pub fn attach(&mut self, stream: StreamId, handle: SessionHandle) {
        self.streams.entry(stream).or_insert_with(|| StreamCredit {
            handle,
            credit: self.window.per_stream_initial,
            unrendered: 0,
            ungranted: 0,
        });
    }

    /// Forget a stream and return its share of the shared budget.
    ///
    /// Without the second half, closing thirty sessions would leak the total budget away a
    /// stream at a time until nothing could be read at all.
    pub fn detach(&mut self, stream: StreamId) {
        if let Some(closed) = self.streams.remove(&stream) {
            self.total_credit = self
                .total_credit
                .saturating_add(closed.unrendered)
                .min(self.window.total_max);
        }
    }

    /// Whether a stream is attached.
    #[cfg(test)]
    pub fn is_attached(&self, stream: StreamId) -> bool {
        self.streams.contains_key(&stream)
    }

    /// Bytes this stream may still receive, which is the smaller of its own credit and the
    /// shared budget.
    ///
    /// The reader does not consult this — it stops on [`Self::is_blocked`], which is the
    /// coarser question of whether *any* stream could take another chunk. This is the
    /// per-stream figure the ceiling tests assert against.
    #[cfg(test)]
    pub fn available(&self, stream: StreamId) -> u32 {
        self.streams
            .get(&stream)
            .map_or(0, |s| s.credit.min(self.total_credit))
    }

    /// The shared budget still unspent.
    #[cfg(test)]
    pub fn shared_credit(&self) -> u32 {
        self.total_credit
    }

    /// Whether the shared budget can no longer fund a full chunk of output.
    ///
    /// "Cannot fund a chunk" rather than "is exactly zero", because the daemon sends in
    /// [`CreditWindow::chunk`]-sized pieces (48 KiB at the defaults) and a budget that
    /// cannot cover one is a budget that cannot cover anything. A ledger holding 40 KiB of
    /// credit is not usefully different from one holding none, and treating it as unblocked
    /// would have the reader wake, find nothing it can accept, and sleep again.
    ///
    /// This is the signal the socket reader stops on. It does not mean anything is broken —
    /// it means the webview is behind, and the correct response is to stop reading until it
    /// catches up, which is what pushes the stall back down to the child.
    pub fn is_blocked(&self) -> bool {
        self.total_credit < self.window.chunk
    }

    /// Unrendered bytes across every stream: the figure the memory ceiling bounds.
    ///
    /// Nothing in the reader needs it, because the ceiling is enforced by refusing credit
    /// rather than by measuring afterwards. The flood test measures it, which is how the
    /// bound is shown to hold rather than asserted to.
    #[cfg(test)]
    pub fn unrendered(&self) -> u64 {
        self.streams.values().map(|s| u64::from(s.unrendered)).sum()
    }

    /// Record output arriving from the daemon and heading for the webview.
    ///
    /// # Errors
    ///
    /// [`CreditError::UnknownStream`] for a stream that was never attached, and
    /// [`CreditError::Overrun`] when the daemon spent credit it did not have.
    pub fn receive(&mut self, stream: StreamId, bytes: u32) -> Result<(), CreditError> {
        let total_credit = self.total_credit;
        let entry = self
            .streams
            .get_mut(&stream)
            .ok_or(CreditError::UnknownStream(stream))?;

        let credit = entry.credit.min(total_credit);
        if bytes > credit {
            return Err(CreditError::Overrun {
                stream,
                sent: bytes,
                credit,
            });
        }

        entry.credit -= bytes;
        entry.unrendered = entry.unrendered.saturating_add(bytes);
        self.total_credit -= bytes;
        Ok(())
    }

    /// Record that the webview has rendered `bytes` of this stream.
    ///
    /// Returns a [`CreditGrant`] once the batch reaches [`CreditWindow::ack_batch`], and
    /// `None` while it is still accumulating. Call [`Self::drain_grant`] when the stream has
    /// gone quiet, or the tail of a burst is never returned and the window shrinks by that
    /// much for the life of the session.
    ///
    /// # Errors
    ///
    /// [`CreditError::UnknownStream`], and [`CreditError::PhantomAck`] when the webview
    /// claims to have rendered more than it was sent.
    pub fn render(
        &mut self,
        stream: StreamId,
        bytes: u32,
    ) -> Result<Option<CreditGrant>, CreditError> {
        let ack_batch = self.window.ack_batch;
        {
            let entry = self
                .streams
                .get_mut(&stream)
                .ok_or(CreditError::UnknownStream(stream))?;
            if bytes > entry.unrendered {
                return Err(CreditError::PhantomAck {
                    stream,
                    acked: bytes,
                    unrendered: entry.unrendered,
                });
            }
            entry.unrendered -= bytes;
            entry.ungranted = entry.ungranted.saturating_add(bytes);
        }
        self.total_ungranted = self.total_ungranted.saturating_add(bytes);

        let batched = self
            .streams
            .get(&stream)
            .is_some_and(|entry| entry.ungranted >= ack_batch);
        if batched {
            return Ok(self.drain_grant(stream));
        }
        Ok(None)
    }

    /// Return whatever credit this stream has earned but not yet been granted.
    ///
    /// `None` when there is nothing to return. This is the idle flush: without it the last
    /// few kilobytes of every burst sit in `ungranted` forever, and a session that alternates
    /// between bursts and silence loses a little of its window on each one.
    pub fn drain_grant(&mut self, stream: StreamId) -> Option<CreditGrant> {
        let window = self.window;
        let total_credit = self.total_credit;
        let total_ungranted = self.total_ungranted;
        let entry = self.streams.get_mut(&stream)?;
        if entry.ungranted == 0 {
            return None;
        }

        // Neither balance may climb past its ceiling, so a stream that renders faster than
        // it receives does not bank credit indefinitely.
        let bytes = entry
            .ungranted
            .min(window.per_stream_max - entry.credit.min(window.per_stream_max));
        entry.ungranted = 0;
        entry.credit = entry
            .credit
            .saturating_add(bytes)
            .min(window.per_stream_max);

        let shared = total_ungranted.min(window.total_max - total_credit.min(window.total_max));
        self.total_credit = total_credit.saturating_add(shared).min(window.total_max);
        self.total_ungranted = 0;

        if bytes == 0 {
            return None;
        }
        Some(CreditGrant {
            handle: entry.handle.clone(),
            bytes,
            window,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(n: u8) -> SessionHandle {
        format!("sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f{n:02}")
            .parse()
            .expect("a well-formed handle")
    }

    fn ledger() -> CreditLedger {
        let mut ledger = CreditLedger::new(CreditWindow::DEFAULT);
        ledger.attach(1, handle(1));
        ledger
    }

    #[test]
    fn a_fresh_stream_starts_on_the_windows_initial_credit() {
        let ledger = ledger();
        assert_eq!(
            ledger.available(1),
            CreditWindow::DEFAULT.per_stream_initial
        );
        assert!(!ledger.is_blocked());
        assert_eq!(ledger.unrendered(), 0);
        // A stream nobody attached has no credit, rather than unlimited credit.
        assert_eq!(ledger.available(99), 0);
    }

    #[test]
    fn receiving_spends_credit_and_rendering_earns_it_back() {
        let mut ledger = ledger();
        let chunk = CreditWindow::DEFAULT.chunk;

        ledger.receive(1, chunk).unwrap();
        assert_eq!(
            ledger.available(1),
            CreditWindow::DEFAULT.per_stream_initial - chunk
        );
        assert_eq!(ledger.unrendered(), u64::from(chunk));

        // Under the ack batch: nothing goes upstream yet.
        assert_eq!(ledger.render(1, chunk).unwrap(), None);
        assert_eq!(ledger.unrendered(), 0);

        let grant = ledger
            .drain_grant(1)
            .expect("an idle flush returns the tail");
        assert_eq!(grant.bytes, chunk);
        assert_eq!(grant.handle, handle(1));
        assert_eq!(grant.window, CreditWindow::DEFAULT);
        assert_eq!(
            ledger.available(1),
            CreditWindow::DEFAULT.per_stream_initial
        );
    }

    #[test]
    fn grants_are_batched_to_the_windows_ack_batch() {
        // The point of batching: a stream rendering 48 KiB chunks must not send a grant per
        // chunk, or a quarter of the channel is acknowledgements.
        let mut ledger = ledger();
        let window = CreditWindow::DEFAULT;

        let mut grants = 0;
        let mut rendered = 0;
        while rendered < window.ack_batch {
            ledger.receive(1, window.chunk).unwrap();
            if ledger.render(1, window.chunk).unwrap().is_some() {
                grants += 1;
            }
            rendered += window.chunk;
        }
        assert_eq!(grants, 1, "exactly one grant, once the batch filled");
        assert!(rendered >= window.ack_batch);
    }

    /// Deliverable 4: unbounded output, bounded memory.
    ///
    /// `yes` in a pane nobody is rendering. The webview never acks, so credit is never
    /// returned, and the assertion is that the ledger stops the flood rather than absorbing
    /// it. This is the unit-level half of the proof — the end-to-end half needs W4's daemon,
    /// which is not in this tree yet.
    #[test]
    fn a_flood_with_no_acknowledgement_stops_at_the_ceiling() {
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        for stream in 0..4 {
            ledger.attach(stream, handle(stream as u8));
        }

        let mut accepted: u64 = 0;
        let mut refused = 0;
        // Far more than any budget could hold: 4 streams × 512 KiB is 2 MiB, and this is
        // 48 MiB of offered output.
        for round in 0..1024 {
            let stream = round % 4;
            let room = ledger.available(stream);
            if room < window.chunk {
                refused += 1;
                continue;
            }
            ledger.receive(stream, window.chunk).unwrap();
            accepted += u64::from(window.chunk);
        }

        assert!(
            refused > 0,
            "the flood must have been refused, not absorbed"
        );
        for stream in 0..4 {
            assert!(
                ledger.available(stream) < window.chunk,
                "stream {stream} still had room for another chunk"
            );
        }

        // The ceiling, stated twice because both halves are the deliverable: nothing is
        // held beyond the window, and what is held equals what was accepted — no byte was
        // quietly dropped to make the number look good.
        assert!(
            ledger.unrendered() <= u64::from(window.total_initial),
            "held {} bytes, ceiling is {}",
            ledger.unrendered(),
            window.total_initial
        );
        assert_eq!(ledger.unrendered(), accepted);
    }

    #[test]
    fn the_flood_resumes_the_moment_the_webview_catches_up() {
        // Blocked is congestion, not failure: the reader must start again on its own.
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        ledger.attach(1, handle(1));

        while ledger.available(1) >= window.chunk {
            ledger.receive(1, window.chunk).unwrap();
        }
        assert!(ledger.available(1) < window.chunk);

        let grant = ledger
            .render(1, window.ack_batch)
            .unwrap()
            .expect("a full batch grants at once");
        assert_eq!(grant.bytes, window.ack_batch);
        assert!(ledger.available(1) >= window.chunk, "reading may resume");
    }

    #[test]
    fn one_stream_cannot_spend_the_shared_budget_out_from_under_the_others() {
        // §7.3 gives a total as well as a per-stream figure precisely so that thirty noisy
        // sessions cost 2 MiB rather than thirty times 512 KiB.
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        for stream in 0..8 {
            ledger.attach(stream, handle(stream as u8));
        }

        let mut total = 0u64;
        for stream in 0..8 {
            while ledger.available(stream) >= window.chunk {
                ledger.receive(stream, window.chunk).unwrap();
                total += u64::from(window.chunk);
            }
        }
        assert!(
            total <= u64::from(window.total_initial),
            "eight streams took {total} bytes against a {} byte shared budget",
            window.total_initial
        );
    }

    #[test]
    fn closing_a_stream_returns_its_share_of_the_shared_budget() {
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        ledger.attach(1, handle(1));
        ledger.attach(2, handle(2));

        ledger.receive(1, window.per_stream_initial).unwrap();
        let after_spending = ledger.shared_credit();
        assert_eq!(
            after_spending,
            window.total_initial - window.per_stream_initial
        );

        ledger.detach(1);
        assert!(!ledger.is_attached(1));
        assert_eq!(
            ledger.shared_credit(),
            window.total_initial,
            "a closed stream's budget must come back, or thirty closes exhaust it"
        );
        assert_eq!(ledger.unrendered(), 0);
    }

    #[test]
    fn a_peer_that_ignores_the_window_is_reported_rather_than_absorbed() {
        let mut ledger = ledger();
        let over = CreditWindow::DEFAULT.per_stream_initial + 1;
        assert_eq!(
            ledger.receive(1, over),
            Err(CreditError::Overrun {
                stream: 1,
                sent: over,
                credit: CreditWindow::DEFAULT.per_stream_initial,
            })
        );
        // Refused, not partially applied.
        assert_eq!(
            ledger.available(1),
            CreditWindow::DEFAULT.per_stream_initial
        );
        assert_eq!(ledger.unrendered(), 0);
    }

    #[test]
    fn a_webview_claiming_to_have_rendered_bytes_it_never_saw_is_refused() {
        // Returning that credit upstream would raise the ceiling by exactly the lie.
        let mut ledger = ledger();
        ledger.receive(1, 100).unwrap();
        assert_eq!(
            ledger.render(1, 101),
            Err(CreditError::PhantomAck {
                stream: 1,
                acked: 101,
                unrendered: 100,
            })
        );
        assert_eq!(ledger.unrendered(), 100);
    }

    #[test]
    fn frames_and_acks_for_an_unattached_stream_are_refused() {
        let mut ledger = ledger();
        assert_eq!(ledger.receive(7, 1), Err(CreditError::UnknownStream(7)));
        assert_eq!(ledger.render(7, 1), Err(CreditError::UnknownStream(7)));
        assert_eq!(ledger.drain_grant(7), None);
    }

    #[test]
    fn attaching_twice_does_not_re_grant_credit_the_daemon_has_spent() {
        let mut ledger = ledger();
        ledger.receive(1, 4096).unwrap();
        let spent = ledger.available(1);

        ledger.attach(1, handle(1));
        assert_eq!(
            ledger.available(1),
            spent,
            "a reconnect must not mint credit"
        );
    }

    #[test]
    fn an_incoherent_window_is_refused_and_the_working_one_stays_in_force() {
        let mut ledger = ledger();
        let deadlocking = CreditWindow {
            ack_batch: CreditWindow::DEFAULT.pending_cap + 1,
            ..CreditWindow::DEFAULT
        };
        assert!(!ledger.adopt(deadlocking));
        assert_eq!(ledger.window(), CreditWindow::DEFAULT);

        let narrower = CreditWindow {
            per_stream_initial: 64 * 1024,
            ..CreditWindow::DEFAULT
        };
        assert!(ledger.adopt(narrower));
        assert_eq!(ledger.window(), narrower);
    }
}
