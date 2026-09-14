import type { StreamId } from '../frames';
import {
  HIDDEN_BUFFER_CAP_BYTES,
  REPLAY_BOUNDARY_DEADLINE_MS,
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
 *
 * ## Why input is held shut until the replay has been parsed
 *
 * An attach begins with the daemon replaying the session's scrollback, and a replay is the
 * bytes the child once wrote — escape sequences intact, which means every `ESC[6n` and
 * `ESC[c` the child ever emitted. xterm answers a query when it parses one and has no way to
 * know it is reading history, so those answers arrive on `onData` looking exactly like
 * keystrokes, and forwarding them puts input into a shell nobody typed. On Windows ConPTY
 * reads the `…R` of a cursor-position report as F3, which `cmd` treats as
 * recall-previous-command: the pane came back after a relaunch showing a phantom command
 * line, and the user's next command was concatenated onto it.
 *
 * So the channel starts shut and opens on the `replay_end` frame — but **not when that frame
 * arrives**. xterm parses on its own timer to keep the main thread responsive, so the marker
 * reaches {@link XtermSurface.replayEnded} while the bytes it follows are still sitting in
 * xterm's write queue. Opening there would forward every answer to a replayed query and fix
 * nothing. What opens the gate is the write *callback* for the last chunk issued before the
 * marker — the same callback the byte accounting already rests on, and the only signal this
 * side gets that says "parsed" rather than "queued". A pane with no scrollback has no such
 * chunk and opens immediately, which is correct: there is nothing to have answered.
 *
 * The two things this deliberately does not do are the two workarounds that were considered
 * and rejected. It does not time its way past the replay — that drops real keystrokes on a
 * slow machine and still lets a query through on a fast one — and it does not strip query
 * sequences from what it writes, which would break every full-screen program that
 * legitimately asks. A live query, after the boundary, is answered normally.
 *
 * **Keystrokes during the window are dropped, not queued.** The pane is not interactive yet;
 * it is painting history. A keystroke held and delivered later lands at a prompt that has
 * moved on, which is the same class of fault as the phantom line. The count is kept so the
 * claim is checkable rather than merely asserted.
 */
export class XtermSurface implements TerminalSurface {
  readonly stream: StreamId;

  readonly #createTerminal: TerminalFactory;
  readonly #pool: WebglPool;
  readonly #onRendered: RenderedCallback;
  readonly #onReplayTimeout: (stream: StreamId, droppedChars: number) => void;

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

  /** Whether `onData` is forwarded. Shut until the replay has been parsed. */
  #inputOpen = false;
  /** Writes handed to the renderer, and writes it has reported parsing. */
  #writesIssued = 0;
  #writesParsed = 0;
  /**
   * The value {@link XtermSurface.#writesParsed} has to reach for the gate to open.
   *
   * `null` while no boundary has been seen. Set to the issue count at the moment the marker
   * arrived, which is what makes "parsed everything before the marker" a number rather than
   * a guess.
   */
  #openInputAfter: number | null = null;
  /**
   * Where the boundary fell inside the hidden buffer, if it arrived while hidden.
   *
   * A hidden pane has no renderer, so nothing parses and nothing answers — but the buffer is
   * drained into a fresh terminal on the next `show`, and *that* parse answers the replayed
   * queries. The index splits the drain: everything before it is replay, and the gate opens
   * between the two halves rather than before or after both.
   */
  #boundaryInHidden: number | null = null;
  /** Characters of input dropped because the replay had not finished. */
  #inputDropped = 0;
  /**
   * The backstop that opens the gate if the boundary never arrives.
   *
   * Named for the timer rather than for the moment it fires, because a private field spelled
   * `deadline` begins with four hex digits after its `#` and the theme guard reads that as a
   * hardcoded colour. A rename is cheaper than an exception in a guard whose value is that it
   * has none.
   */
  #boundaryTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(options: {
    readonly stream: StreamId;
    readonly createTerminal: TerminalFactory;
    readonly pool: WebglPool;
    readonly onRendered: RenderedCallback;
    /**
     * Called when the replay boundary never arrived and the deadline opened the gate.
     *
     * Optional so a test that is not about the deadline need not supply one; the surface
     * still opens, because a pane that refused input for ever would be worse than the defect
     * the gate closes.
     */
    readonly onReplayTimeout?: (stream: StreamId, droppedChars: number) => void;
  }) {
    this.stream = options.stream;
    this.#createTerminal = options.createTerminal;
    this.#pool = options.pool;
    this.#onRendered = options.onRendered;
    this.#onReplayTimeout = options.onReplayTimeout ?? (() => {});
    // Armed at construction rather than at the first write. A surface only comes into
    // existence because a stream was attached, and an attach that produced no frames at all
    // is exactly the case where nothing would otherwise ever open the gate.
    this.#boundaryTimer = setTimeout(() => {
      this.#boundaryTimer = null;
      if (this.#inputOpen) {
        return;
      }
      this.#openInput();
      this.#onReplayTimeout(this.stream, this.#inputDropped);
    }, REPLAY_BOUNDARY_DEADLINE_MS);
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

  /** Bytes held for a hidden pane that were thrown away, awaiting a report. */
  #droppedBytes = 0;

  /**
   * Take the record of output discarded while this pane was hidden, in bytes.
   *
   * **One shot.** Reading it clears it, which is what stops one overflow being reported
   * forever — and the reset that goes with it being re-injected on every later `show`,
   * wiping a pane that had nothing wrong with it.
   *
   * The caller reports the loss; the surface cannot. What was dropped is gone from this
   * process, and the authoritative screen is in Rust (D-7) — so all this can honestly say
   * is how much went, and to whom it mattered.
   */
  takeDroppedWhileHidden(): number {
    const dropped = this.#droppedBytes;
    this.#droppedBytes = 0;
    return dropped;
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
    this.#writeParsed(terminal, bytes.slice(), () => {
      // The callback, never the return of `write`. xterm parses on a timer to keep the main
      // thread responsive, so a `write` that has returned has queued bytes rather than
      // painted them; acking there would return credit for work not done and grow a queue
      // inside xterm that nothing bounds.
      this.#onRendered(this.stream, count);
    });
  }

  /**
   * Write to the renderer, counting the chunk in and out again.
   *
   * Every write that reaches a terminal goes through here, because the input gate opens on a
   * *count* of parsed chunks and a write that skipped the tally would let the gate open one
   * chunk early — with the replayed queries in that chunk still unanswered and about to be.
   */
  #writeParsed(terminal: XtermLike, data: Uint8Array | string, done?: () => void): void {
    this.#writesIssued += 1;
    terminal.write(data, () => {
      this.#writesParsed += 1;
      done?.();
      this.#openInputIfReplayParsed();
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
      // **The one line the defect turns on.** While the gate is shut this is not a keystroke
      // — it is xterm answering a `ESC[6n` or `ESC[c` it found in the replayed scrollback,
      // and forwarding it types into the child on the user's behalf. Real keystrokes in the
      // same window go the same way, which is the accepted cost: the pane is painting history
      // and is not interactive yet.
      if (!this.#inputOpen) {
        this.#inputDropped += data.length;
        return;
      }
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

    // The write queue died with the terminal, so chunks issued and not yet parsed never will
    // be — and a gate waiting on their callbacks would stay shut until its deadline. The
    // bytes are gone with the parser that would have answered them, so there is nothing left
    // to hold the gate for: reset the tally, and let a boundary already seen take effect.
    this.#writesIssued = 0;
    this.#writesParsed = 0;
    if (this.#openInputAfter !== null) {
      this.#openInput();
    }
  }

  resize(cols: number, rows: number): void {
    this.#cols = cols;
    this.#rows = rows;
    this.#terminal?.resize(cols, rows);
  }

  fit(): { readonly cols: number; readonly rows: number } | null {
    const measured = this.#terminal?.fit();
    if (!measured) {
      return null;
    }
    // Remembered, so a pane resized while visible and then hidden reopens at the size the
    // daemon already knows about rather than snapping back to the default.
    this.#cols = measured.cols;
    this.#rows = measured.rows;
    return measured;
  }

  focus(): void {
    this.#terminal?.focus();
  }

  /** Characters of input dropped because the replay had not finished parsing. */
  get inputDroppedWhileReplaying(): number {
    return this.#inputDropped;
  }

  /** Whether this surface is currently forwarding what the terminal reports. */
  get acceptsInput(): boolean {
    return this.#inputOpen;
  }

  replayEnded(): void {
    if (this.#inputOpen || this.#disposed) {
      return;
    }
    // **Stood down here, on arrival, not when the gate opens.** The deadline asks one
    // question — did the daemon mark the end of its replay? — and the marker has just
    // answered it. The gate opens later, and how much later is not the daemon's business:
    // a hidden pane's gate cannot open until it is shown, which may be minutes away or
    // never, and a large replay under a tight window can take longer to parse than the
    // deadline allows. Anchored to the gate instead, every background tab accused the daemon
    // of a protocol violation five seconds after connecting, and a slow parse had its gate
    // forced open part-way through the replay — which is the defect again, arriving later
    // and harder to see.
    this.#clearDeadline();
    if (this.#terminal === null) {
      // Nothing is parsing, so nothing can be answering — but the buffer collected so far is
      // replay, and the next `show` feeds it to a fresh terminal that *will* answer it.
      // Remember where the seam falls so the drain can open the gate in the middle.
      this.#boundaryInHidden = this.#hidden.length;
      return;
    }
    // Not `#openInput()`. The marker says the daemon has stopped replaying; it says nothing
    // about what xterm has parsed, and the chunks before it are still in xterm's queue.
    this.#openInputAfter = this.#writesIssued;
    this.#openInputIfReplayParsed();
  }

  /** Open the gate once every chunk issued before the boundary has been parsed. */
  #openInputIfReplayParsed(): void {
    if (this.#openInputAfter === null || this.#writesParsed < this.#openInputAfter) {
      return;
    }
    this.#openInput();
  }

  /** Start forwarding input, and stand the deadline down. */
  #openInput(): void {
    this.#inputOpen = true;
    this.#openInputAfter = null;
    this.#boundaryInHidden = null;
    this.#clearDeadline();
  }

  #clearDeadline(): void {
    if (this.#boundaryTimer !== null) {
      clearTimeout(this.#boundaryTimer);
      this.#boundaryTimer = null;
    }
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
    // A surface outlives nothing, but its deadline would: a timer left armed on a disposed
    // pane fires after a reconnect has already replaced it and puts a notice about a stream
    // that no longer exists in front of the user.
    this.#clearDeadline();
  }

  /** Collect output for a pane with no renderer, dropping everything on overflow. */
  #bufferWhileHidden(bytes: Uint8Array): void {
    if (this.#hiddenBytes + bytes.length > HIDDEN_BUFFER_CAP_BYTES) {
      // The whole buffer, not a trim. See the class docs: a cut at an arbitrary offset ends
      // mid-sequence and xterm's parser never recovers from one.
      this.#droppedBytes += this.#hiddenBytes + bytes.length;
      this.#hidden = [];
      this.#hiddenBytes = 0;
      this.#resetPending = true;
      // The replayed bytes went with the buffer, so there is nothing left that could answer a
      // replayed query and the seam is now at the front of an empty buffer. Left pointing at
      // the old length it would index past the end and the gate would never open here.
      if (this.#boundaryInHidden !== null) {
        this.#boundaryInHidden = 0;
      }
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
      this.#writeParsed(terminal, RESET_SEQUENCE);
    }
    // The seam, if the boundary arrived while this pane was hidden. Everything before it is
    // replay and must be parsed with the gate shut; everything after it is live and its
    // queries deserve their answers. Marking the gate *between* the two writes is what makes
    // both true — opening before the drain would forward the replay's answers, and opening
    // after it would swallow a live program's.
    const seam = this.#boundaryInHidden;
    this.#boundaryInHidden = null;
    this.#hidden.forEach((chunk, index) => {
      if (seam !== null && index === seam) {
        this.#openInputAfter = this.#writesIssued;
      }
      this.#writeParsed(terminal, chunk);
    });
    if (seam !== null && seam >= this.#hidden.length) {
      this.#openInputAfter = this.#writesIssued;
    }
    this.#hidden = [];
    this.#hiddenBytes = 0;
    if (seam !== null) {
      this.#openInputIfReplayParsed();
    }
    // Cleared here, with the reset it asked for now written. Leaving it set was a real
    // defect: every later `show` re-injected `ESC c` and wiped a pane whose buffer had
    // drained perfectly well, so one overflow early in a session blanked that terminal on
    // every tab switch for the rest of the process. What the *caller* still needs to know
    // — that output was lost — is `takeDroppedWhileHidden`, which survives this and is
    // cleared by being read.
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
  /**
   * Resize to fill the host, and report the size in cells.
   *
   * `undefined` when the terminal cannot measure itself — a host with no layout yet, which
   * is the state during the first frame after mounting.
   */
  fit(): { readonly cols: number; readonly rows: number } | undefined;
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
