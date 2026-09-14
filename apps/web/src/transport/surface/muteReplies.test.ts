import { describe, expect, it } from 'vitest';

import { muteTerminalReplies, type ReplyParser, type SequenceId } from './muteReplies';

/**
 * A parser that dispatches the way xterm's does, with recording stand-ins for the built-ins.
 *
 * The three properties the mute rests on, all three read out of `@xterm/xterm`
 * 6.1.0-beta.304's source rather than its documentation — see `muteReplies.ts` for the
 * citations:
 *
 * 1. a registration is **pushed** onto the list for its identifier;
 * 2. dispatch walks that list **backwards**, so the last registration runs first;
 * 3. it stops at the first handler that returns `true`.
 *
 * Which makes this a model of xterm, not xterm — node-only tests cannot construct a
 * `Terminal` (D-18). What it pins is the half that is ours: that every sequence xterm can
 * answer is covered, and that each handler returns the value that stops the built-in. The
 * half that is xterm's is the ordering, and `scripts/e2e/walking-skeleton.ps1` is where a
 * real one is driven.
 */
class ModelParser implements ReplyParser {
  /** Everything a handler wrote back, in order. Empty is the whole point. */
  readonly emitted: string[] = [];
  /** What a built-in did that was not a reply. These must survive the mute. */
  readonly acted: string[] = [];

  readonly #csi = new Map<string, ((params: (number | number[])[]) => boolean)[]>();
  readonly #dcs = new Map<
    string,
    ((data: string, params: (number | number[])[]) => boolean)[]
  >();
  readonly #osc = new Map<number, ((data: string) => boolean)[]>();

