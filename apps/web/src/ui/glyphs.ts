/**
 * Every glyph the chrome draws, transcribed from `docs/design/Nysia-ADE.dc.html`.
 *
 * Collected in one module for two reasons: a glyph is copy, and copy scattered through
 * twenty components drifts; and several of these are visually similar enough
 * (`⑂` vs `⚭`, `✱` vs `✳`) that a silent substitution in a merge is easy to miss and
 * impossible to review inline.
 *
 * They are characters, not an icon font — the design uses them at text weight, inheriting
 * the token colour, and a font file would be one more thing to ship for eighteen shapes.
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
} as const;

/** The Command-K affordance. Spelled with the command glyph on every platform, as designed. */
export const COMMAND_PALETTE_HINT = '⌘K';

/** Settings has its own search, on its own key (design-spec.md §5). */
export const SETTINGS_SEARCH_HINT = '⌘F';
