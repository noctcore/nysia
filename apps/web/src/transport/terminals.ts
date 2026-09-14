import type { CreditGrant } from '../generated/CreditGrant';
import type { CreditWindow } from '../generated/CreditWindow';
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
 * How long a stream may sit on a part-filled batch before it is acknowledged anyway.
 *
 * Short enough that the daemon is not left holding the tail of a burst, long enough that a
 * busy session still batches rather than spending an IPC round trip per repaint. The
 * coalescing window upstream is 16 ms, so this is a few of those.
 */
const IDLE_ACK_MS = 50;

/** Whether a decoded credit frame is the daemon advertising a window. */
function isGrant(frame: unknown): frame is CreditGrant & { credit: 'grant' } {
  if (typeof frame !== 'object' || frame === null) {
    return false;
  }
  const tagged = frame as { credit?: unknown; window?: unknown };
  return (
    tagged.credit === 'grant' && typeof tagged.window === 'object' && tagged.window !== null
  );
}

/** Owns every live surface and the flow control behind them. */
export class TerminalRouter {
  readonly #bridge: DaemonBridge;
  readonly #createTerminal: TerminalFactory;
  readonly #frameDecoder = new FrameDecoder();
  readonly #ledger = new CreditLedger();
  readonly #surfaces = new Map<StreamId, XtermSurface>();
  /** Pending idle flushes, one per stream. See {@link TerminalRouter.#rendered}. */
  readonly #idleAcks = new Map<StreamId, ReturnType<typeof setTimeout>>();
  readonly #pool: WebglPool;
  #onBell: (stream: StreamId) => void = () => {};
  #onExit: (stream: StreamId) => void = () => {};
  #onReplayTimeout: (stream: StreamId, droppedChars: number) => void = () => {};

  constructor(options: {
    readonly bridge: DaemonBridge;
    readonly createTerminal: TerminalFactory;
    /**
     * `std::env::consts::OS`, from the `host_platform` command.
     *
     * Optional because it arrives a round trip later than the first pane can. Until it
     * does the pool runs the cautious policy — see the constructor.
     */
    readonly platform?: string;
  }) {
    this.#bridge = options.bridge;
    this.#createTerminal = options.createTerminal;
    // The cautious policy until `host_platform` answers, not a guess at the common case.
    // Guessing "windows" hands out an *uncapped* pool, so any pane built in that first round
    // trip keeps an unbounded allowance — and on macOS, where the cap exists because WebKit
    // counts contexts app-wide, being wrong that way is the expensive direction. DOM for a
    // few milliseconds is correct everywhere and costs a frame.
    this.#pool = new WebglPool(policyFor(options.platform ?? ''));
  }

  /** Adopt the renderer policy for a platform learned after construction. */
  setPlatform(platform: string): void {
    this.#pool.adopt(policyFor(platform));
  }

  /**
   * Be told when a session rings its bell, its child exits, or a replay never ended.
   *
   * `replayTimeout` is the observable half of the input gate. A surface holds input shut
   * from the moment it is built until the daemon's `replay_end` frame has been parsed, and
   * if that frame never arrives the deadline opens the gate anyway — a pane that refused
   * input for ever would be worse than the defect the gate closes. But it must not open in
   * silence: keystrokes were dropped, and "the first few seconds of typing did nothing" with
   * no notice anywhere is how a user concludes the window is broken.
   */
  on(events: {
    readonly bell?: (stream: StreamId) => void;
    readonly exit?: (stream: StreamId) => void;
    readonly replayTimeout?: (stream: StreamId, droppedChars: number) => void;
  }): void {
    this.#onBell = events.bell ?? this.#onBell;
    this.#onExit = events.exit ?? this.#onExit;
    this.#onReplayTimeout = events.replayTimeout ?? this.#onReplayTimeout;
  }