  registerCsiHandler(
    id: SequenceId,
    handler: (params: (number | number[])[]) => boolean,
  ): { dispose(): void } {
    return push(this.#csi, key(id), handler);
  }

  registerDcsHandler(
    id: SequenceId,
    handler: (data: string, params: (number | number[])[]) => boolean,
  ): { dispose(): void } {
    return push(this.#dcs, key(id), handler);
  }

  registerOscHandler(ident: number, handler: (data: string) => boolean): { dispose(): void } {
    return push(this.#osc, ident, handler);
  }

  /** The identifiers something is registered under, sorted, for an exact-set assertion. */
  registeredCsi(): string[] {
    return [...this.#csi.keys()].sort();
  }

  registeredDcs(): string[] {
    return [...this.#dcs.keys()].sort();
  }

  registeredOsc(): number[] {
    return [...this.#osc.keys()].sort((one, other) => one - other);
  }

  csi(id: SequenceId, params: (number | number[])[] = [0]): void {
    dispatch(this.#csi.get(key(id)), (handler) => handler(params));
  }

  dcs(id: SequenceId, data: string, params: (number | number[])[] = [0]): void {
    dispatch(this.#dcs.get(key(id)), (handler) => handler(data, params));
  }

  osc(ident: number, data: string): void {
    dispatch(this.#osc.get(ident), (handler) => handler(data));
  }

  /**
   * Register a built-in that answers, the way `InputHandler` does in its constructor.
   *
   * Registered **before** {@link muteTerminalReplies} in every test here, because that is the
   * order the real terminal has: the mute goes on a terminal whose built-ins are already in
   * place, and a mute that only worked when it went first would be a fact about this file
   * rather than about xterm.
   */
  builtInCsi(id: SequenceId, reply: string): void {
    this.registerCsiHandler(id, () => {
      this.emitted.push(reply);
      return true;
    });
  }

  builtInDcs(id: SequenceId, reply: string): void {
    this.registerDcsHandler(id, () => {
      this.emitted.push(reply);
      return true;
    });
  }

  /** `windowOptions`: reports for 14/16/18, and manipulates the title stack for 22/23. */
  builtInWindowOptions(): void {
    this.registerCsiHandler(WINDOW_OPTIONS, (params) => {
      const option = params[0];
      if (option === 14 || option === 16 || option === 18) {
        this.emitted.push(`report ${String(option)}`);
      } else {
        this.acted.push(`window option ${String(option)}`);
      }
      return true;
    });
  }

  /**
   * `setOrReportIndexedColor` and `_setOrReportSpecialColor`: answer a `?`, apply anything
   * else.
   *
   * Modelled at the slot rather than over the whole payload, because the mute's own reading
   * is per-slot and a stand-in that searched the string would agree with it by accident.
   * `OSC 4` is `index;spec` pairs and a non-numeric index throws its pair away — xterm's
   * `/^\d+$/` test — while `OSC 10/11/12` are bare specs.
   */
  builtInColour(ident: number): void {
    this.registerOscHandler(ident, (data) => {
      const slots = data.split(';');
      if (ident !== INDEXED_COLOUR) {
        for (const spec of slots) {
          this.#colour(`${String(ident)}`, spec);
        }
        return true;
      }
      while (slots.length > 1) {
        const index = slots.shift();
        const spec = slots.shift();
        if (index !== undefined && spec !== undefined && /^\d+$/.test(index)) {
          this.#colour(`${String(ident)};${index}`, spec);
        }
      }
      return true;
    });
  }

  #colour(what: string, spec: string): void {
    if (spec === QUERY) {
      this.emitted.push(`colour ${what}`);
    } else {
      this.acted.push(`colour ${what} = ${spec}`);
    }
  }
}

function push<K, H>(into: Map<K, H[]>, at: K, handler: H): { dispose(): void } {
  const handlers = into.get(at) ?? [];
  handlers.push(handler);
  into.set(at, handlers);
  return {
    dispose: () => {
      into.set(
        at,
        handlers.filter((candidate) => candidate !== handler),
      );
    },
  };
}

/** Backwards, stopping at the first `true`. The ordering claim, in four lines. */
function dispatch<H>(handlers: H[] | undefined, call: (handler: H) => boolean): void {
  if (!handlers) {
    return;
  }
  for (let at = handlers.length - 1; at >= 0; at--) {
    const handler = handlers[at];
    if (handler !== undefined && call(handler)) {
      return;
    }
  }
}

function key(id: SequenceId): string {
  return `${id.prefix ?? ''}|${id.intermediates ?? ''}|${id.final}`;
}

const DA1: SequenceId = { final: 'c' };
const DA2: SequenceId = { prefix: '>', final: 'c' };
const DSR: SequenceId = { final: 'n' };
const DECDSR: SequenceId = { prefix: '?', final: 'n' };
const XTVERSION: SequenceId = { prefix: '>', final: 'q' };
const DECRQM: SequenceId = { intermediates: '$', final: 'p' };
const DECRQM_PRIVATE: SequenceId = { prefix: '?', intermediates: '$', final: 'p' };
const KITTY_QUERY: SequenceId = { prefix: '?', final: 'u' };
const DECRQSS: SequenceId = { intermediates: '$', final: 'q' };
const WINDOW_OPTIONS: SequenceId = { final: 't' };

/** The value that turns an OSC colour sequence into a question. */
const QUERY = '?';

/** `OSC 4` addresses the palette by index, so its payload is `index;spec` pairs. */
const INDEXED_COLOUR = 4;

/**
 * A parser with every responder xterm has, and no mute.
 *
 * Two of these answer here and would not answer in the app, deliberately. `kittyKeyboardQuery`
 * returns without writing unless `vtExtensions.kittyKeyboard` is set, and `windowOptions`'
 * reports are refused by `paramToWindowOption` against an option xterm defaults to `{}` —
 * `./xterm.ts` sets neither. What is under test is the mute's own answer to each sequence, and
 * a stand-in that copied xterm's option gates would test the gates instead, then pass the
 * moment somebody turned one on. A responder an option flip away from being live is a
 * responder (CLAUDE.md §6).
 */
function wired(): ModelParser {
  const parser = new ModelParser();
  parser.builtInCsi(DA1, '\u001b[?1;2c');
  parser.builtInCsi(DA2, '\u001b[>0;276;0c');
  parser.builtInCsi(DSR, '\u001b[3;1R');
  parser.builtInCsi(DECDSR, '\u001b[?3;1R');
  parser.builtInCsi(XTVERSION, '\u001bP>|xterm.js(6.1.0)\u001b\\');
  parser.builtInCsi(DECRQM, '\u001b[4;2$y');
  parser.builtInCsi(DECRQM_PRIVATE, '\u001b[?2004;2$y');
  parser.builtInCsi(KITTY_QUERY, '\u001b[?0u');
  parser.builtInDcs(DECRQSS, '\u001bP1$r0m\u001b\\');
  parser.builtInWindowOptions();
  for (const ident of [INDEXED_COLOUR, 10, 11, 12]) {
    parser.builtInColour(ident);
  }
  return parser;
}

/** The same parser with the mute on top of the built-ins. */
function muted(): ModelParser {
  const parser = wired();
  muteTerminalReplies(parser);
  return parser;
}

/** Ask every question xterm knows how to answer. Returns how many were asked. */
function askEverything(parser: ModelParser): number {
  parser.csi(DA1);
  parser.csi(DA2);
  parser.csi(DSR, [6]);
  parser.csi(DSR, [5]);
  parser.csi(DECDSR, [6]);
  parser.csi(DECDSR, [996]);
  parser.csi(XTVERSION);
  parser.csi(DECRQM, [4]);
  parser.csi(DECRQM_PRIVATE, [2004]);
  parser.csi(KITTY_QUERY);
  parser.dcs(DECRQSS, 'm');
  parser.csi(WINDOW_OPTIONS, [14]);
  parser.csi(WINDOW_OPTIONS, [16]);
  parser.csi(WINDOW_OPTIONS, [18]);
  parser.osc(4, `1;${QUERY}`);
  parser.osc(10, QUERY);
  parser.osc(11, QUERY);
  parser.osc(12, QUERY);
  return 18;
}

describe('the renderer answers nothing', () => {
  it('writes nothing back for any query a terminal can be asked', () => {
    // **The defect, and the whole of it.** Every one of these is answered by the daemon
    // before the bytes reach this window (D-7): its virtual terminal has a reply sink and
    // the pump writes what that sink collects to the pty. A second answer from here leaves
    // through `onData` as input nobody typed, and ConPTY reads the end of a cursor-position
    // report as F3 — `cmd`'s recall-previous-command.
    const parser = muted();
    askEverything(parser);
    expect(parser.emitted).toEqual([]);
  });

  it('is what stops them: the same questions all answer without it', () => {
    // Traps register #12 — a gate ships with a proof that it trips. Without the mute every
    // question above is answered, which is the state v0.1 shipped in. Counted against what
    // `askEverything` asked rather than a literal, so adding a question moves both.
    const parser = wired();
    const asked = askEverything(parser);
    expect(parser.emitted).toHaveLength(asked);
  });

  it('covers exactly the sequences xterm answers, and no others', () => {
    // An exact set rather than a spot check. A registration here that xterm has no responder
    // for swallows a sequence for no reason, and one missing is the defect back. The keys are
    // `prefix|intermediates|final`.
    const parser = new ModelParser();
    muteTerminalReplies(parser);

    const expected = [
      DA1,
      DA2,
      DSR,
      DECDSR,
      XTVERSION,
      DECRQM,
      DECRQM_PRIVATE,
      KITTY_QUERY,
      WINDOW_OPTIONS,
    ];
    expect(parser.registeredCsi()).toEqual(expected.map(key).sort());
    expect(parser.registeredDcs()).toEqual([key(DECRQSS)]);
    expect(parser.registeredOsc()).toEqual([INDEXED_COLOUR, 10, 11, 12]);
  });
});

describe('the sequences that do more than answer', () => {
  it('lets a window option that is not a report through to the built-in', () => {
    // `CSI 22 t` pushes the window title and `CSI 23 t` pops it. Muting the whole of `CSI t`
    // would have been the smaller table and would break both the moment `windowOptions` is
    // set — which it is not today, so this pins the shape rather than a live behaviour.
    const parser = muted();
    parser.csi(WINDOW_OPTIONS, [22, 0]);
    parser.csi(WINDOW_OPTIONS, [23, 0]);
    // An empty `CSI t` reads as option 0, which is what xterm's own `params.params[0]` gives
    // it — a `Params` is never shorter than one. Nothing reports for 0, so it is not the
    // mute's business either.
    parser.csi(WINDOW_OPTIONS, [0]);

    expect(parser.emitted).toEqual([]);
    expect(parser.acted).toEqual([
      'window option 22',
      'window option 23',
      'window option 0',
    ]);
  });

  it('reads the leading value of a parameter that carries sub-parameters', () => {
    // xterm hands a parameter with sub-parameters back as an array (`CSI 4 : 3 m`). None of
    // the muted sequences take one, but reading `[18, 1]` as the object it is rather than as
    // the number 18 would let a report through on a sequence nobody expected.
    const parser = muted();
    parser.csi(WINDOW_OPTIONS, [[18, 1]]);
    expect(parser.emitted).toEqual([]);
    expect(parser.acted).toEqual([]);
  });

  it('lets a colour that is being set through, and swallows one being asked for', () => {
    const parser = muted();
    parser.osc(4, '1;rgb:12/34/56');
    parser.osc(10, 'rgb:ab/cd/ef');
    parser.osc(11, 'rgb:00/00/00');
    parser.osc(12, 'rgb:ff/ff/ff');

    expect(parser.emitted).toEqual([]);
    expect(parser.acted).toEqual([
      'colour 4;1 = rgb:12/34/56',
      'colour 10 = rgb:ab/cd/ef',
      'colour 11 = rgb:00/00/00',
      'colour 12 = rgb:ff/ff/ff',
    ]);
  });

  it('reads an OSC 4 question in the value slot, never in the index', () => {
    // `OSC 4` is `index;spec` pairs, so only the odd slots can ask. A `?` where an index
    // belongs is not a query — xterm's own `/^\d+$/` test throws that pair away — and reading
    // it as one would swallow the palette change beside it over a malformed neighbour. The
    // first pair here is valid and must still be applied.
    const parser = muted();
    parser.osc(INDEXED_COLOUR, `1;rgb:12/34/56;${QUERY};rgb:ab/cd/ef`);
    expect(parser.emitted).toEqual([]);
    expect(parser.acted).toEqual(['colour 4;1 = rgb:12/34/56']);
  });

  it('swallows a sequence that both sets and asks, rather than answering half of it', () => {
    const parser = muted();
    parser.osc(4, `1;rgb:12/34/56;2;${QUERY}`);
    expect(parser.emitted).toEqual([]);
    expect(parser.acted).toEqual([]);
  });
});
