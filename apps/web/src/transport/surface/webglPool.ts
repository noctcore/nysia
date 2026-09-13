import type { StreamId } from '../frames';

/**
 * Which panes get a WebGL renderer, and on which platforms (§7.3).
 *
 * xterm 6 removed the canvas renderer, so the choice is WebGL or DOM with nothing between.
 * That makes the choice consequential: DOM is correct everywhere and slow on a busy
 * session, WebGL is fast and, on one platform, actively dangerous.
 *
 * - **Windows (WebView2) and Linux**: WebGL on by default.
 * - **macOS**: a pooled opt-in. xtermjs#5816 — atlas corruption in WebKit — is still open,
 *   and WebKit hard-caps live WebGL contexts at **16 app-wide**. Sixteen is not per tab or
 *   per surface; it is the whole application, and Nysia is a terminal multiplexer whose
 *   entire premise is many sessions at once. Handing every pane a context would exhaust the
 *   cap and then break unrelated parts of the window that also want one.
 *
 * So on macOS the pool holds at most six contexts and evicts hidden panes least-recently-
 * used. Six rather than sixteen because the cap is app-wide: leaving ten for the rest of
 * the process is the margin that keeps a full pool from being someone else's bug.
 *
 * This module is pure policy and holds no WebGL object of its own, which is what makes it
 * testable under node-only vitest (D-18). What it decides is applied by
 * {@link import('./XtermSurface').XtermSurface}.
 */

/** What a surface draws with. */
export type RendererKind = 'webgl' | 'dom';

/** The platform's renderer rules. */
export interface RendererPolicy {
  /** Whether a visible pane asks for WebGL without the user opting in. */
  readonly webglByDefault: boolean;
  /** How many live WebGL contexts may exist at once. `Infinity` where there is no cap. */
  readonly maxContexts: number;
}

/**
 * The policy for `platform`, which is `std::env::consts::OS` as the `host_platform` command
 * reports it.
 *
 * Asked of the process rather than parsed out of a user-agent string, deliberately: both
 * WebView2 and WKWebView have changed their strings between releases, and a renderer that
 * guesses wrong either forfeits WebGL on Windows for nothing or exhausts WebKit's app-wide
 * cap on macOS. An unknown platform gets the cautious answer, because the failure it avoids
 * is much worse than the frame rate it costs.
 */
export function policyFor(platform: string): RendererPolicy {
  switch (platform) {
    case 'windows':
    case 'linux':
      return { webglByDefault: true, maxContexts: Number.POSITIVE_INFINITY };
    case 'macos':
      return { webglByDefault: false, maxContexts: 6 };
    default:
      return { webglByDefault: false, maxContexts: 0 };
  }
}

/**
 * Hands out WebGL contexts under the platform's cap.
 *
 * Visible panes are never evicted — evicting the pane the user is looking at to give a
 * context to one they are not is the one trade that is always wrong. When every holder is
 * visible and the pool is full, the newcomer draws with DOM: correct, slower, and
 * unremarkable next to a context loss.
 */
export class WebglPool {
  readonly #policy: RendererPolicy;
  /** Stream ids holding a context, least-recently-used first. */
  #holders: StreamId[] = [];
  /** Streams currently visible, which are never evicted. */
  readonly #visible = new Set<StreamId>();
  /**
   * Streams that have lost a context once.
   *
   * A lost context means the driver or the compositor took it away, and asking for another
   * usually loses that one too. §7.3's rule is `onContextLoss → dispose → DOM`, so the
   * demotion is permanent for the life of the pane rather than something to retry into a
   * loop.
   */
  readonly #demoted = new Set<StreamId>();

  constructor(policy: RendererPolicy) {
    this.#policy = policy;
  }

  /** The policy in force. */
  get policy(): RendererPolicy {
    return this.#policy;
  }

  /** How many contexts are out. */
  get live(): number {
    return this.#holders.length;
  }

  /** Whether this stream holds a context. */
  holds(stream: StreamId): boolean {
    return this.#holders.includes(stream);
  }

  /**
   * Decide what a pane becoming visible should draw with.
   *
   * `preferWebgl` is the caller's opt-in, which on Windows and Linux defaults to true and on
   * macOS does not. Evicting to make room only ever takes a context from a hidden pane.
   */
  acquire(stream: StreamId, preferWebgl = this.#policy.webglByDefault): RendererKind {
    this.#visible.add(stream);

    if (this.#demoted.has(stream) || !preferWebgl || this.#policy.maxContexts <= 0) {
      return 'dom';
    }
    if (this.holds(stream)) {
      this.touch(stream);
      return 'webgl';
    }

    if (this.#holders.length >= this.#policy.maxContexts && !this.#evictOne()) {
      // Every holder is visible. Taking a context from a pane the user is looking at to
      // give it to another is never the right trade.
      return 'dom';
    }

    this.#holders.push(stream);
    return 'webgl';
  }

  /** Mark a stream as most-recently-used, so it is evicted last. */
  touch(stream: StreamId): void {
    const at = this.#holders.indexOf(stream);
    if (at >= 0) {
      this.#holders.splice(at, 1);
      this.#holders.push(stream);
    }
  }

  /**
   * A pane is no longer visible.
   *
   * It keeps its context for now — a tab switched away from and back is the commonest
   * thing a user does, and rebuilding an atlas each time would make that stutter. It simply
   * becomes evictable, which is what the pool is for.
   */
  hide(stream: StreamId): void {
    this.#visible.delete(stream);
  }

  /** A pane has gone. Its context is available immediately. */
  release(stream: StreamId): void {
    this.#visible.delete(stream);
    this.#demoted.delete(stream);
    const at = this.#holders.indexOf(stream);
    if (at >= 0) {
      this.#holders.splice(at, 1);
    }
  }

  /**
   * This stream's context was lost.
   *
   * §7.3: dispose and fall back to DOM. The demotion sticks for the life of the pane,
   * because a context that was taken once is usually taken again and retrying is a loop
   * that renders nothing.
   */
  contextLost(stream: StreamId): RendererKind {
    this.#demoted.add(stream);
    const at = this.#holders.indexOf(stream);
    if (at >= 0) {
      this.#holders.splice(at, 1);
    }
    return 'dom';
  }

  /** Whether this stream has been permanently demoted to DOM. */
  isDemoted(stream: StreamId): boolean {
    return this.#demoted.has(stream);
  }

  /** Drop the least-recently-used hidden holder. `false` when every holder is visible. */
  #evictOne(): boolean {
    const victim = this.#holders.find((held) => !this.#visible.has(held));
    if (victim === undefined) {
      return false;
    }
    this.#holders.splice(this.#holders.indexOf(victim), 1);
    return true;
  }
}
