/**
 * The finder behind the hardcoded-colour guard.
 *
 * design-spec.md §6.8 is blunt about this: theme and accent are live user tweaks, so a
 * colour that is not a token is a pixel that stops following the switcher. The guard turns
 * that from a review habit into a gate.
 *
 * It used to look only for hex, which left three ways to write a fixed colour and compile
 * clean: a CSS colour function, a Tailwind palette class, and a named colour inside an
 * arbitrary value. `index.css` now closes the palette with `--color-*: initial` so the
 * third of those cannot compile at all — but the guard still names it, because a future
 * `@theme` edit could reopen the ramp and nothing else would notice.
 *
 * The four rules below are deliberately separate: each one reports what kind of violation
 * it found, so a failure says what to do rather than just where.
 */

/** What a scan found, with enough context to fix it without opening the file. */
export interface ColourLiteral {
  readonly kind: 'hex' | 'function' | 'palette-class' | 'named-colour';
  readonly text: string;
}

/** Rule 1: a hash followed by exactly 3, 4, 6 or 8 hex digits. */
const HEX =
  /#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})(?![0-9a-fA-F])/g;

/**
 * Rule 2: a CSS colour function.
 *
 * The leading guard rejects a match preceded by a word character or a hyphen, which is
 * what keeps `var(--color-acc)` — where the word is preceded by a hyphen — out of the
 * results while still catching the bare CSS `color` function on its own.
 */
