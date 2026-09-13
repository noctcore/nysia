import { describe, expect, it } from 'vitest';

import type { CreditWindow } from '../generated/CreditWindow';
import { CREDIT_WINDOW_DEFAULT } from '../generated/wireConstants';
import { CreditLedger, isCoherent } from './credit';

const WINDOW = CREDIT_WINDOW_DEFAULT;

describe('the render ledger', () => {
  it('starts on the window nysia-proto generated', () => {
    // Not a copy of the numbers: the daemon and the webview read the same generated
    // constants, so there is one authority on the wire (D-13).
    expect(new CreditLedger().window).toBe(CREDIT_WINDOW_DEFAULT);
  });

  it('holds an ack back until the batch is full', () => {
    // A grant per chunk would spend a quarter of the channel on acknowledgements.
    const ledger = new CreditLedger();
    let acks = 0;
    let rendered = 0;

    while (rendered < WINDOW.ackBatch) {
      ledger.delivered(1, WINDOW.chunk);
      if (ledger.rendered(1, WINDOW.chunk) !== null) {
        acks += 1;
      }
      rendered += WINDOW.chunk;
    }

    expect(acks).toBe(1);
    expect(rendered).toBeGreaterThanOrEqual(WINDOW.ackBatch);
  });

  it('acks the whole batch at once, not the chunk that completed it', () => {
    const ledger = new CreditLedger();
    let sent = 0;
    for (;;) {
      ledger.delivered(1, WINDOW.chunk);
      const ack = ledger.rendered(1, WINDOW.chunk);
      sent += WINDOW.chunk;
      if (ack !== null) {
        expect(ack.stream).toBe(1);
        expect(ack.bytes).toBe(sent);
        break;
      }
    }
    expect(ledger.pending(1)).toBe(0);
  });

  it('batches each session separately', () => {
    // One busy pane must not carry a quiet one's ack over the line with it, or the quiet
    // pane's credit is returned for bytes it never rendered.
    const ledger = new CreditLedger();
    ledger.delivered(1, WINDOW.ackBatch);
    ledger.delivered(2, 64);

    const ack = ledger.rendered(1, WINDOW.ackBatch);
    expect(ack).toEqual({ stream: 1, bytes: WINDOW.ackBatch });

    ledger.rendered(2, 64);
    expect(ledger.pending(2)).toBe(64);
  });

  it('returns the tail of a burst when the stream goes quiet', () => {
    // Without the idle flush, a session that alternates between bursts and silence loses a
    // little of its window on every burst and eventually stalls.
    const ledger = new CreditLedger();
    ledger.delivered(3, 900);
    expect(ledger.rendered(3, 900)).toBeNull();

    expect(ledger.drain(3)).toEqual({ stream: 3, bytes: 900 });
    expect(ledger.drain(3)).toBeNull();
  });

  it('tracks what has been handed over but not yet painted', () => {
    // The gap the daemon's ceiling bounds. xterm's write is asynchronous, so this is never
    // zero on a busy session — and acking on `write` returning rather than on its callback
    // is exactly the bug that would make it invisible.
    const ledger = new CreditLedger();
    ledger.delivered(1, 4096);
    expect(ledger.inFlight(1)).toBe(4096);

    ledger.rendered(1, 1024);
    expect(ledger.inFlight(1)).toBe(3072);
    expect(ledger.pending(1)).toBe(1024);
  });

  it('clamps a surface that reports rendering more than it was given', () => {
    // The ledger is the webview's own bookkeeping, so a double-counting surface would
    // inflate the credit returned and quietly raise the daemon's ceiling by the mistake.
    const ledger = new CreditLedger();
    ledger.delivered(1, 100);

    ledger.rendered(1, 500);
    expect(ledger.pending(1)).toBe(100);
    expect(ledger.inFlight(1)).toBe(0);

    expect(ledger.rendered(1, 50)).toBeNull();
    expect(ledger.pending(1)).toBe(100);
  });

  it('ignores a report for a stream that was never given anything', () => {
    const ledger = new CreditLedger();
    expect(ledger.rendered(99, 1024)).toBeNull();
    expect(ledger.pending(99)).toBe(0);
  });

  it('sends a final ack when a pane closes mid-burst', () => {
    // Otherwise the daemon holds credit for a pane that no longer exists, and the shared
    // budget shrinks by that much for the rest of the session.
    const ledger = new CreditLedger();
    ledger.delivered(4, 2048);
    ledger.rendered(4, 2048);

    expect(ledger.forget(4)).toEqual({ stream: 4, bytes: 2048 });
    expect(ledger.pending(4)).toBe(0);
    expect(ledger.inFlight(4)).toBe(0);
    expect(ledger.forget(4)).toBeNull();
  });
});

describe('adopting a window the daemon sent', () => {
  it('takes a narrower one, so a slow renderer needs no protocol change', () => {
    const ledger = new CreditLedger();
    const narrower: CreditWindow = { ...WINDOW, ackBatch: 32 * 1024 };
    expect(ledger.adopt(narrower)).toBe(true);
    expect(ledger.window).toBe(narrower);

    ledger.delivered(1, 32 * 1024);
    expect(ledger.rendered(1, 32 * 1024)).toEqual({ stream: 1, bytes: 32 * 1024 });
  });

  it('refuses one that would deadlock, and keeps the working one', () => {
    // Batching acks past the pending cap stalls: the daemon stops at the cap while the
    // webview is still short of enough bytes to ack. A stall with no cause is far worse to
    // debug than a refusal here.
    const ledger = new CreditLedger();
    expect(ledger.adopt({ ...WINDOW, ackBatch: WINDOW.pendingCap + 1 })).toBe(false);
    expect(ledger.window).toBe(CREDIT_WINDOW_DEFAULT);
  });
});

describe('window coherence', () => {
  it('accepts the generated default', () => {
    // The same check `CreditWindow::is_coherent` makes in Rust. If the shipped default ever
    // failed it, every stream would stall at once.
    expect(isCoherent(CREDIT_WINDOW_DEFAULT)).toBe(true);
  });

  it('rejects every way the parameters can contradict each other', () => {
    const broken: readonly [string, CreditWindow][] = [
      ['acks batched past the pending cap', { ...WINDOW, ackBatch: WINDOW.pendingCap + 1 }],
      ['a chunk larger than the pending cap', { ...WINDOW, chunk: WINDOW.pendingCap + 1 }],
      ['a zero chunk, which sends nothing forever', { ...WINDOW, chunk: 0 }],
      [
        'one stream granted more than every stream together',
        { ...WINDOW, perStreamInitial: WINDOW.totalInitial + 1 },
      ],
      ['a per-stream max over the total max', { ...WINDOW, perStreamMax: WINDOW.totalMax + 1 }],
      [
        'an initial larger than its maximum, which never refills',
        { ...WINDOW, totalInitial: WINDOW.totalMax + 1 },
      ],
    ];

    for (const [why, window] of broken) {
      expect(isCoherent(window), why).toBe(false);
    }
  });
});
