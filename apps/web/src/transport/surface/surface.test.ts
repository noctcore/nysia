import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { StreamId } from '../frames';
import {
  HIDDEN_BUFFER_CAP_BYTES,
  RESET_SEQUENCE,
  type TerminalSurface,
} from './TerminalSurface';
import { XtermSurface, type TerminalFactory, type XtermLike } from './XtermSurface';
import { policyFor, WebglPool, type RendererKind } from './webglPool';

/**
 * A terminal that records instead of drawing.
 *
 * The render callback is held rather than invoked, because *when* it fires is the thing
 * under test: xterm parses on a timer, so a surface that acked on `write` returning would
 * pass every test that called the callback synchronously and would still be wrong.
 */
class StubTerminal implements XtermLike {
  readonly writes: (Uint8Array | string)[] = [];
  readonly pending: (() => void)[] = [];
  element: { parentElement: HTMLElement | null } | undefined;
  cols: number;
  rows: number;
  disposed = false;
  focused = 0;
  #data: ((data: string) => void)[] = [];

  constructor(
    readonly renderer: RendererKind,
    cols: number,
    rows: number,
  ) {
    this.cols = cols;
    this.rows = rows;
  }

  open(host: HTMLElement): void {
    this.element = { parentElement: host };
  }

  write(data: Uint8Array | string, done?: () => void): void {
    this.writes.push(data);
    if (done) {
      this.pending.push(done);
    }
  }

  /** Let xterm's parser catch up. */
  flush(): void {
    const callbacks = this.pending.splice(0, this.pending.length);
    for (const done of callbacks) {
      done();
    }
  }

  resize(cols: number, rows: number): void {
    this.cols = cols;
    this.rows = rows;
  }

  focus(): void {
    this.focused += 1;
  }

  dispose(): void {
    this.disposed = true;
  }

  onData(handler: (data: string) => void): { dispose(): void } {
    this.#data.push(handler);
    return {
      dispose: () => {
        this.#data = this.#data.filter((candidate) => candidate !== handler);
      },
    };
  }

