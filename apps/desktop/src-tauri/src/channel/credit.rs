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
//!                                             └───── CreditAck ◀───┘  (after write() returned)
//! ```
//!
//! **The producer grants and the consumer acks, and the arrow above is the direction that
//! matters.** The daemon owns the window: it issues a [`nysia_proto::CreditGrant`] on attach and spends
//! that allowance byte for byte as it writes. This process is the consumer, so what travels
//! back up the socket is a [`nysia_proto::CreditAck`] naming the bytes the webview has
//! *rendered*, and each one replenishes the daemon's allowance. A client that sent grants
//! instead was speaking the producer's half of the protocol into a daemon that honours only
//! acks: it dropped them, nothing ever replenished, and a flood stalled dead at the
//! per-stream ceiling — with the daemon's pump blocked behind it, which froze `terminal
//! read` for that session for the CLI and for hooks as well.
//!
//! The ack goes out after xterm's `write()` callback fires, not when the message arrives,
//! because a message sitting in a queue has not been rendered. When no
//! attached stream can take another chunk this process stops reading the socket; the socket
//! buffer fills, the daemon's write blocks, the daemon stops reading the PTY, the kernel
//! buffer fills, and the child blocks in `write(2)`. Not one byte is dropped and nothing
//! grows without bound.
//!
//! ## Two balances, and why the reader has to watch both
//!
//! §7.3 gives a per-stream figure *and* a total. They run out at different times, and the
//! difference is not academic: with one session attached, per-stream credit is exhausted at
//! 512 KiB while the 2 MiB total is still three quarters full. A reader that asked only
//! "is the total spent?" would answer no, go back to `read`, and block — while an honest
//! daemon, obeying the per-stream figure it was given, had already stopped sending. Nothing
//! would ever arrive, the queued render reports would never be drained, no grant would ever
//! be written, and the session would wedge on its first burst.
//!
//! So [`CreditLedger::is_blocked`] asks the question the reader actually has: **can any
//! attached stream take another chunk?** [`CreditLedger::available`] is the per-stream
//! figure behind it, and both are asserted against the flood, precisely because an earlier
//! version of this file tested only the second and shipped the first broken.
//!
//! ## Why the numbers are not here
//!
//! [`CreditWindow`] comes from `nysia-proto` and the daemon's [`nysia_proto::CreditGrant`] carries the
//! window it was issued under, so the client never holds its own copy of the constants
//! (D-13). A daemon that narrows the window for a slow renderer is obeyed on the next
//! grant, with no protocol change and no chance of the two ends disagreeing about what is
//! in force.
//!
//! An ack names no session. It rides a [`nysia_proto::FrameKind::Credit`] frame whose
//! header already carries the stream id, and a second copy in the payload could only ever
//! disagree with it.

use std::collections::HashMap;

