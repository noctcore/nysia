import type { StreamId } from '../frames';

/**
 * The seam between the app and whatever draws a terminal.
 *
 * §7.3 names the reason this exists by name: `ghostty-web` is canvas-2D with no WebGL at
 * all, and when it is worth swapping in, it must be swappable without touching a caller.
 * So nothing above this interface may know that xterm.js exists — not a type, not an
 * option name, not a lifecycle quirk. {@link XtermSurface} is one implementation of it and
 * is imported in exactly one place.
 *
 * ## The two rules the interface encodes
 *
 * **Only visible panes get a renderer** (D-7). A hidden session costs nothing in
 * JavaScript because Rust holds the authoritative terminal state; the webview is a display
 * cache. {@link TerminalSurface.show} is therefore not "make this element visible", it is
 * "this pane now needs a renderer", and {@link TerminalSurface.hide} gives one back.
 *
 * **A hidden pane still acknowledges.** This is the rule that is easy to get backwards.
 * Credit is returned when bytes are *rendered*, and a hidden pane renders nothing — so a
 * surface that only acked what it painted would stop returning credit the moment its tab
 * lost focus, the daemon would stop reading that PTY, and the shell behind a background tab
 * would silently block. A hidden surface accounts for every byte it is given, whether it
 * buffers it or drops it. Bounded memory comes from the transient buffer's cap, not from
 * withholding credit.
 */
export interface TerminalSurface {
  /** Which multiplexed stream this surface draws. */
  readonly stream: StreamId;

  /** Whether a renderer is currently attached. */
  readonly visible: boolean;

  /**
   * Whether what the terminal reports is forwarded yet.
   *
   * `false` from construction until the replay has been parsed — see
   * {@link TerminalSurface.replayEnded}. On the interface rather than left private because it
   * is the one piece of gate state a caller can honestly need: a pane that is shut is a pane
   * that is dropping what the user types, and a surface that hid that would make the cost
   * unmeasurable.
   */
  readonly acceptsInput: boolean;

  /**
   * Draw terminal output.
   *
   * The bytes are the payload of one or more `output` frames, escape sequences intact. The
   * surface reports them rendered through the callback it was constructed with — for a
   * visible pane once the renderer says so, for a hidden one as soon as they are accounted
   * for.
   *
   * The buffer may be a view the caller will reuse, so an implementation that retains it
   * must copy.
   */
  write(bytes: Uint8Array): void;

  /** Give this pane a renderer and attach it to `host`. */
  show(host: HTMLElement): void;

  /** Take the renderer away. The pane's state stays in Rust. */
  hide(): void;

  /** Tell the surface the pane's size changed, in cells. */
  resize(cols: number, rows: number): void;

  /**
   * Measure the host element and resize to fit it, returning the new size in cells.
   *
   * On the interface rather than left to the caller because only the surface knows its own
   * cell metrics — font, line height, padding — and a caller that computed them would be
   * reimplementing the renderer's layout from the outside and getting it wrong by a row on
   * every zoom level. `null` when there is no renderer to measure.
   */
  fit(): { readonly cols: number; readonly rows: number } | null;

  /** Move keyboard focus into the terminal. */
  focus(): void;

  /**
   * Be told what the user typed.
   *
   * Returns an unsubscribe function. The caller forwards the data to the daemon; a surface
   * never talks to a socket itself.
   *
   * **Nothing is reported until the replay is over.** See {@link TerminalSurface.replayEnded}:
   * a terminal answers the queries it parses, it cannot tell a replayed one from a live one,
   * and those answers arrive here indistinguishable from keystrokes. A surface holds this
   * channel shut until it knows the bytes being parsed are live.
   */
  onInput(handler: (data: string) => void): () => void;

  /**
   * The daemon's replay for this stream is over; everything after it is live.
   *
   * Called when the `replay_end` frame arrives. What the surface owes it is **not** to open
   * the input channel here, but to open it once the renderer has finished *parsing* the
   * bytes that came before — those are different moments. A terminal parses on its own
   * schedule, so the marker reaches this method while the replayed bytes are still queued,
   * and a surface that opened on arrival would still forward every answer to a replayed
   * query.
   */
  replayEnded(): void;

  /** Release everything. The surface is unusable afterwards. */
  dispose(): void;
}

/** How a surface reports that bytes have reached the screen. */
export type RenderedCallback = (stream: StreamId, bytes: number) => void;

/**
 * The most output a hidden pane accumulates before its buffer is thrown away.
 *
 * 256 KiB, matching §7.3's `pendingCap`, because they bound the same thing from opposite
 * ends: the daemon will not hold more than that unacknowledged, so a hidden pane that holds
 * the same amount can never be the larger of the two.
 */
export const HIDDEN_BUFFER_CAP_BYTES = 256 * 1024;

/**
 * A full reset: `ESC c`.
 *
 * What a hidden pane writes after its transient buffer overflowed, and the reason the whole
 * buffer is dropped rather than trimmed to fit. Cutting a buffer at an arbitrary offset
 * leaves a partial escape sequence at the seam, and xterm's parser does not recover from
 * one — it consumes whatever follows as parameters to a sequence that never ended, and the
 * pane paints garbage from then on. Dropping everything and resetting the parser is the
 * only cut that is safe at any offset.
 */
export const RESET_SEQUENCE = 'c';

/**
 * How long a surface holds its input channel shut waiting for a replay boundary.
 *
 * A backstop, never the mechanism. The boundary is a frame the daemon sends exactly once per
 * attach, and a client that timed its way past a replay instead of waiting for the marker
 * would be the race the marker exists to remove — it would drop real keystrokes on a slow
 * machine and still let a replayed query answer on a fast one.
 *
 * It measures the wait for the marker to **arrive**, and nothing else. Not the wait for the
 * gate to open: a hidden pane's gate cannot open until it is shown, and a large replay can
 * take longer to parse than this allows, so a deadline anchored to the gate accuses the daemon
 * of a fault whenever a tab sits in the background — and forces a slow pane open part-way
 * through its own replay.
 *
 * This is what happens when the marker does **not** arrive, which a v2 daemon only does by
 * violating its own protocol or by losing the frame to a full outbox. Long enough that a
 * large replay under a tight credit window is never mistaken for one, short enough that a
 * user who has hit it is not still waiting when they give up. Hitting it raises a notice
 * rather than passing in silence: a pane that quietly ignored the first few seconds of
 * typing is indistinguishable from a broken keyboard.
 */
export const REPLAY_BOUNDARY_DEADLINE_MS = 5_000;
