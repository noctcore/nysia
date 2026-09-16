/**
 * The two glyphs this screen needs that the chrome's vocabulary does not have yet.
 *
 * They belong in `ui/glyphs.ts` with the other three dozen — *"a glyph is copy, and copy
 * scattered through twenty components drifts"* is that module's opening argument and it is
 * right. They are here because `ui/` is not this wave's to edit; one named module inside
 * `tasks/` is the nearest thing to the rule that can be done from here, and it is a great
 * deal better than the same character inlined at three call sites. Folding these two into
 * `GLYPH` is a one-line move for whoever next owns `ui/`.
 *
 * Both pass that module's one hard requirement: the code point must default to **text**
 * presentation. A default-emoji code point renders as a colour bitmap that no token can
 * reach and no theme can switch — a hardcoded colour `theme/colourGuard.ts` cannot see,
 * because it is a property of the character rather than a literal in the syntax tree.
 *
 * Neither bundled face carries either of them, so both fall through to the platform symbol
 * font, which is why each names its Unicode block: that is the coverage a reviewer has to
 * eyeball and no gate in this repository can check.
 */
export const TASK_GLYPH = {
  /** `Start →`: U+2192 RIGHTWARDS ARROW, Arrows. */
  start: '→',
  /** A row's overflow menu: U+22EE VERTICAL ELLIPSIS, Mathematical Operators. */
  overflow: '⋮',
  /** The repository dropdown's caret: U+25BE, Geometric Shapes. */
  caret: '▾',
  /** Open externally: U+2197 NORTH EAST ARROW, Arrows. */
  external: '↗',
  /** The filter affordance: U+2254, Mathematical Operators. As the design draws it. */
  filters: '≔',
} as const;