  /** Pretend the user typed. */
  type(data: string): void {
    for (const handler of [...this.#data]) {
      handler(data);
    }
  }
}

/** A stand-in for the pane element. Never touched, so it needs no behaviour. */
const host = (): HTMLElement => ({}) as HTMLElement;

interface Harness {
  readonly surface: XtermSurface;
  readonly pool: WebglPool;
  readonly rendered: StreamId[];
  readonly bytes: number[];
  readonly built: StubTerminal[];
  latest(): StubTerminal;
}

function harness(
  stream: StreamId = 1,
  platform = 'windows',
  pool = new WebglPool(policyFor(platform)),
): Harness {
  const built: StubTerminal[] = [];
  const rendered: StreamId[] = [];
  const bytes: number[] = [];

  const createTerminal: TerminalFactory = ({ cols, rows, renderer }) => {
    const terminal = new StubTerminal(renderer, cols, rows);
    built.push(terminal);
    return terminal;
  };

  const surface = new XtermSurface({
    stream,
    createTerminal,
    pool,
    onRendered: (which, count) => {
      rendered.push(which);
      bytes.push(count);
    },
  });

  return {
    surface,
    pool,
    rendered,
    bytes,
    built,
    latest: () => {
      const last = built.at(-1);
      if (!last) {
        throw new Error('no terminal has been built');
      }
      return last;
    },
  };
}

const text = new TextEncoder();

describe('the surface adapter contract', () => {
  let fixture: Harness;

  beforeEach(() => {
    fixture = harness();
  });

  it('has no renderer until a pane becomes visible', () => {
    // D-7: hidden sessions cost nothing in JavaScript, because Rust holds the state.
    expect(fixture.surface.visible).toBe(false);
    expect(fixture.surface.renderer).toBeNull();
    expect(fixture.built).toHaveLength(0);
  });

  it('builds a renderer on show and releases it on hide', () => {
    fixture.surface.show(host());
    expect(fixture.surface.visible).toBe(true);
    expect(fixture.built).toHaveLength(1);

    fixture.surface.hide();
    expect(fixture.surface.visible).toBe(false);
    expect(fixture.latest().disposed).toBe(true);
  });

  it('showing an already-visible pane does not build a second terminal', () => {
    fixture.surface.show(host());
    fixture.surface.show(host());
    expect(fixture.built).toHaveLength(1);
  });

  it('carries a resize that arrived while hidden into the next terminal', () => {
    // A pane resized in a background tab and then revealed must not open at 80x24 and
    // reflow a screen the daemon already sized correctly.
    fixture.surface.resize(120, 40);
    fixture.surface.show(host());
    expect(fixture.latest().cols).toBe(120);
    expect(fixture.latest().rows).toBe(40);
  });

  it('forwards what the user typed, and stops after unsubscribing', () => {
    const typed: string[] = [];
    const stop = fixture.surface.onInput((data) => typed.push(data));
    fixture.surface.show(host());

    fixture.latest().type('ls\r');
    expect(typed).toEqual(['ls\r']);

    stop();
    fixture.latest().type('pwd\r');
    expect(typed).toEqual(['ls\r']);
  });

  it('is inert after dispose', () => {
    fixture.surface.show(host());
    fixture.surface.dispose();

    fixture.surface.write(text.encode('ignored'));
    fixture.surface.show(host());
    expect(fixture.built).toHaveLength(1);
    expect(fixture.surface.visible).toBe(false);
  });

  it('satisfies the interface it is typed against', () => {
    // The seam §7.3 names: a canvas-2D implementation must be swappable without touching a
    // caller, which only holds while callers depend on the interface and nothing else.
    const surface: TerminalSurface = fixture.surface;
    expect(typeof surface.write).toBe('function');
    expect(typeof surface.show).toBe('function');
    expect(typeof surface.hide).toBe('function');
  });
});

describe('acknowledging what has been rendered', () => {
  it('acks in the render callback, not when write returns', () => {
    // The bug this exists to prevent: xterm parses on a timer, so acking on `write`
    // returning returns credit for work not done. The daemon then sends more, and the queue
    // inside xterm grows without bound — the unbounded buffer the credit window exists to
    // prevent, moved one layer in where nothing measures it.
    const fixture = harness();
    fixture.surface.show(host());

    fixture.surface.write(text.encode('hello'));
    expect(fixture.bytes, 'nothing may be acked before the callback fires').toEqual([]);

    fixture.latest().flush();
    expect(fixture.bytes).toEqual([5]);
    expect(fixture.rendered).toEqual([1]);
  });

  it('acks each write exactly once, in order', () => {
    const fixture = harness();
    fixture.surface.show(host());
    fixture.surface.write(text.encode('one'));
    fixture.surface.write(text.encode('three'));

    fixture.latest().flush();
    expect(fixture.bytes).toEqual([3, 5]);
  });

  it('copies the payload, because the channel reuses its delivery buffer', () => {
    // xterm's write is asynchronous, so a retained view would be overwritten before it was
    // parsed and the terminal would paint whatever landed there instead.
    const fixture = harness();
    fixture.surface.show(host());

    const delivery = text.encode('live');
    fixture.surface.write(delivery);
    delivery.fill(0);

    const written = fixture.latest().writes[0];
    expect(written).toBeInstanceOf(Uint8Array);
    expect(new TextDecoder().decode(written as Uint8Array)).toBe('live');
  });

  it('ignores an empty write rather than acking nothing', () => {
    const fixture = harness();
    fixture.surface.show(host());
    fixture.surface.write(new Uint8Array(0));
    expect(fixture.latest().writes).toHaveLength(0);
    expect(fixture.bytes).toEqual([]);
  });
});

describe('a pane that is not visible', () => {
  it('still acknowledges every byte, immediately', () => {
    // The rule that is easy to get backwards. A hidden pane renders nothing, so a surface
    // that only acked what it painted would stop returning credit the moment a tab lost
    // focus — the daemon would stop reading that PTY and the shell behind the background
    // tab would silently block.
    const fixture = harness();
    fixture.surface.write(text.encode('background output'));

    expect(fixture.bytes).toEqual([17]);
    expect(fixture.built, 'no renderer may be built for a hidden pane').toHaveLength(0);
  });

  it('replays what it collected when the pane is revealed', () => {
    const fixture = harness();
    fixture.surface.write(text.encode('while away'));
    expect(fixture.surface.bufferedBytes).toBe(10);

    fixture.surface.show(host());
    expect(fixture.latest().writes).toHaveLength(1);
    expect(fixture.surface.bufferedBytes).toBe(0);
    expect(fixture.surface.droppedWhileHidden).toBe(false);
  });

  it('drops the whole buffer on overflow and never a part of it', () => {
    // Trimming to fit would cut mid-escape-sequence, and xterm's parser does not recover:
    // it reads whatever follows as parameters to a sequence that never ended and the pane
    // paints garbage from then on. `ESC c` is the only cut safe at any offset.
    const fixture = harness();
    fixture.surface.write(text.encode('[31m'));
    fixture.surface.write(new Uint8Array(HIDDEN_BUFFER_CAP_BYTES).fill(0x78));

    expect(fixture.surface.bufferedBytes).toBe(0);
    expect(fixture.surface.droppedWhileHidden).toBe(true);

    fixture.surface.show(host());
    expect(fixture.latest().writes).toEqual([RESET_SEQUENCE]);
  });

  it('keeps acknowledging the bytes it dropped', () => {
    // Dropping is how memory stays bounded; withholding credit is not. Doing both would
    // block the shell as well as losing its output.
    const fixture = harness();
    fixture.surface.write(new Uint8Array(HIDDEN_BUFFER_CAP_BYTES + 1));
    expect(fixture.bytes).toEqual([HIDDEN_BUFFER_CAP_BYTES + 1]);
  });

  it('holds the drop flag until the caller has repainted', () => {
    // Only the caller can repaint, because the authoritative state is in Rust (D-7). The
    // surface would have nothing to redraw from if it cleared the flag itself.
    const fixture = harness();
    fixture.surface.write(new Uint8Array(HIDDEN_BUFFER_CAP_BYTES + 1));
    fixture.surface.show(host());

    expect(fixture.surface.droppedWhileHidden).toBe(true);
    fixture.surface.acknowledgeDrop();
    expect(fixture.surface.droppedWhileHidden).toBe(false);
  });

  it('bounds memory however long the pane stays hidden', () => {
    const fixture = harness();
    for (let round = 0; round < 500; round += 1) {
      fixture.surface.write(new Uint8Array(8 * 1024).fill(0x79));
      expect(fixture.surface.bufferedBytes).toBeLessThanOrEqual(HIDDEN_BUFFER_CAP_BYTES);
    }
  });
});

describe('the renderer policy', () => {
  it('turns WebGL on by default where it is safe', () => {
    for (const platform of ['windows', 'linux']) {
      const policy = policyFor(platform);
      expect(policy.webglByDefault, platform).toBe(true);
      expect(policy.maxContexts, platform).toBe(Number.POSITIVE_INFINITY);
    }
  });

  it('makes macOS a pooled opt-in capped well under WebKit’s app-wide limit', () => {
    // xtermjs#5816 is open, and WebKit caps live contexts at 16 for the whole application —
    // not per tab. Six leaves ten for the rest of the process, so a full pool never becomes
    // someone else's bug.
    const policy = policyFor('macos');
    expect(policy.webglByDefault).toBe(false);
    expect(policy.maxContexts).toBe(6);
    expect(policy.maxContexts).toBeLessThan(16);
  });

  it('gives an unknown platform the cautious answer', () => {
    // Guessing wrong on a platform that caps contexts is much worse than the frame rate
    // caution costs.
    expect(policyFor('haiku')).toEqual({ webglByDefault: false, maxContexts: 0 });
  });
});

describe('the WebGL pool', () => {
  it('hands out contexts up to the cap and then falls back to DOM', () => {
    const pool = new WebglPool(policyFor('macos'));
    for (let stream = 0; stream < 6; stream += 1) {
      expect(pool.acquire(stream, true), `stream ${stream}`).toBe('webgl');
    }
    expect(pool.live).toBe(6);
    expect(pool.acquire(6, true), 'every holder is visible').toBe('dom');
  });

  it('evicts the least-recently-used hidden pane to make room', () => {
    const pool = new WebglPool(policyFor('macos'));
    for (let stream = 0; stream < 6; stream += 1) {
      pool.acquire(stream, true);
    }
    pool.hide(0);
    pool.hide(1);
    pool.touch(0);

    expect(pool.acquire(6, true)).toBe('webgl');
    expect(pool.holds(1), 'the least recently used hidden pane goes first').toBe(false);
    expect(pool.holds(0), 'the more recently used one is kept').toBe(true);
  });

  it('never evicts a pane the user is looking at', () => {
    // Taking a context from the visible pane to give it to a hidden one is the one trade
    // that is always wrong.
    const pool = new WebglPool(policyFor('macos'));
    for (let stream = 0; stream < 6; stream += 1) {
      pool.acquire(stream, true);
    }
    expect(pool.acquire(99, true)).toBe('dom');
    for (let stream = 0; stream < 6; stream += 1) {
      expect(pool.holds(stream), `stream ${stream}`).toBe(true);
    }
  });

  it('frees a closed pane’s context immediately', () => {
    const pool = new WebglPool(policyFor('macos'));
    for (let stream = 0; stream < 6; stream += 1) {
      pool.acquire(stream, true);
    }
    pool.release(3);
    expect(pool.live).toBe(5);
    expect(pool.acquire(7, true)).toBe('webgl');
  });

  it('demotes a pane permanently once its context is lost', () => {
    // §7.3: onContextLoss → dispose → DOM. A context taken once is usually taken again, so
    // retrying is a loop that renders nothing.
    const pool = new WebglPool(policyFor('windows'));
    expect(pool.acquire(1)).toBe('webgl');

    expect(pool.contextLost(1)).toBe('dom');
    expect(pool.isDemoted(1)).toBe(true);
    expect(pool.acquire(1)).toBe('dom');
    expect(pool.holds(1)).toBe(false);
  });

  it('honours an opt-out even where WebGL is the default', () => {
    const pool = new WebglPool(policyFor('windows'));
    expect(pool.acquire(1, false)).toBe('dom');
    expect(pool.live).toBe(0);
  });
});

describe('a surface whose context is lost', () => {
  it('disposes the WebGL terminal and rebuilds on DOM', () => {
    const pool = new WebglPool(policyFor('windows'));
    const built: StubTerminal[] = [];
    const losses: (() => void)[] = [];

    const surface = new XtermSurface({
      stream: 1,
      pool,
      onRendered: () => {},
      createTerminal: ({ cols, rows, renderer, onContextLoss }) => {
        const terminal = new StubTerminal(renderer, cols, rows);
        built.push(terminal);
        losses.push(onContextLoss);
        return terminal;
      },
    });

    surface.show(host());
    expect(surface.renderer).toBe('webgl');

    losses[0]?.();

    expect(built[0]?.disposed, 'the lost terminal must be disposed').toBe(true);
    expect(built).toHaveLength(2);
    expect(built[1]?.renderer).toBe('dom');
    expect(surface.renderer).toBe('dom');
    expect(surface.visible).toBe(true);
  });

  it('does not rebuild when there is nothing to rebuild onto', () => {
    // A loss arriving after the pane was detached has no host element. Rebuilding blind
    // would throw inside an event handler, where nothing would catch it.
    const pool = new WebglPool(policyFor('windows'));
    const losses: (() => void)[] = [];
    const surface = new XtermSurface({
      stream: 1,
      pool,
      onRendered: () => {},
      createTerminal: ({ cols, rows, renderer, onContextLoss }) => {
        losses.push(onContextLoss);
        const terminal = new StubTerminal(renderer, cols, rows);
        // A terminal that was never opened has no element.
        terminal.element = undefined;
        return terminal;
      },
    });

    surface.show(host());
    expect(() => losses[0]?.()).not.toThrow();
    expect(pool.isDemoted(1)).toBe(true);
  });

  it('leaves no stray input subscription behind after a rebuild', () => {
    // The failure this catches: one keystroke sent twice, because the disposed terminal's
    // handler was never unsubscribed.
    const pool = new WebglPool(policyFor('windows'));
    const built: StubTerminal[] = [];
    const losses: (() => void)[] = [];
    const typed = vi.fn();

    const surface = new XtermSurface({
      stream: 1,
      pool,
      onRendered: () => {},
      createTerminal: ({ cols, rows, renderer, onContextLoss }) => {
        const terminal = new StubTerminal(renderer, cols, rows);
        built.push(terminal);
        losses.push(onContextLoss);
        return terminal;
      },
    });

    surface.onInput(typed);
    surface.show(host());
    losses[0]?.();

    built[1]?.type('x');
    expect(typed).toHaveBeenCalledTimes(1);

    built[0]?.type('x');
    expect(typed, 'the disposed terminal must not still be heard').toHaveBeenCalledTimes(1);
  });
});
