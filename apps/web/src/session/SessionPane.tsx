import { useSnapshot } from '../store/hooks';
import { TerminalView } from '../transport/TerminalView';
import { GLYPH } from '../ui/glyphs';

/**
 * The main pane: a terminal, full height, and nothing else.
 *
 * **v0.1 renders the shell half of design-spec.md §3 only.** The spec draws an *agent*
 * session — a command block, an agent turn, footer meta, a prompt input and a chips row of
 * model, context and token rates — and that drawing is accurate; it is the surface a Claude
 * session gets. It is not a fixture of this pane. Until it exists, every session here is a
 * pty and the only place to type is the terminal itself, so the pane is the terminal.
 *
 * Item 4 of §3, the prompt input, used to be transcribed here as a decorative row: an
 * accent `❯`, the words "Send a message", a block cursor, and the focus ring permanently
 * on. Nothing typed into it and nothing read it. A second place to type is worse than dead
 * chrome — it is a claim about how the thing works that is false. It and the chips row
 * (item 5) come back in v0.2 as parts of the agent surface, built against a real
 * transcript, and they belong to that component rather than to this one. If you are here
 * because the spec shows a prompt and the app does not: that is the reason, and putting one
 * back below a shell would reintroduce the lie.
 *
 * Geometry: a 12px horizontal gutter and no vertical padding, so the grid fills the body
 * between the tab strip and the status bar. The spec's `0 24px` is the gutter of a
 * transcript *card*, which pads itself; a raw terminal grid does not, and 48px of chrome
 * costs about six columns at Fira Code 12.5px.
 *
 * `flex-1 min-h-0` on the root is load-bearing and was missing. A column-flex child that
 * does not grow sizes to its content, so the pane takes the height xterm gave itself and
 * `fit()` measures a box the terminal had already chosen — the fit then follows the window
 * only by accident. That is a reading of the flex chain rather than a measurement: this
 * repo has no DOM to measure in (D-18), so the chain is held by
 * `SessionPane.render.test.ts` and the fit that consumes it by `TerminalView`'s
 * `ResizeObserver`. Anything added between the two needs `flex-1 min-h-0` as well.
 *
 * The terminal inside it is `TerminalView` from `src/transport`, which owns the renderer,
 * the WebGL pool, the acknowledgement of rendered bytes and the resize that follows this
 * geometry to the pty — none of which a component should know about.
 *
 * Terminal state lives in Rust (D-7): this pane is a display cache, never an authority,
 * which is why it holds no scrollback of its own.
 */
export function SessionPane() {
  const { tabs, activeTab } = useSnapshot();
  const tab = tabs.find((candidate) => candidate.paneKey === activeTab);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-3">
      {tab ? <TerminalView /> : <NoSession />}
    </div>
  );
}

/**
 * What the pane shows with no session open.
 *
 * Deliberate rather than apologetic — a tile, a heading and one instruction — because this
 * is the first thing a new window shows and one line of grey text reads like a failure to
 * load. The tile borrows the spec's icon-tile shape (§3, the usage popover): `--bg0`, 1px
 * `--line`, the glyph in the accent.
 *
 * The copy names what v0.1 can actually do. The `+` menu does offer Claude, but starting it
 * opens a pty running `claude`, not the agent surface — so the promise made here is a
 * terminal, and the transcript-and-prompt half is dated rather than implied.
 *
 * It also names the control the way the control names itself. The line used to read *press
 * the `+` in the title bar*, and both halves of that were wrong for anyone not looking at
 * the screen: the button is announced as **New session**, so a listener went hunting for a
 * control called plus, and there is no keyboard shortcut for *press* to be about (#40). The
 * accessible name is the copy now, and the glyph beside it is `aria-hidden` decoration —
 * the sentence read aloud and the sentence on screen point at the same button.
 *
 * `SessionPane.render.test.ts` takes that name from `NewTabButton` rather than repeating
 * the string, so renaming the button fails this copy instead of quietly parting from it.
 */
function NoSession() {
  return (
    <div className="m-auto flex max-w-[380px] flex-col items-center gap-3 p-10 text-center">
      <div
        aria-hidden="true"
        className="border-line bg-bg0 text-acc grid size-12 place-items-center rounded-card border font-mono text-lg"
      >
        {GLYPH.shell}
      </div>
      <div className="text-row font-semibold">No session open</div>
      <p className="text-fg2 text-term leading-normal">
        Choose{' '}
        <span aria-hidden="true" className="text-fg font-mono">
          {GLYPH.add}
        </span>{' '}
        <span className="text-fg">New session</span> in the title bar to open a shell.
      </p>
      <p className="text-fg3 text-chip leading-normal">
        Every v0.1 session is a terminal, and you type into the terminal itself. The agent
        surface — transcript, prompt and chips — arrives in v0.2.
      </p>
    </div>
  );
}
