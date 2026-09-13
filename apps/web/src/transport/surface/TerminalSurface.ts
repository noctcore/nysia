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
   */
  onInput(handler: (data: string) => void): () => void;

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
