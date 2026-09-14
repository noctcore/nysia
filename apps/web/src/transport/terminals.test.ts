import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { CreditWindow } from '../generated/CreditWindow';
import { CREDIT_WINDOW_DEFAULT } from '../generated/wireConstants';
import type { DaemonBridge } from './bridge';
import type { StreamId } from './frames';
import type { TerminalFactory, XtermLike } from './surface/XtermSurface';
import { TerminalRouter } from './terminals';
import { encodeFrame } from './testFrames';

/**
 * The two halves of the credit window this side of the channel is responsible for.
 *
 * Both were dead before these existed: no credit frame reached the webview at all, so
 * `CreditLedger.adopt` had no callers and this side ran on the compiled-in defaults; and
 * nothing ever drained a part-filled batch, so the doc for the idle flush described a caller
 * that was not there.
 */

/** A terminal that renders instantly and records nothing, so no DOM is needed (D-18). */
function stubTerminals(): TerminalFactory {
  return (): XtermLike => ({
    element: undefined,
    open: () => {},
    write: (_data, done) => done?.(),
    resize: () => {},
    fit: () => undefined,
    focus: () => {},
    dispose: () => {},
    onData: () => ({ dispose: () => {} }),
  });
}

/** A bridge that records every `terminal_ack` and answers nothing else. */
function recordingBridge(): {
  bridge: DaemonBridge;
  acks: { stream: StreamId; bytes: number }[];
} {
  const acks: { stream: StreamId; bytes: number }[] = [];
  const bridge = {
    async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      if (command === 'terminal_ack') {
        acks.push({
          stream: args?.stream as StreamId,
          bytes: args?.bytes as number,
        });
      }
      return undefined as T;
    },
    async attachChannel(): Promise<void> {},
    window: {} as DaemonBridge['window'],
  } as unknown as DaemonBridge;
  return { bridge, acks };
}

function build(): {
  router: TerminalRouter;
  acks: { stream: StreamId; bytes: number }[];
} {
  const { bridge, acks } = recordingBridge();
  const router = new TerminalRouter({
    bridge,
    createTerminal: stubTerminals(),
    platform: 'windows',
  });
  return { router, acks };
}

/** A credit frame carrying `window`, as the daemon writes one. */
function grant(stream: StreamId, window: CreditWindow): Uint8Array {
  const payload = new TextEncoder().encode(
    JSON.stringify({ credit: 'grant', bytes: window.perStreamInitial, window }),
  );
  return encodeFrame('credit', stream, payload);
}

describe('the window the daemon advertises', () => {
  it('is adopted from the credit frame it rides on', () => {
    // D-13: the daemon owns the window and announces it, and the client never keeps its own
    // copy of the constants. That held in Rust only — the reader consumed the credit frame
    // and returned before delivery, so this ledger never saw one.
    const { router } = build();
    const narrower: CreditWindow = {
      ...CREDIT_WINDOW_DEFAULT,
      ackBatch: Math.floor(CREDIT_WINDOW_DEFAULT.ackBatch / 4),
    };

    expect(router.deliver(grant(1, narrower))).toBeNull();
    expect(router.creditWindow.ackBatch).toBe(narrower.ackBatch);
  });

  it('is left alone when the frame does not parse or is not a grant', () => {
    // A credit frame this build cannot read is the daemon's problem to fix, not a reason to
    // stop rendering. The window in force stays in force, as it does in Rust.
    const { router } = build();
    const before = router.creditWindow;

    expect(router.deliver(encodeFrame('credit', 1, new TextEncoder().encode('{')))).toBeNull();
    expect(
      router.deliver(
        encodeFrame('credit', 1, new TextEncoder().encode(JSON.stringify({ credit: 'ack', bytes: 8 }))),
      ),
    ).toBeNull();

    expect(router.creditWindow).toBe(before);
  });

  it('refuses one that would deadlock rather than adopting it', () => {
    const { router } = build();
    const before = router.creditWindow;
    const deadlocking: CreditWindow = {
      ...CREDIT_WINDOW_DEFAULT,
      ackBatch: CREDIT_WINDOW_DEFAULT.pendingCap + 1,
    };

    router.deliver(grant(1, deadlocking));
    expect(router.creditWindow).toBe(before);
  });
});