const COLOUR_FUNCTION =
  /(?<![\w-])(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color-mix|color)\s*\(/g;

/**
 * Rule 3: a Tailwind palette utility.
 *
 * The ramp names are split from a string rather than written as an array literal on
 * purpose — an array of colour names is itself a bracketed span full of colour names, and
 * rule 4 would report this module as its own worst offender.
 */
const PALETTE_RAMPS =
  'slate gray grey zinc neutral stone red orange amber yellow lime green emerald teal cyan sky blue indigo violet purple fuchsia pink rose'.split(
    ' ',
  );
const COLOUR_PROPERTIES =
  'text bg border ring fill stroke outline decoration accent caret divide shadow placeholder from via to'.split(
    ' ',
  );
const PALETTE_CLASS = new RegExp(
  `(?<![\\w-])(?:${COLOUR_PROPERTIES.join('|')})-(?:${PALETTE_RAMPS.join('|')}|white|black)(?:-\\d{2,3})?(?:/\\d{1,3})?(?![\\w-])`,
  'g',
);

/**
 * Rule 4: a bare CSS colour name, in either of the two places one can be written.
 *
 * It cannot scan whole files the way the hex and function rules do — these words are
 * ordinary English, and "the red build turned green" is not a violation. So it looks in
 * the two syntactic positions where a bare word *is* a colour:
 *
 *  - inside a Tailwind arbitrary value, which is bracketed and never carries a quote;
 *  - as the entire content of a quoted string, which is what an inline style is. The rule
 *    used to stop at brackets, so an inline style naming a colour shipped with every gate
 *    green — while the same style naming a hex or a colour function was caught, because
 *    those two rules scan the whole file. The scope is consistent now.
 *
 * Requiring the *whole* string to be the colour name is what keeps prose out: a comment
 * mentioning one is not a one-word string literal. Backticks are excluded for the same
 * reason, since a doc comment marks up code with them.
 */
const BRACKET_SPAN = /\[[^\]'"`]*\]/g;
const WHOLE_STRING = /'\s*([a-zA-Z]+)\s*'|"\s*([a-zA-Z]+)\s*"/g;
const NAMED_COLOURS = new Set(
  ('aliceblue antiquewhite aqua aquamarine azure beige bisque black blanchedalmond blue ' +
    'blueviolet brown burlywood cadetblue chartreuse chocolate coral cornflowerblue ' +
    'cornsilk crimson cyan darkblue darkcyan darkgoldenrod darkgray darkgreen darkgrey ' +
    'darkkhaki darkmagenta darkolivegreen darkorange darkorchid darkred darksalmon ' +
    'darkseagreen darkslateblue darkslategray darkslategrey darkturquoise darkviolet ' +
    'deeppink deepskyblue dimgray dimgrey dodgerblue firebrick floralwhite forestgreen ' +
    'fuchsia gainsboro ghostwhite gold goldenrod gray green greenyellow grey honeydew ' +
    'hotpink indianred indigo ivory khaki lavender lavenderblush lawngreen lemonchiffon ' +
    'lightblue lightcoral lightcyan lightgoldenrodyellow lightgray lightgreen lightgrey ' +
    'lightpink lightsalmon lightseagreen lightskyblue lightslategray lightslategrey ' +
    'lightsteelblue lightyellow lime limegreen linen magenta maroon mediumaquamarine ' +
    'mediumblue mediumorchid mediumpurple mediumseagreen mediumslateblue ' +
    'mediumspringgreen mediumturquoise mediumvioletred midnightblue mintcream mistyrose ' +
    'moccasin navajowhite navy oldlace olive olivedrab orange orangered orchid ' +
    'palegoldenrod palegreen paleturquoise palevioletred papayawhip peachpuff peru pink ' +
    'plum powderblue purple rebeccapurple red rosybrown royalblue saddlebrown salmon ' +
    'sandybrown seagreen seashell sienna silver skyblue slateblue slategray slategrey ' +
    'snow springgreen steelblue tan teal thistle tomato turquoise violet wheat white ' +
    'whitesmoke yellow yellowgreen').split(' '),
);

/**
 * Every hardcoded colour in `source`.
 *
 * `currentcolor`, `transparent` and `inherit` are deliberately absent from the named list:
 * all three follow whatever the theme sets, which is the opposite of the problem.
 */
export function findColourLiterals(source: string): readonly ColourLiteral[] {
  const found: ColourLiteral[] = [];

  for (const match of source.matchAll(HEX)) {
    found.push({ kind: 'hex', text: match[0] });
  }
  for (const match of source.matchAll(COLOUR_FUNCTION)) {
    found.push({ kind: 'function', text: match[0].trim() });
  }
  for (const match of source.matchAll(PALETTE_CLASS)) {
    found.push({ kind: 'palette-class', text: match[0] });
  }
  for (const span of source.matchAll(BRACKET_SPAN)) {
    for (const word of span[0].toLowerCase().matchAll(/[a-z]+/g)) {
      if (NAMED_COLOURS.has(word[0])) {
        found.push({ kind: 'named-colour', text: span[0] });
        break;
      }
    }
  }
  for (const match of source.matchAll(WHOLE_STRING)) {
    const word = (match[1] ?? match[2] ?? '').toLowerCase();
    if (NAMED_COLOURS.has(word)) {
      found.push({ kind: 'named-colour', text: match[0] });
    }
  }

  return found;
}

/** One violation, located precisely enough to click on. */
export interface ColourViolation {
  readonly file: string;
  readonly kind: ColourLiteral['kind'];
  readonly text: string;
}

export interface ScannedFile {
  readonly path: string;
  readonly source: string;
}

/**
 * Sweep a set of files, skipping the ones allowed to define tokens.
 *
 * Separated from the test so the proof can run the *real* sweep over the real tree plus an
 * injected offender, rather than over a string fixture. A guard whose proof only exercises
 * its regex is a guard that can stop being wired up without anyone noticing (trap 12).
 */
export function scanForColourLiterals(
  files: readonly ScannedFile[],
): readonly ColourViolation[] {
  return files
    .filter((file) => !TOKEN_DEFINITION_MODULES.includes(file.path))
    .flatMap((file) =>
      findColourLiterals(file.source).map((literal) => ({
        file: file.path,
        kind: literal.kind,
        text: literal.text,
      })),
    );
}

/**
 * The one TypeScript module allowed to carry colour literals: the theme tables.
 *
 * Adding a second entry is a design decision, not a convenience — which is why it has to be
 * made here, in a diff a reviewer sees. The guard also asserts that every entry *still
 * contains* a literal, so one left behind after a refactor fails rather than silently
 * widening the hole.
 *
 * This module is not on the list. It describes the shapes it hunts for without writing any
 * of them down, which is a constraint worth keeping: a guard that has to exempt itself has
 * stopped being checkable.
 */
export const TOKEN_DEFINITION_MODULES: readonly string[] = ['src/theme/themes.ts'];

/**
 * The one stylesheet in the package, which is the Tailwind `@theme` block itself.
 *
 * Its contents cannot be scanned: vitest runs with CSS processing off, so a `?raw` import
 * of a stylesheet comes back empty, and turning it on is a change to the shared root
 * `vitest.config.ts`. The guard covers the same ground a different way — it asserts this is
 * the *only* stylesheet in `apps/web`, so a second one cannot appear without failing here
 * and forcing the colours in it to be reviewed.
 */
export const TOKEN_DEFINITION_STYLESHEETS: readonly string[] = ['src/index.css'];
