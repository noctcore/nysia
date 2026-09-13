import type { CreditWindow } from '../generated/CreditWindow';
import { CREDIT_WINDOW_DEFAULT } from '../generated/wireConstants';
import type { StreamId } from './frames';

/**
 * The webview's half of the credit window (§7.3).
 *
 * The daemon may only send bytes it has been granted, and credit comes back when the
 * webview says it has **rendered** them. The whole design turns on where "rendered" is
 * measured:
 *
 * > The web side acks after xterm's `write()` callback, not before.
 *
 * xterm's `write` is asynchronous — it parses on a timer to keep the main thread
 * responsive — so a `write` that has returned has *queued* bytes, not painted them.
 * Acknowledging there would return credit for work not yet done, the daemon would send
 * more, and the queue inside xterm would grow without bound. Which is precisely the
 * unbounded buffer the credit window exists to prevent, moved one layer in where nothing
 * measures it. So the ack goes in the callback, and this ledger exists to make that
 * cheap: acks are batched to the window's `ackBatch` so a busy session does not spend a
 * round trip per chunk.
 *
 * The numbers come from `generated/wireConstants`, which `nysia-proto` writes (D-13), and
 * a grant carries the window in force — so a daemon that narrows the window for a slow
 * renderer is obeyed without a protocol change. {@link CreditLedger.adopt} is how.
 */

/** What the ledger asks the caller to send upstream. */
export interface PendingAck {
  readonly stream: StreamId;
  /** Bytes rendered since the last ack. */
  readonly bytes: number;
}

/**
 * Batched render acknowledgements, per stream.
 *
 * Pure and synchronous: it counts and it decides, and the caller does the invoking. That
 * is what lets the batching policy be tested against a clock nobody has to wait for.
 */
export class CreditLedger {
  #window: CreditWindow;
  /** Rendered but not yet acknowledged, per stream. */
  readonly #pending = new Map<StreamId, number>();
  /** Delivered but not yet reported rendered, per stream. */
  readonly #inFlight = new Map<StreamId, number>();

  constructor(window: CreditWindow = CREDIT_WINDOW_DEFAULT) {
    this.#window = window;
  }

  /** The window in force. */
  get window(): CreditWindow {
    return this.#window;
  }

  /**
   * Adopt a window the daemon advertised on a grant.
   *
   * An incoherent window is refused rather than adopted, and the one in force stays in
   * force. §7.3's parameters deadlock if the ack batch exceeds the pending cap — the
   * daemon stops at the cap while the webview is still short of enough bytes to ack — and
   * a stall with no cause is far worse to debug than a warning here.
   */
  adopt(window: CreditWindow): boolean {
    if (!isCoherent(window)) {
      return false;
    }
    this.#window = window;
    return true;
  }

  /** Bytes delivered to this stream but not yet reported rendered. */
  inFlight(stream: StreamId): number {
    return this.#inFlight.get(stream) ?? 0;
  }

  /** Bytes rendered but not yet acknowledged upstream. */
  pending(stream: StreamId): number {
    return this.#pending.get(stream) ?? 0;
  }

  /**
   * Record bytes handed to the surface.
   *
   * Called when a frame is routed, before anything is painted. Pairs with
   * {@link rendered}, and the gap between the two is what the daemon's ceiling bounds.
   */
  delivered(stream: StreamId, bytes: number): void {
    this.#inFlight.set(stream, this.inFlight(stream) + bytes);
  }

  /**
   * Record bytes the surface has finished rendering.
   *
   * Returns the ack to send once the batch is full, and `null` while it is still filling.
   * Call {@link drain} when the stream goes quiet, or the tail of every burst is never
   * acknowledged and the window shrinks by that much for the life of the session.
   *
   * Rendering more than was delivered is clamped rather than trusted. The ledger is the
   * webview's own bookkeeping, so a surface that double-counted would inflate the credit
   * returned and quietly raise the daemon's ceiling by the size of the mistake.
   */
  rendered(stream: StreamId, bytes: number): PendingAck | null {
    const outstanding = this.inFlight(stream);
    const counted = Math.min(bytes, outstanding);
    if (counted <= 0) {
      return null;
    }

    this.#inFlight.set(stream, outstanding - counted);
    const pending = this.pending(stream) + counted;
    this.#pending.set(stream, pending);

    return pending >= this.#window.ackBatch ? this.drain(stream) : null;
  }

  /**
   * Take whatever this stream has rendered but not yet acknowledged.
   *
   * The idle flush. Without it a session that alternates between bursts and silence loses
   * a little of its window on every burst, and after enough of them it stalls.
   */
  drain(stream: StreamId): PendingAck | null {
    const bytes = this.pending(stream);
    if (bytes <= 0) {
      return null;
    }
    this.#pending.delete(stream);
    return { stream, bytes };
  }

  /**
   * Forget a stream whose pane has closed.
   *
   * Returns the final ack, if it had rendered anything unacknowledged. Sending it matters:
   * a stream closed mid-burst would otherwise leave the daemon holding credit for a pane
   * that no longer exists.
   */
  forget(stream: StreamId): PendingAck | null {
    const final = this.drain(stream);
    this.#inFlight.delete(stream);
    return final;
  }
}

/**
 * Whether a window's parameters can all hold at once.
 *
 * The same check `CreditWindow::is_coherent` makes in Rust, because a client that adopted
 * a deadlocking window would stall with no error anywhere. Cheap to check and expensive to
 * discover.
 */
export function isCoherent(window: CreditWindow): boolean {
  return (
    window.perStreamInitial <= window.perStreamMax &&
    window.totalInitial <= window.totalMax &&
    window.perStreamInitial <= window.totalInitial &&
    window.perStreamMax <= window.totalMax &&
    window.ackBatch <= window.pendingCap &&
    window.chunk <= window.pendingCap &&
    window.chunk > 0
  );
}