  /**
   * The window in force, as the daemon last advertised it.
   *
   * Exposed so a test can see that the announcement was adopted. Nothing in the app reads
   * it: the ledger applies it, and a second reader of the numbers is a second place they
   * could be wrong.
   */
  get creditWindow(): CreditWindow {
    return this.#ledger.window;
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
      onReplayTimeout: (which, dropped) => this.#onReplayTimeout(which, dropped),
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
          case 'credit':
            // The daemon advertising the window in force. It paints nothing, but this
            // ledger is what decides when a render is worth acknowledging, and this frame
            // is the only way it can learn the numbers it decides against (D-13).
            //
            // Rust adopts it too, for its own mirror. Two adoptions of one announcement is
            // not two sources of truth — the daemon remains the only one — and the
            // alternative was this side running on compiled-in defaults for ever.
            this.#adopt(frame.payload);
            break;
          case 'replay_end':
            // The seam between the scrollback the daemon replayed and what the child writes
            // next. It paints nothing; what it does is tell this pane that the bytes ahead of
            // it are the last of the replay, so the surface can stop swallowing what the
            // terminal reports once it has finished parsing them. The renderer answers no
            // query at all any more — `surface/muteReplies.ts` displaces every responder,
            // because the daemon has already answered by the time these bytes arrive — so
            // this gate is what covers the keystrokes typed at a pane still painting history,
            // and whatever a future renderer emits that the mute's table does not name. See
            // `XtermSurface` for both layers and what each costs.
            //
            // `surface(stream)` rather than a lookup, so a session with *no* scrollback still
            // gets its gate opened: the marker can be the first frame that stream ever
            // delivers, and a pane built lazily on the next byte would have opened on the
            // deadline instead.
            this.surface(stream).replayEnded();
            break;
          // Shell-integration state. Nothing here paints it yet.
          case 'osc133':
            break;
        }
      }
    }
    return null;
  }

  /**
   * Take the record of output this pane discarded while it was hidden, in bytes.
   *
   * Zero for a surface that has not been built, which is the ordinary case: a pane nobody
   * has opened has nothing to have lost.
   */
  takeDroppedWhileHidden(stream: StreamId): number {
    return this.#surfaces.get(stream)?.takeDroppedWhileHidden() ?? 0;
  }

  /** A pane closed: release its surface and return its outstanding credit. */
  close(stream: StreamId): void {
    const surface = this.#surfaces.get(stream);
    if (!surface) {
      return;
    }
    this.#surfaces.delete(stream);
    this.#cancelIdleAck(stream);
    surface.dispose();

    // The final ack matters: a stream closed mid-burst would otherwise leave the daemon
    // holding credit for a pane that no longer exists, and the shared budget would shrink
    // by that much for the rest of the session.
    const final = this.#ledger.forget(stream);
    if (final) {
      void this.#ack(final.stream, final.bytes);
    }
  }

  /**
   * Drop every surface and the decoder's half-read frame, for a new stream connection.
   *
   * Ids do not survive a reconnect — proto scopes them to one stream connection — so a
   * surface keyed by one describes nothing on the next. A daemon whose counter restarted
   * would otherwise hand the first new session id 1 and have its output painted into
   * whichever pane held id 1 before.
   *
   * Unlike {@link dispose} this sends no final acks: the connection they would travel on is
   * gone, and the daemon has already released that connection's credit with the ids.
   */
  resetStreams(): void {
    for (const surface of this.#surfaces.values()) {
      surface.dispose();
    }
    this.#surfaces.clear();
    for (const stream of [...this.#idleAcks.keys()]) {
      this.#cancelIdleAck(stream);
    }
    this.#frameDecoder.reset();
  }

  /** Tear everything down. */
  dispose(): void {
    for (const stream of [...this.#surfaces.keys()]) {
      this.close(stream);
    }
    for (const stream of [...this.#idleAcks.keys()]) {
      this.#cancelIdleAck(stream);
    }
    this.#frameDecoder.reset();
  }

  /**
   * Adopt a window the daemon advertised, ignoring anything that does not parse.
   *
   * A credit frame that cannot be read is the daemon's problem to fix, not a reason to stop
   * rendering: the window already in force stays in force, which is the same thing the Rust
   * side does with one.
   */
  #adopt(payload: Uint8Array): void {
    let frame: unknown;
    try {
      frame = JSON.parse(new TextDecoder().decode(payload));
    } catch {
      return;
    }
    if (!isGrant(frame)) {
      return;
    }
    this.#ledger.adopt(frame.window);
  }

  /** A surface has painted; batch the acknowledgement and send it when the batch fills. */
  #rendered(stream: StreamId, bytes: number): void {
    const ack = this.#ledger.rendered(stream, bytes);
    if (ack) {
      this.#cancelIdleAck(stream);
      void this.#ack(ack.stream, ack.bytes);
      return;
    }

    // **A partial batch has to leave on its own.** Without this the tail of every burst sits
    // here unacknowledged: the daemon is holding that much of the stream's allowance, and a
    // session that alternates between bursts and silence loses a little of its window each
    // time. `CreditWindow::is_coherent` does not require the ack batch to fit inside the
    // per-stream allowance, so a window the daemon is entitled to advertise can leave a
    // batch that will never fill — at which point the pane stops dead with nothing in the
    // log. Rust has had this flush since it was written; this side only claimed to.
    this.#scheduleIdleAck(stream);
  }

  #scheduleIdleAck(stream: StreamId): void {
    this.#cancelIdleAck(stream);
    this.#idleAcks.set(
      stream,
      setTimeout(() => {
        this.#idleAcks.delete(stream);
        const ack = this.#ledger.drain(stream);
        if (ack) {
          void this.#ack(ack.stream, ack.bytes);
        }
      }, IDLE_ACK_MS),
    );
  }

  #cancelIdleAck(stream: StreamId): void {
    const scheduled = this.#idleAcks.get(stream);
    if (scheduled !== undefined) {
      clearTimeout(scheduled);
      this.#idleAcks.delete(stream);
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
