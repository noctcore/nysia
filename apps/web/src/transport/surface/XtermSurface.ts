import type { StreamId } from '../frames';
import {
  HIDDEN_BUFFER_CAP_BYTES,
  RESET_SEQUENCE,
  type RenderedCallback,
  type TerminalSurface,
} from './TerminalSurface';
import type { RendererKind, WebglPool } from './webglPool';

/**
 * The xterm.js implementation of {@link TerminalSurface}.
 *
 * ## Why the terminal is injected
 *
 * `@xterm/xterm` needs a DOM to construct and a canvas to draw on, and v0.1's tests are
 * node-only (D-18). Taking the terminal from a factory rather than importing it means the
 * logic that is actually easy to get wrong — acking on the render callback rather than on
 * `write` returning, dropping a hidden pane's whole buffer, demoting on context loss — is
 * exercised against a stub in the same tests that run in CI, instead of being the one part
 * nothing covers. `./xterm.ts` is where the real terminal is built, and it is the only
 * module in the repository that imports `@xterm/*`.
 *
 * ## What happens while a pane is hidden
 *
 * No renderer, per D-7: Rust holds the authoritative state and the webview is a display
 * cache, so a hidden pane costs a byte buffer and nothing else. Output accumulates in that
 * buffer up to {@link HIDDEN_BUFFER_CAP_BYTES}, and on overflow the **whole** buffer is
 * dropped and a reset is queued — never a trim to fit. A buffer cut at an arbitrary offset
 * ends mid-escape-sequence, and xterm's parser does not recover: it reads whatever comes
 * next as parameters to a sequence that never ended, and the pane paints garbage
 * indefinitely. `ESC c` is the only cut that is safe at any offset.
 *
 * Bytes are acknowledged either way. A hidden pane that stopped acking would stop the
 * daemon reading its PTY, and the shell behind a background tab would block.
 */
export class XtermSurface implements TerminalSurface {
  readonly stream: StreamId;

  readonly #createTerminal: TerminalFactory;
  readonly #pool: WebglPool;
  readonly #onRendered: RenderedCallback;

  #terminal: XtermLike | null = null;
  #renderer: RendererKind | null = null;
  #disposables: (() => void)[] = [];
  readonly #inputHandlers = new Set<(data: string) => void>();

  /** Output collected while no renderer is attached. */
  #hidden: Uint8Array[] = [];
  #hiddenBytes = 0;
  /** Set when the transient buffer overflowed and the parser must be reset on reveal. */
  #resetPending = false;

  #cols = 80;
  #rows = 24;
  #disposed = false;

  constructor(options: {
    readonly stream: StreamId;
    readonly createTerminal: TerminalFactory;
    readonly pool: WebglPool;
    readonly onRendered: RenderedCallback;
  }) {
    this.stream = options.stream;
    this.#createTerminal = options.createTerminal;
    this.#pool = options.pool;
    this.#onRendered = options.onRendered;
  }

  get visible(): boolean {
    return this.#terminal !== null;
  }

  /** What this surface is drawing with, or `null` while hidden. */
  get renderer(): RendererKind | null {
    return this.#renderer;
  }

  /** Bytes held for a hidden pane. Zero whenever one is visible. */
  get bufferedBytes(): number {
    return this.#hiddenBytes;
  }

  /**
   * Whether the transient buffer overflowed and the pane will need repainting.
   *
   * The caller reads this after {@link show} to decide whether to ask the daemon for the
   * screen again. The surface cannot answer that itself — the authoritative state is in
   * Rust, which is the whole point of D-7.
   */
  get droppedWhileHidden(): boolean {
    return this.#resetPending;
  }

  write(bytes: Uint8Array): void {
    if (this.#disposed || bytes.length === 0) {
      return;
    }

    const terminal = this.#terminal;
    if (terminal === null) {
      this.#bufferWhileHidden(bytes);
      // Acknowledged now, not on reveal. Withholding credit for a hidden pane stops the
      // daemon reading its PTY and blocks the shell behind a background tab.
      this.#onRendered(this.stream, bytes.length);
      return;
    }

    const count = bytes.length;
    // The payload is a view into the delivery buffer, which the Channel may reuse the
    // moment this returns — and xterm's write is asynchronous. Copying is not optional.
    terminal.write(bytes.slice(), () => {
      // The callback, never the return of `write`. xterm parses on a timer to keep the main
      // thread responsive, so a `write` that has returned has queued bytes rather than
      // painted them; acking there would return credit for work not done and grow a queue
      // inside xterm that nothing bounds.
      this.#onRendered(this.stream, count);
    });
  }

