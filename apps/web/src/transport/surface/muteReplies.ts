/**
 * Stop the renderer answering questions on the authority's behalf.
 *
 * ## Why a display cache must not speak
 *
 * D-7: terminal state lives in Rust and the webview is a display cache. The daemon's virtual
 * terminal is built with a reply sink and its pump writes what that sink collects straight
 * back to the pty — see `nysia_core::rpc::session::spawn_pump`, which calls `take_replies`
 * on every chunk it feeds. **So by the time a `ESC[6n` reaches this window, the authority has
 * already answered it.** xterm parses the same query, answers it again, and that second
 * answer leaves through `onData` and `terminal_send` as if somebody had typed it.
 *
 * On Windows that is not a harmless duplicate. ConPTY reads the `…R` that ends a
 * cursor-position report as F3, and `cmd` treats F3 as recall-previous-command — which is
 * the phantom command line §12 q7 was opened for. It looked harmless at a first attach only
 * because there was nothing yet to recall: ConPTY's own startup probe gets the authority's
 * reply and then this one, and the second arrives as F3 against an empty history. It was
 * never harmless. It was invisible.
 *
 * The replay boundary in {@link import('./XtermSurface').XtermSurface} closes the *replayed*
 * case by holding `onData` shut until the scrollback has been parsed. This closes the live
 * one, which the boundary cannot reach and never claimed to: a query the child writes a
 * minute after attaching is parsed with the gate wide open, and its answer was unsolicited
 * every single time.
 *
 * ## Why registering handlers, and not filtering `onData`
 *
 * Recognising replies on the way out means keeping a pattern for every sequence xterm can
 * emit and hoping a keystroke never matches one — `ESC [ 5 ; 3 R` is a reply and also
 * something a user can type. Registering a handler means the reply is never composed, so
 * there is nothing to recognise. #43 asked for the second and rejected the first.
 *
 * ## The ordering this rests on, read from xterm's source rather than its documentation
 *
 * Verified against `@xterm/xterm` 6.1.0-beta.304, the version pinned in `package.json`:
 *
 * - `EscapeSequenceParser.registerCsiHandler` **pushes** onto the per-identifier list
 *   (`handlerList.push(handler)`), and both CSI dispatch sites walk it
 *   **backwards** — `for (let j = handlers.length - 1; j >= 0; j--)` — breaking on the first
 *   handler that returns `true`. `InputHandler` registers its own handlers in its
 *   constructor, so anything registered afterwards runs *first* and a `true` from it means
 *   the built-in never runs. `OscParser.end` and `DcsParser.unhook` iterate and break the
 *   same way.
 * - `OscHandler.end(false)` and `DcsHandler.unhook(false)` do not call their wrapped
 *   callback, so the cleanup pass that runs over the handlers left below the winner cannot
 *   let a built-in answer after all.
 * - `EscapeSequenceParser.reset()` resets parser *state*; it does not clear the handler
 *   lists. That matters here because {@link import('./TerminalSurface').RESET_SEQUENCE} is
 *   written by this repository after a hidden buffer overflows — `ESC c` routes to
 *   `InputHandler.fullReset`, which calls that same `reset()`. A mute that died on the first
 *   overflow would come back only on the panes that had already lost output.
 * - `Terminal.parser` is **not** behind `allowProposedApi`, and `ParserApi` forwards to
 *   `CoreTerminal` and then to `InputHandler` without reordering anything. The one thing it
 *   does add is a `CSI t` wrapper that consults `windowOptions` first — which is harmless
 *   here, because when an option is off that wrapper returns `true` and the built-in does not
 *   run either.
 *
 * ## What is muted, and what is deliberately not
 *
 * Everything below is a sequence whose built-in handler reaches
 * `coreService.triggerDataEvent` **while parsing output** — directly, or through an event
 * `CoreBrowserTerminal` answers for it. Where a handler both acts and answers, only the asking
 * form is taken. One thing xterm writes back that is *not* triggered by parsing is named a few
 * paragraphs down; it is the boundary of what this file can claim.
 *
 * **No ESC handler is registered.** #43 named `registerEscHandler` as part of the fix shape,
 * and reading the source says it is not needed: every `registerEscHandler` call in
 * `InputHandler` is a cursor save/restore, an index, a charset selection, a keypad mode,
 * `ESC c`, or `ESC # 8` — none of them write back, and there is no `ESC Z` (DECID) responder
 * to mute. Registering one for symmetry would claim a guarantee about a handler that does
 * not exist. Same for APC: `InputHandler` forwards `registerApcHandler` and registers
 * nothing under it.
 *
 * **One thing the renderer can still emit is outside this table, and outside what a parser
 * hook can reach.** `DECSET 2031` asks a terminal to *notify* the program whenever the colour
 * scheme changes, and xterm honours it from its theme-change listener — `CoreBrowserTerminal`
 * registers `this._themeService.onChangeColors(…)` and emits `CSI ?997;Nn` when
 * `decPrivateModes.colorSchemeUpdates` is set. Nothing parses at that moment, so there is no
 * handler to displace, and Nysia has a runtime theme switcher, so the path is reachable: a
 * program that set 2031 gets a line typed at it the next time the user changes theme.
 *
 * It is left, deliberately, and recorded in §12 q7. It is the one item here the authority does
 * **not** hold — the daemon has no theme — the program asked to be told rather than the cache
 * volunteering, and the only way to suppress it from this side is to intercept `CSI ? h` and
 * drop one parameter out of a list that may carry several modes the program does want. That is
 * a different change from this one, and pretending otherwise by leaving the sentence above
 * unqualified would be the same fault #43 was opened for.
 *
 * A query nobody answers is the remaining cost, and it is bounded. The daemon answers DA1,
 * DA2, DSR 5/6, DECRQM and `CSI 18 t` — `alacritty_terminal`'s `identify_terminal`,
 * `device_status`, `report_mode`/`report_private_mode` and `text_area_size_chars`, all of
 * which reach the pty through the reply sink. It does **not** answer XTVERSION, DECRQSS,
 * `CSI 14/16 t` or an OSC colour query: alacritty either has no handler or raises an event
 * (`TextAreaSizeRequest`, `ColorRequest`) that the sink drops. Those four now go unanswered
 * rather than being answered by the cache, which is the correct direction under D-7 — the
 * window's palette, cell size and version are not the session's — and a gap recorded in
 * §12 q7 for the daemon to close if a program ever needs it.
 *
 * The kitty keyboard query is **not** in that list, in either direction: neither side answers
 * it as either is configured, and both are one option away from doing so. See its entry in
 * the table.
 */