describe('acknowledging a batch that never fills', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('flushes the tail of a burst once the session goes quiet', () => {
    // Without this the daemon holds that much of the stream's allowance for ever. It is not
    // only a slow leak: `CreditWindow::is_coherent` does not require the ack batch to fit
    // inside the per-stream allowance, so a window the daemon may legitimately advertise can
    // leave a batch that cannot fill — and the pane then stops dead with nothing in the log.
    const { router, acks } = build();
    const short = CREDIT_WINDOW_DEFAULT.chunk;

    router.surface(1).show({} as HTMLElement);
    router.deliver(encodeFrame('output', 1, new Uint8Array(short)));
    expect(acks, 'a partial batch must not be acknowledged immediately').toHaveLength(0);

    vi.runAllTimers();
    expect(acks).toEqual([{ stream: 1, bytes: short }]);
  });

  it('does not acknowledge the same bytes twice when the batch fills first', () => {
    const { router, acks } = build();
    const window = CREDIT_WINDOW_DEFAULT;

    router.surface(1).show({} as HTMLElement);
    let sent = 0;
    while (sent < window.ackBatch) {
      router.deliver(encodeFrame('output', 1, new Uint8Array(window.chunk)));
      sent += window.chunk;
    }
    expect(acks).toHaveLength(1);

    // The scheduled flush was cancelled by the batch that beat it, so nothing follows.
    vi.runAllTimers();
    expect(acks).toHaveLength(1);
  });

  it('drops a pending flush when the pane closes', () => {
    // `close` already sends the final ack itself. A timer that fired afterwards would report
    // bytes for a stream the daemon has forgotten.
    const { router, acks } = build();

    router.surface(1).show({} as HTMLElement);
    router.deliver(encodeFrame('output', 1, new Uint8Array(CREDIT_WINDOW_DEFAULT.chunk)));
    router.close(1);
    const afterClose = acks.length;

    vi.runAllTimers();
    expect(acks).toHaveLength(afterClose);
  });

  it('drops a pending flush when the connection is replaced', () => {
    // The connection the ack would travel on is gone, and the daemon released that
    // connection's credit with its ids.
    const { router, acks } = build();

    router.surface(1).show({} as HTMLElement);
    router.deliver(encodeFrame('output', 1, new Uint8Array(CREDIT_WINDOW_DEFAULT.chunk)));
    router.resetStreams();

    vi.runAllTimers();
    expect(acks).toHaveLength(0);
  });
});

describe('the replay boundary reaches the pane it belongs to', () => {
  /** A stand-in for the pane element. The stub terminal never touches it. */
  const host = (): HTMLElement => ({}) as HTMLElement;

  it('opens the gate on the stream the marker names, and no other', () => {
    // Routing, which is the mistake this file exists to catch: a boundary delivered to the
    // wrong pane opens one that is still replaying and leaves the right one shut until its
    // deadline. Two streams in one delivery is the ordinary case, not an edge one.
    const { router } = build();
    const replaying = router.surface(1);
    const other = router.surface(2);
    replaying.show(host());
    other.show(host());

    expect(router.deliver(encodeFrame('replay_end', 1, new Uint8Array(0)))).toBeNull();

    expect(replaying.acceptsInput).toBe(true);
    expect(other.acceptsInput).toBe(false);
  });

  it('builds the pane the marker names rather than dropping it', () => {
    // A session with no scrollback replays nothing, so the boundary can be the first frame
    // its stream ever carries. Looked up instead of built, it would fall on the floor — and
    // the pane the delivery path builds on the next byte would sit shut for its whole
    // deadline, on a brand-new session where there was never anything to hold it for.
    const { router } = build();
    expect(router.deliver(encodeFrame('replay_end', 9, new Uint8Array(0)))).toBeNull();

    const surface = router.surface(9);
    surface.show(host());
    expect(surface.acceptsInput).toBe(true);
  });
});
