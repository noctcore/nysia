import type { SessionHandle } from '../generated/SessionHandle';
import type { DaemonBridge } from './bridge';
import { CreditLedger } from './credit';
import { bySession, FrameDecoder, type StreamId } from './frames';
import type { TerminalSurface } from './surface/TerminalSurface';
import { XtermSurface, type TerminalFactory } from './surface/XtermSurface';
import { policyFor, WebglPool } from './surface/webglPool';

/**
 * Where a delivery on the one Channel ends up.
 *
 * Decode, group by session, write into that session's surface, and return credit as each
 * surface reports what it has rendered. Everything it composes is tested on its own; what
 * this adds is the wiring, and the wiring is where the two mistakes that matter live:
 * routing a frame to the wrong pane, and returning credit for a pane that has gone.
 */

/**
 * Which stream id a session's output arrives on.
 *
 * **This is the webview half of the shim**, and the counterpart of
 * `daemon::stream::attach_frame` in Rust. `nysia-proto` has no control-plane attach verb
 * yet, so neither side is *told* the mapping; both derive it from the order sessions are
 * first seen, which is what the Rust `StreamTable` does when it assigns dense, monotonic
 * ids. Two implementations of one convention, which is exactly the arrangement that rots —
 * so it is one function on each side, named in the other's docs, and both are deleted
 * together when W1 lands the attach verb.
 */
export function streamIdFor(
  handle: SessionHandle,
  order: readonly SessionHandle[],
): StreamId | null {
  const at = order.indexOf(handle);
  return at < 0 ? null : at;
}

/** Owns every live surface and the flow control behind them. */
export class TerminalRouter {
  readonly #bridge: DaemonBridge;
  readonly #createTerminal: TerminalFactory;
  readonly #frameDecoder = new FrameDecoder();
  readonly #ledger = new CreditLedger();
  readonly #surfaces = new Map<StreamId, XtermSurface>();
  #pool: WebglPool;
  #onBell: (stream: StreamId) => void = () => {};
  #onExit: (stream: StreamId) => void = () => {};

  constructor(options: {
    readonly bridge: DaemonBridge;
    readonly createTerminal: TerminalFactory;
    /** `std::env::consts::OS`, from the `host_platform` command. */
    readonly platform: string;
  }) {
    this.#bridge = options.bridge;
    this.#createTerminal = options.createTerminal;
    this.#pool = new WebglPool(policyFor(options.platform));
  }

  /** Adopt the renderer policy for a platform learned after construction. */
  setPlatform(platform: string): void {
    this.#pool = new WebglPool(policyFor(platform));
  }

  /** Be told when a session rings its bell or its child exits. */
  on(events: {
    readonly bell?: (stream: StreamId) => void;
    readonly exit?: (stream: StreamId) => void;
  }): void {
    this.#onBell = events.bell ?? this.#onBell;
    this.#onExit = events.exit ?? this.#onExit;
  }

  /** The surface for a stream, built on first use. */
  surface(stream: StreamId): TerminalSurface {
    const existing = this.#surfaces.get(stream);
    if (existing) {
      return existing;
    }
    const surface = new XtermSurface({
      stream,
      pool: this.#pool,
      createTerminal: this.#createTerminal,
      onRendered: (which, bytes) => this.#rendered(which, bytes),
    });
    this.#surfaces.set(stream, surface);
    return surface;
  }

  /**
   * Route one delivery from the Channel.
   *
   * Returns the error if the channel proved unreadable, rather than throwing: this runs
   * inside the Channel's `onmessage`, where a throw goes nowhere anyone can see it.
   */
  deliver(delivery: Uint8Array): Error | null {
    let frames;
    try {
      frames = this.#frameDecoder.push(delivery);
    } catch (cause) {
      return cause instanceof Error ? cause : new Error(String(cause));
    }

    for (const [stream, group] of bySession(frames)) {
      for (const frame of group) {
        switch (frame.kind) {
          case 'output': {
            this.#ledger.delivered(stream, frame.payload.length);
            this.surface(stream).write(frame.payload);
            break;
          }
          case 'bell':
            this.#onBell(stream);
            break;
          case 'exit':
            this.#onExit(stream);
            break;
          // `osc133` is shell-integration state and `credit` is the daemon advertising its
          // window; neither paints anything, and the Rust side has already acted on the
          // credit frame before it reached here.
          case 'osc133':
          case 'credit':
            break;
        }
      }
    }
    return null;
  }

  /** A pane closed: release its surface and return its outstanding credit. */
  close(stream: StreamId): void {
    const surface = this.#surfaces.get(stream);
    if (!surface) {
      return;
    }
    this.#surfaces.delete(stream);
    surface.dispose();

    // The final ack matters: a stream closed mid-burst would otherwise leave the daemon
    // holding credit for a pane that no longer exists, and the shared budget would shrink
    // by that much for the rest of the session.
    const final = this.#ledger.forget(stream);
    if (final) {
      void this.#ack(final.stream, final.bytes);
    }
  }

  /** Tear everything down. */
  dispose(): void {
    for (const stream of [...this.#surfaces.keys()]) {
      this.close(stream);
    }
    this.#frameDecoder.reset();
  }

  /** A surface has painted; batch the acknowledgement and send it when the batch fills. */
  #rendered(stream: StreamId, bytes: number): void {
    const ack = this.#ledger.rendered(stream, bytes);
    if (ack) {
      void this.#ack(ack.stream, ack.bytes);
    }
  }

  async #ack(stream: StreamId, bytes: number): Promise<void> {
    try {
      await this.#bridge.invoke('terminal_ack', { stream, bytes });
    } catch {
      // A failed ack means the connection has gone, which the store learns from
      // `daemon_watch` and handles by reconnecting. Reporting it here would put a notice in
      // front of the user for every unacknowledged batch of a disconnect they have already
      // been told about.
    }
  }
}