/** How xterm names a sequence: an optional prefix and intermediates, and a final byte. */
export interface SequenceId {
  /** `\x3c`..`\x3f`, the byte before the parameters. */
  readonly prefix?: string;
  /** `\x20`..`\x2f`, the bytes between the parameters and the final. */
  readonly intermediates?: string;
  /** The byte that ends the sequence. */
  readonly final: string;
}

/** What a registration hands back. Registrations here are never disposed; see below. */
export interface Registration {
  dispose(): void;
}

/**
 * The slice of xterm's parser API this needs — declared, not imported.
 *
 * `./xterm.ts` is the only module in the repository that imports `@xterm/*`, and that rule is
 * worth more here than anywhere else: written against a structural interface, the table below
 * is exercised by the node-only tests (D-18) against a registrar that records, which is what
 * makes "these exact sequences, and this return for each" a checked claim rather than a
 * comment. What the node tests cannot see is xterm's dispatch order — that is read from the
 * source above, and it is the reason the replay boundary stays where it is.
 */
export interface ReplyParser {
  registerCsiHandler(
    id: SequenceId,
    handler: (params: (number | number[])[]) => boolean,
  ): Registration;
  registerDcsHandler(
    id: SequenceId,
    handler: (data: string, params: (number | number[])[]) => boolean,
  ): Registration;
  registerOscHandler(ident: number, handler: (data: string) => boolean): Registration;
}

/**
 * The CSI sequences whose built-in handler does nothing but write a reply.
 *
 * Muted unconditionally, because there is no second effect to lose. Each line names the
 * `InputHandler` method it displaces, so the next version bump has something to check against.
 */
const REPLY_ONLY_CSI: readonly SequenceId[] = [
  /** DA1 — `sendDeviceAttributesPrimary`. */
  { final: 'c' },
  /** DA2 — `sendDeviceAttributesSecondary`. */
  { prefix: '>', final: 'c' },
  /** DSR — `deviceStatus`: `CSI 5 n` status, `CSI 6 n` cursor position. */
  { final: 'n' },
  /** DECDSR — `deviceStatusPrivate`: `CSI ? 6 n`, and `CSI ? 996 n`'s colour-scheme report. */
  { prefix: '?', final: 'n' },
  /** XTVERSION — `sendXtVersion`, which answers with xterm.js's own version. */
  { prefix: '>', final: 'q' },
  /** DECRQM — `requestMode`. */
  { intermediates: '$', final: 'p' },
  /** DECRQM, private — `requestMode`. */
  { prefix: '?', intermediates: '$', final: 'p' },
  /**
   * The kitty keyboard query — `kittyKeyboardQuery`. **Muted for safety, not because it is
   * answered.** In the pinned build it returns without writing anything unless
   * `vtExtensions.kittyKeyboard` is set, and `./xterm.ts` does not set it; xterm's default
   * `vtExtensions` is `{}`. The daemon is in the same position from the other side —
   * `alacritty_terminal`'s `report_keyboard_mode` returns early unless `kitty_keyboard` is on,
   * and 0.26 defaults it to `false`. Listed anyway: an option flip is one line and would make
   * a responder of it, which is the shape CLAUDE.md §6 calls a suggestion rather than a
   * default.
   */
  { prefix: '?', final: 'u' },
];