  show(host: HTMLElement): void {
    if (this.#disposed) {
      return;
    }
    if (this.#terminal !== null) {
      this.#pool.touch(this.stream);
      return;
    }

    const renderer = this.#pool.acquire(this.stream);
    const terminal = this.#createTerminal({
      cols: this.#cols,
      rows: this.#rows,
      renderer,
      onContextLoss: () => this.#onContextLoss(),
    });
    this.#terminal = terminal;
    this.#renderer = renderer;

    terminal.open(host);
    const subscription = terminal.onData((data) => {
      for (const handler of this.#inputHandlers) {
        handler(data);
      }
    });
    this.#disposables.push(() => subscription.dispose());

    this.#drainHiddenBuffer(terminal);
  }

  hide(): void {
    this.#pool.hide(this.stream);

    const terminal = this.#terminal;
    if (terminal === null) {
      return;
    }
    this.#terminal = null;
    this.#renderer = null;
    this.#runDisposables();
    terminal.dispose();
  }

  resize(cols: number, rows: number): void {
    this.#cols = cols;
    this.#rows = rows;
    this.#terminal?.resize(cols, rows);
  }

  focus(): void {
    this.#terminal?.focus();
  }

  onInput(handler: (data: string) => void): () => void {
    this.#inputHandlers.add(handler);
    return () => {
      this.#inputHandlers.delete(handler);
    };
  }

  dispose(): void {
    if (this.#disposed) {
      return;
    }
    this.#disposed = true;
    this.hide();
    this.#pool.release(this.stream);
    this.#inputHandlers.clear();
    this.#hidden = [];
    this.#hiddenBytes = 0;
  }

  /** Collect output for a pane with no renderer, dropping everything on overflow. */
  #bufferWhileHidden(bytes: Uint8Array): void {
    if (this.#hiddenBytes + bytes.length > HIDDEN_BUFFER_CAP_BYTES) {
      // The whole buffer, not a trim. See the class docs: a cut at an arbitrary offset ends
      // mid-sequence and xterm's parser never recovers from one.
      this.#hidden = [];
      this.#hiddenBytes = 0;
      this.#resetPending = true;
      return;
    }
    this.#hidden.push(bytes.slice());
    this.#hiddenBytes += bytes.length;
  }

  /** Replay what was collected while hidden into a freshly opened terminal. */
  #drainHiddenBuffer(terminal: XtermLike): void {
    if (this.#resetPending) {
      // `ESC c` first, so the parser starts from a known state rather than wherever the
      // dropped bytes left it.
      terminal.write(RESET_SEQUENCE);
    }
    for (const chunk of this.#hidden) {
      terminal.write(chunk);
    }
    this.#hidden = [];
    this.#hiddenBytes = 0;
    // Deliberately *not* cleared: the caller reads `droppedWhileHidden` after `show` to
    // decide whether to ask the daemon to repaint, and clearing it here would lose that.
    // `acknowledgeDrop` is how it is cleared, once the repaint has been requested.
  }

  /** The caller has repainted after a drop; stop reporting one. */
  acknowledgeDrop(): void {
    this.#resetPending = false;
  }

  /** The renderer's context went away. */
  #onContextLoss(): void {
    // §7.3: dispose and fall back to DOM. The pool makes the demotion permanent for this
    // pane, because a context taken once is usually taken again and retrying is a loop that
    // renders nothing.
    this.#pool.contextLost(this.stream);

    const terminal = this.#terminal;
    const host = terminal?.element?.parentElement ?? null;
    if (terminal === null || host === null) {
      // Nothing to rebuild onto. The next `show` picks DOM, because the pool now says so.
      return;
    }

    this.hide();
    this.#pool.hide(this.stream);
    this.show(host);
  }

  #runDisposables(): void {
    for (const dispose of this.#disposables) {
      dispose();
    }
    this.#disposables = [];
  }
}

/** What {@link XtermSurface} needs a terminal to do. */
export interface XtermLike {
  /** The element the terminal was opened into, once it has been. */
  readonly element?: { readonly parentElement: HTMLElement | null } | undefined;
  open(host: HTMLElement): void;
  /** `done` fires when the bytes have been parsed and painted, not when `write` returns. */
  write(data: Uint8Array | string, done?: () => void): void;
  resize(cols: number, rows: number): void;
  focus(): void;
  dispose(): void;
  onData(handler: (data: string) => void): { dispose(): void };
}

/** How a terminal is built, so the real one is imported in exactly one module. */
export type TerminalFactory = (options: {
  readonly cols: number;
  readonly rows: number;
  readonly renderer: RendererKind;
  readonly onContextLoss: () => void;
}) => XtermLike;