use nysia_proto::credit::{CreditAck, CreditWindow};
use nysia_proto::stream::StreamId;

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
    pub fn attach(&mut self, stream: StreamId) {
        let initial = self.window.per_stream_initial;
        self.streams.entry(stream).or_insert(StreamCredit {
            credit: initial,
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
    pub fn is_attached(&self, stream: StreamId) -> bool {
        self.streams.contains_key(&stream)
    }

    /// Bytes this stream may still receive, which is the smaller of its own credit and the
    /// shared budget.
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

    /// Whether no attached stream can take another chunk of output.
    ///
    /// **The question the reader loop actually has**, and deliberately not "is the total
    /// spent?". With one session attached, per-stream credit runs out at 512 KiB while the
    /// 2 MiB total is still three quarters full; a reader gated on the total alone would go
    /// back to `read` and block against a daemon that had already stopped sending, and the
    /// session would wedge on its first burst with no error anywhere. See the module docs.
    ///
    /// "Cannot take a chunk" rather than "has exactly zero", because the daemon sends in
    /// [`CreditWindow::chunk`]-sized pieces and a balance that cannot cover one cannot cover
    /// anything. With nothing attached the answer is `false`: there is no session to stall,
    /// and a reader that stopped would never see the first frame of the next one.
    pub fn is_blocked(&self) -> bool {
        if self.streams.is_empty() {
            return false;
        }
        let chunk = self.window.chunk;
        self.total_credit < chunk || self.streams.keys().all(|s| self.available(*s) < chunk)
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
    /// Returns a [`CreditAck`] once the batch reaches [`CreditWindow::ack_batch`], and
    /// `None` while it is still accumulating. The reader also walks
    /// [`Self::streams_owing_an_ack`] on every pass, so a partial batch is returned by
    /// whichever pass notices it rather than waiting for one that happens to fill.
    ///
    /// # Errors
    ///
    /// [`CreditError::UnknownStream`], and [`CreditError::PhantomAck`] when the webview
    /// claims to have rendered more than it was sent.
    pub fn render(
        &mut self,
        stream: StreamId,
        bytes: u32,
    ) -> Result<Option<CreditAck>, CreditError> {
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
            return Ok(self.drain_ack(stream));
        }
        Ok(None)
    }

    /// Return whatever credit this stream has earned but not yet acknowledged.
    ///
    /// `None` when there is nothing to return. This is the idle flush: without it the last
    /// few kilobytes of every burst sit in `ungranted` forever, and a session that
    /// alternates between bursts and silence loses a little of its window on each one.
    ///
    /// The ledger's own balances move here too, not only the number on the wire: they are
    /// this client's mirror of the allowance the daemon will restore when the ack lands, and
    /// [`Self::is_blocked`] — which decides whether the reader keeps reading — is asked of
    /// the mirror.
    pub fn drain_ack(&mut self, stream: StreamId) -> Option<CreditAck> {
        let window = self.window;
        let total_credit = self.total_credit;
        let total_ungranted = self.total_ungranted;
        let entry = self.streams.get_mut(&stream)?;
        if entry.ungranted == 0 {
            return None;
        }

        // Neither balance may climb past its ceiling, so a stream that renders faster than
        // it receives does not bank credit indefinitely.
        let headroom = window
            .per_stream_max
            .saturating_sub(entry.credit.min(window.per_stream_max));
        let bytes = entry.ungranted.min(headroom);
        entry.ungranted = 0;
        entry.credit = entry
            .credit
            .saturating_add(bytes)
            .min(window.per_stream_max);

        let shared = total_ungranted.min(
            window
                .total_max
                .saturating_sub(total_credit.min(window.total_max)),
        );
        self.total_credit = total_credit.saturating_add(shared).min(window.total_max);
        self.total_ungranted = 0;

        if bytes == 0 {
            return None;
        }
        Some(CreditAck { bytes })
    }

    /// Every attached stream holding credit that has not been returned upstream.
    ///
    /// The reader walks this on each pass, so an ack leaves as soon as any pass notices it
    /// is owed. Without that, the only thing that ever wrote one was the render report that
    /// happened to fill a batch — and a stream whose last burst fell short of one sat on the
    /// credit forever.
    pub fn streams_owing_an_ack(&self) -> Vec<StreamId> {
        self.streams
            .iter()
            .filter(|(_, entry)| entry.ungranted > 0)
            .map(|(stream, _)| *stream)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> CreditLedger {
        let mut ledger = CreditLedger::new(CreditWindow::DEFAULT);
        ledger.attach(StreamId(1));
        ledger
    }

    #[test]
    fn a_fresh_stream_starts_on_the_windows_initial_credit() {
        let ledger = ledger();
        assert_eq!(
            ledger.available(StreamId(1)),
            CreditWindow::DEFAULT.per_stream_initial
        );
        assert!(!ledger.is_blocked());
        assert_eq!(ledger.unrendered(), 0);
        // A stream nobody attached has no credit, rather than unlimited credit.
        assert_eq!(ledger.available(StreamId(99)), 0);
    }

    #[test]
    fn receiving_spends_credit_and_rendering_earns_it_back() {
        let mut ledger = ledger();
        let chunk = CreditWindow::DEFAULT.chunk;

        ledger.receive(StreamId(1), chunk).unwrap();
        assert_eq!(
            ledger.available(StreamId(1)),
            CreditWindow::DEFAULT.per_stream_initial - chunk
        );
        assert_eq!(ledger.unrendered(), u64::from(chunk));

        // Under the ack batch: nothing goes upstream yet.
        assert_eq!(ledger.render(StreamId(1), chunk).unwrap(), None);
        assert_eq!(ledger.unrendered(), 0);

        let ack = ledger
            .drain_ack(StreamId(1))
            .expect("an idle flush returns the tail");
        assert_eq!(ack.bytes, chunk);
        assert_eq!(
            ledger.available(StreamId(1)),
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
            ledger.receive(StreamId(1), window.chunk).unwrap();
            if ledger.render(StreamId(1), window.chunk).unwrap().is_some() {
                grants += 1;
            }
            rendered += window.chunk;
        }
        assert_eq!(grants, 1, "exactly one grant, once the batch filled");
        assert!(rendered >= window.ack_batch);
    }

    /// The regression the reader loop wedged on: one stream, its own credit spent, the
    /// shared total still three quarters full.
    ///
    /// The earlier `is_blocked` asked only whether the *total* was spent, so it answered
    /// "keep reading" here — and the reader went back into `read` against a daemon that had
    /// already stopped sending, never drained its render reports, and never wrote the grant
    /// that would have freed it. Asserting on `available` alone could not catch it, because
    /// `available` was right and the loop's question was wrong.
    #[test]
    fn a_single_stream_out_of_its_own_credit_blocks_the_reader() {
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        ledger.attach(StreamId(1));

        while ledger.available(StreamId(1)) >= window.chunk {
            ledger.receive(StreamId(1), window.chunk).unwrap();
        }

        assert!(
            ledger.shared_credit() > window.chunk,
            "the shared budget must still be healthy or this proves nothing: {} left",
            ledger.shared_credit()
        );
        assert!(
            ledger.is_blocked(),
            "the reader must stop: this stream cannot take a chunk even though the total can"
        );
    }

    #[test]
    fn a_ledger_with_nothing_attached_does_not_stop_the_reader() {
        // There is no session to stall, and a reader that stopped here would never see the
        // first frame of the next one.
        let ledger = CreditLedger::new(CreditWindow::DEFAULT);
        assert!(!ledger.is_blocked());
    }

    #[test]
    fn one_exhausted_stream_does_not_stop_a_reader_that_still_has_another() {
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        ledger.attach(StreamId(1));
        ledger.attach(StreamId(2));

        while ledger.available(StreamId(1)) >= window.chunk {
            ledger.receive(StreamId(1), window.chunk).unwrap();
        }
        assert!(
            !ledger.is_blocked(),
            "stream 2 can still take a chunk, so its frames must keep arriving"
        );
    }

    /// Deliverable 4: unbounded output, bounded memory.
    ///
    /// `yes` in a pane nobody is rendering. The webview never acks, so credit is never
    /// returned, and the assertion is that the ledger stops the flood rather than absorbing
    /// it. Asserted through `is_blocked` — the predicate the reader actually branches on —
    /// as well as through `available`, because a previous version of this test checked only
    /// the second and the loop shipped broken.
    #[test]
    fn a_flood_with_no_acknowledgement_stops_at_the_ceiling() {
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        for stream in 1..=4 {
            ledger.attach(StreamId(stream));
        }

        let mut accepted: u64 = 0;
        let mut refused = 0;
        // Far more than any budget could hold: 4 streams × 512 KiB is 2 MiB, and this is
        // 48 MiB of offered output.
        for round in 0..1024u32 {
            let stream = StreamId(round % 4 + 1);
            if ledger.available(stream) < window.chunk {
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
        assert!(
            ledger.is_blocked(),
            "the reader must have stopped reading, which is what stalls the child"
        );
        for stream in 1..=4 {
            assert!(
                ledger.available(StreamId(stream)) < window.chunk,
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
        ledger.attach(StreamId(1));

        while ledger.available(StreamId(1)) >= window.chunk {
            ledger.receive(StreamId(1), window.chunk).unwrap();
        }
        assert!(ledger.is_blocked());

        let grant = ledger
            .render(StreamId(1), window.ack_batch)
            .unwrap()
            .expect("a full batch grants at once");
        assert_eq!(grant.bytes, window.ack_batch);
        assert!(!ledger.is_blocked(), "reading may resume");
    }

    #[test]
    fn a_stream_owing_a_grant_is_reported_so_any_pass_can_write_it() {
        // Without this the only thing that ever wrote a grant was the render report that
        // filled a batch, and a stream whose last burst fell short sat on the credit.
        let mut ledger = ledger();
        assert!(ledger.streams_owing_an_ack().is_empty());

        ledger.receive(StreamId(1), 4096).unwrap();
        ledger.render(StreamId(1), 4096).unwrap();
        assert_eq!(ledger.streams_owing_an_ack(), vec![StreamId(1)]);

        ledger.drain_ack(StreamId(1));
        assert!(ledger.streams_owing_an_ack().is_empty());
    }

    #[test]
    fn one_stream_cannot_spend_the_shared_budget_out_from_under_the_others() {
        // §7.3 gives a total as well as a per-stream figure precisely so that thirty noisy
        // sessions cost 2 MiB rather than thirty times 512 KiB.
        let window = CreditWindow::DEFAULT;
        let mut ledger = CreditLedger::new(window);
        for stream in 1..=8 {
            ledger.attach(StreamId(stream));
        }

        let mut total = 0u64;
        for stream in 1..=8 {
            while ledger.available(StreamId(stream)) >= window.chunk {
                ledger.receive(StreamId(stream), window.chunk).unwrap();
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
        ledger.attach(StreamId(1));
        ledger.attach(StreamId(2));

        ledger
            .receive(StreamId(1), window.per_stream_initial)
            .unwrap();
        assert_eq!(
            ledger.shared_credit(),
            window.total_initial - window.per_stream_initial
        );

        ledger.detach(StreamId(1));
        assert!(!ledger.is_attached(StreamId(1)));
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
            ledger.receive(StreamId(1), over),
            Err(CreditError::Overrun {
                stream: StreamId(1),
                sent: over,
                credit: CreditWindow::DEFAULT.per_stream_initial,
            })
        );
        // Refused, not partially applied.
        assert_eq!(
            ledger.available(StreamId(1)),
            CreditWindow::DEFAULT.per_stream_initial
        );
        assert_eq!(ledger.unrendered(), 0);
    }

    #[test]
    fn a_webview_claiming_to_have_rendered_bytes_it_never_saw_is_refused() {
        // Returning that credit upstream would raise the ceiling by exactly the lie.
        let mut ledger = ledger();
        ledger.receive(StreamId(1), 100).unwrap();
        assert_eq!(
            ledger.render(StreamId(1), 101),
            Err(CreditError::PhantomAck {
                stream: StreamId(1),
                acked: 101,
                unrendered: 100,
            })
        );
        assert_eq!(ledger.unrendered(), 100);
    }

    #[test]
    fn frames_and_acks_for_an_unattached_stream_are_refused() {
        let mut ledger = ledger();
        assert_eq!(
            ledger.receive(StreamId(7), 1),
            Err(CreditError::UnknownStream(StreamId(7)))
        );
        assert_eq!(
            ledger.render(StreamId(7), 1),
            Err(CreditError::UnknownStream(StreamId(7)))
        );
        assert_eq!(ledger.drain_ack(StreamId(7)), None);
    }

    #[test]
    fn attaching_twice_does_not_re_grant_credit_the_daemon_has_spent() {
        let mut ledger = ledger();
        ledger.receive(StreamId(1), 4096).unwrap();
        let spent = ledger.available(StreamId(1));

        ledger.attach(StreamId(1));
        assert_eq!(
            ledger.available(StreamId(1)),
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