/** DECRQSS — `requestStatusString`, the one DCS sequence that answers. */
const REPLY_ONLY_DCS: SequenceId = { intermediates: '$', final: 'q' };

/** `CSI t`, whose handler both reports and manipulates the title stack. */
const WINDOW_OPTIONS: SequenceId = { final: 't' };

/**
 * The `CSI t` options that report: window size in pixels, cell size in pixels, window size in
 * cells. Everything else `windowOptions` implements pushes or pops a title, and is left.
 *
 * **In this app nothing here runs, and that is not a reason to drop it.** `InputHandler`
 * wraps every `{ final: 't' }` registration — the mute's included — in a
 * `paramToWindowOption` check against the `windowOptions` option, which xterm defaults to
 * `{}` and `./xterm.ts` does not set; so a `CSI t` of any kind is refused before either
 * handler is reached. The split is what the app would need the moment a window option is
 * turned on, and getting it wrong then would be a report nobody asked for or a title stack
 * that stopped working.
 */
const REPORTING_WINDOW_OPTIONS: readonly number[] = [14, 16, 18];

/** `OSC 4` addresses the palette by index, so its payload is `index;spec` pairs. */
const INDEXED_COLOUR = 4;

/** The OSC idents that set a colour, or report it when the value is a question mark. */
const COLOUR_OSC: readonly number[] = [INDEXED_COLOUR, 10, 11, 12];

/** The value that turns an OSC colour sequence from an instruction into a question. */
const COLOUR_QUERY = '?';

/**
 * Register the mute on a freshly built terminal's parser.
 *
 * Call it once, before the terminal is written to. Nothing disposes the registrations: they
 * live exactly as long as the parser they are on, and a terminal whose mute could be lifted
 * while it is still attached would be a security default an ordinary caller can undo
 * (CLAUDE.md §6). The disposables are dropped rather than stored for that reason.
 */
export function muteTerminalReplies(parser: ReplyParser): void {
  for (const id of REPLY_ONLY_CSI) {
    parser.registerCsiHandler(id, () => true);
  }
  parser.registerDcsHandler(REPLY_ONLY_DCS, () => true);

  // Reported, not handled: `true` swallows the report and `false` leaves the built-in to run.
  // See the constant — xterm's own `windowOptions` gate refuses every `CSI t` in this app
  // before either of them is reached, so this is the shape the split must have rather than a
  // behaviour that is live today.
  parser.registerCsiHandler(WINDOW_OPTIONS, (params) =>
    REPORTING_WINDOW_OPTIONS.includes(leadingParam(params)),
  );

  for (const ident of COLOUR_OSC) {
    parser.registerOscHandler(ident, (data) => asksForAColour(ident, data));
  }
}

/**
 * The first parameter of a sequence, as a plain number.
 *
 * xterm hands back `number | number[]`, the array being a parameter with sub-parameters
 * (`CSI 4 : 3 m`). None of the sequences here take one, and a missing parameter is zero,
 * which is what xterm's own `params.params[0]` reads for an empty `CSI t`.
 */
function leadingParam(params: (number | number[])[]): number {
  const first = params[0];
  if (typeof first === 'number') {
    return first;
  }
  return first?.[0] ?? 0;
}

/**
 * Whether an OSC colour sequence is asking rather than setting.
 *
 * `OSC 4` carries `index;spec` pairs and the spec is the odd slot; `OSC 10/11/12` carry bare
 * specs, one per slot, starting at the colour the ident names. A slot of exactly `?` is the
 * query form — the same test `setOrReportIndexedColor` and `_setOrReportSpecialColor` make.
 *
 * **A sequence that both sets and asks is swallowed whole.** Splitting it would mean
 * answering half of it, and answering is the thing this exists to stop; a program that wants
 * its palette set and read back can send two sequences.
 */
function asksForAColour(ident: number, data: string): boolean {
  const slots = data.split(';');
  const first = ident === INDEXED_COLOUR ? 1 : 0;
  const stride = ident === INDEXED_COLOUR ? 2 : 1;
  for (let at = first; at < slots.length; at += stride) {
    if (slots[at] === COLOUR_QUERY) {
      return true;
    }
  }
  return false;
}
