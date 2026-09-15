import type { SessionKind } from '../generated/SessionKind';

/**
 * Every glyph the chrome draws, transcribed from `docs/design/Nysia-ADE.dc.html`.
 *
 * Collected in one module for two reasons: a glyph is copy, and copy scattered through
 * twenty components drifts; and several of these are visually similar enough
 * (`⑂` vs `⚭`, `✱` vs `✳`) that a silent substitution in a merge is easy to miss and
 * impossible to review inline.
 *
 * `✱` and `⁂` are the one deliberate near-pair, and are written down here rather than left
 * to be discovered as drift: the asterism is three of the agent star because orchestration
 * is agents, plural. The rhyme is the meaning.
 *
 * They are characters, not an icon font — the design uses them at text weight, inheriting
 * the token colour, and a font file would be one more thing to ship for two dozen shapes.
 * Inheriting is also what makes them theme-safe for free: a character has no colour of its
 * own, so the theme switcher and the accent picker carry it without a token per glyph.
 *
 * ## The rule every addition has to pass
 *
 * The character must default to **text** presentation, and `glyphs.test.ts` is the gate.
 * A default-emoji code point — `⚡`, `🐧` — renders as a colour bitmap that no token can
 * reach and no theme can switch: a hardcoded colour that `theme/colourGuard.ts` cannot
 * see, because it is not a literal anywhere in the syntax tree but a property of the code
 * point itself. That is the one way a glyph can break design-spec.md §6.8 while looking
 * like copy.
 *
 * Presentation, not the `Emoji` property, is where the line is drawn: `⚙` (U+2699) carries
 * `Emoji=Yes` and has shipped since v0.1, because its *default* is text and a browser
 * honours the default absent a variation selector.
 *
 * Each addition names its Unicode block, because neither bundled face carries these —
 * Space Grotesk and Fira Code cover almost none of them, so every one falls through to the
 * platform symbol font (Segoe UI Symbol on Windows, Apple Symbols on macOS) and coverage
 * there is the thing a reviewer has to eyeball. No gate in this repo can check it.
 */
export const GLYPH = {
  /** Agent. Rendered in the accent with a glow. */
  agent: '✱',
  /** Shell. Always in Fira Code, so the two characters line up as one cell. */
  shell: '>_',
  branch: '⑂',
  search: '⌕',
  prompt: '❯',
  session: '▤',
  tasks: '◉',
  history: '◷',
  settings: '⚙',
  help: '?',
  add: '+',
  close: '×',
  chevron: '›',
  back: '←',
  refresh: '↻',
  dot: '●',
  minimize: '─',
  maximize: '☐',
  quit: '✕',

  /*
   * The settings nav (design-spec.md §5). Everything above was already drawn somewhere in
   * the chrome; these six are what the nav needed and the vocabulary did not have.
   */

  /** Orchestration: an asterism, U+2042, General Punctuation. Three of `agent`. */
  orchestration: '⁂',
  /**
   * AI provider accounts: a key, U+26BF, Miscellaneous Symbols.
   *
   * The highest coverage risk in this table — it is the only character here outside the
   * blocks a symbol font is certain to carry, and the first one to swap if a real-app run
   * shows tofu. Kept because a key is what a stored provider credential is, and the
   * alternatives were all shapes with nothing behind them.
   */
  credentials: '⚿',
  /** Appearance: U+25D0, Geometric Shapes. The half-filled circle contrast is drawn as. */
  contrast: '◐',
  /** Integrations: U+21C4, Arrows. Two-way traffic with something outside Nysia. */
  exchange: '⇄',
  /** Automations: U+25B7, Geometric Shapes. Something that runs without you starting it. */
  run: '▷',
  /** Quick commands: U+2318, Miscellaneous Technical. The same mark as the ⌘K hint. */
  command: '⌘',

  /*
   * The `+` menu's shells (#73). Two mono characters in one cell, the shape `shell`
   * already established — see `launcherGlyph` for why these and not invented geometry.
   */

  /** PowerShell 7, from its own prompt: `PS C:\>`. */
  powershell: 'PS',
  /** The Command Prompt, from its own prompt: `C:\>`. */
  commandPrompt: 'C:',
  /** WSL, from the home prompt a distribution opens on: `user@host:~$`. */
  wsl: '~$',
} as const;

/** The Command-K affordance. Spelled with the command glyph on every platform, as designed. */
export const COMMAND_PALETTE_HINT = `${GLYPH.command}K`;

/** Settings has its own search, on its own key (design-spec.md §5). */
export const SETTINGS_SEARCH_HINT = '⌘F';

/**
 * The mark for one entry in the `+` menu.
 *
 * Keyed off the launcher id, not the label and not the hint, because the id is the only
 * one of the three anything treats as an identifier: `DaemonStore.profileFor` switches on
 * exactly these four strings to choose a `ShellProfile`, so this reads the same closed set
 * from the same place. A label is copy and a hint is decoration — either can be reworded
 * by someone who never thinks about the glyph column, and the column would go quietly
 * wrong rather than fail.
 *
 * Every shell is marked with something it actually shows you. `PS` and `C:` are the two
 * prompts' own prefixes, `~$` is the home prompt a WSL session opens on, and Git Bash
 * takes the branch glyph because it *is* Git's shell — `pty::profile` resolves it from
 * `git.exe` rather than from a bare `bash`, and for the same reason: the thing that
 * identifies it is Git. Invented geometry was the alternative and it is strictly worse;
 * an arbitrary shape has to be learned too, and there is nothing behind it to learn.
 *
 * Every agent is `✱`, and that is not a gap. Claude is the only agent and there is no
 * provider trait (D-3, D-4), so a second agent mark would be a shape with nothing behind
 * it — the same reasoning `SessionGlyph` already carries.
 *
 * Anything unrecognised keeps `>_`: a shell the daemon grows later should look like a
 * shell rather than like nothing. What it must not do is look like the other four, which
 * is the whole of #73 — one mark on four entries, told apart only by the hint at the far
 * end of the row.
 */
export function launcherGlyph(id: string, kind: SessionKind): string {
  if (kind === 'agent') {
    return GLYPH.agent;
  }
  switch (id) {
    case 'shell.pwsh':
      return GLYPH.powershell;
    case 'shell.cmd':
      return GLYPH.commandPrompt;
    case 'shell.git_bash':
      return GLYPH.branch;
    case 'shell.wsl':
      return GLYPH.wsl;
    default:
      return GLYPH.shell;
  }
}
