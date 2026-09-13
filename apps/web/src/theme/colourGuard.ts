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
 * Rule 4: a bare CSS colour name, in the two positions where a bare word *is* a colour.
 *
 * It cannot scan whole files the way the hex and function rules do — these words are
 * ordinary English, and "the red build turned green" is not a violation. So it looks at:
 *
 *  - a Tailwind arbitrary value, which is bracketed and never carries a quote;
 *  - a string literal that is the value of a CSS property, which is what an inline style
 *    is.
 *
 * The property prefix is what makes the second one safe, and it replaced a much blunter
 * rule that flagged *any* single-word quoted string. That rule had three problems, and the
 * prefix answers all of them at once:
 *
 *  - it fired on unrelated code. `apps/web/src/transport` is being written now, and a
 *    protocol literal spelling `gold`, `navy`, `silver` or `tan` would have tripped a
 *    colour guard in a module that paints nothing. A guard that cries wolf on someone
 *    else's file is a guard the next person in a hurry switches off, which costs more than
 *    the case it was catching.
 *  - it could only see one word, so a multi-token value slipped through. A shorthand
 *    naming a colour among other tokens is caught now.
 *  - it had to exclude backticks, because a doc comment marks up code with them. With a
 *    property required in front, a template literal is no more ambiguous than a quoted
 *    one, so all three quote characters are in scope.
 *
 * The first attempt at that prefix required the literal to sit immediately after the
 * property and a colon, which quietly dropped most of what the blunt rule had been
 * catching. Every one of these has a painting property in front of it and was invisible:
 * a ternary between the separator and the literal, which is how a component ordinarily
 * writes a conditional colour; a template literal whose interpolation contains quotes; a
 * JSX attribute, which separates with an equals sign rather than a colon; an assignment
 * through the style object; a `setProperty` call, whose separator is a comma; and a quoted
 * key, where the quote between the property and the colon broke the match. So the
 * separator is now either punctuation, an expression may sit between it and the literal,
 * and each quote character gets its own pattern — which also lets a value contain the
 * *other* quote, catching a shorthand whose url is quoted inside it.
 *
 * ## What it cannot see
 *
 * Every entry below is a real miss, and `colourGuard.test.ts` proves each one: a case per
 * bullet, asserting the shape is not found. Widen the rule and the matching case goes red,
 * which is what keeps the list from rotting into fiction.
 *
 * That is all it proves. **The list is not exhaustive, and there is no way to make it so.**
 * It claimed to be for three rounds, and a reader was told they could trust the rest of the
 * comment because of it — and each round found another shape that was not on it. A test can
 * show that a listed miss is real; nothing can show that no *unlisted* miss exists, because
 * the unlisted ones are precisely the shapes nobody thought of. Read this as the known
 * blind spots, which is useful, and not as the boundary of them, which it never was:
 *
 *  - a colour that reaches CSS through a variable rather than a literal. Needs types.
 *  - a colour name in a string that no painting property introduces. Needs types.
 *  - a value whose own quote character appears inside it escaped, when the colour word
 *    sits before the escape. After it the colour is caught, but by accident rather than by
 *    understanding — the span treats the escaped quote as a boundary and the tail happens
 *    to become the captured value. Needs a parser to be right either way.
 *  - an expression between the property and the literal that contains a semicolon, a comma
 *    or a brace, or that runs past the length cap. Those are what stop the span crossing
 *    out of the value it belongs to, and the price is a call with more than one argument.
 *  - a painting property this list does not name. It covers CSS, and a library that takes
 *    its own colour keys — a terminal theme object, a chart config — is outside it. A key
 *    that merely *contains* one of these words is outside it too: the boundary guard that
 *    stops `fill` matching inside `autofill` is the same guard, and the same trade.
 *
 * The hex and colour-function rules, which do scan whole files, are the backstop for every
 * one of them, listed or not: only a *named* colour can hide in any of these.
 *
 * In the other direction, the rule does have false positives — a value that merely contains
 * a colour word, most obviously a url with one in the path. They are tracked separately;
 * none of them is silent, so none of them can ship a pixel.
 */
const BRACKET_SPAN = /\[[^\]'"`]*\]/g;

/**
 * A property that can paint: anything ending in `color` in either casing, plus the
 * shorthands that take one among other tokens. Matching the suffix rather than a list
 * covers `borderTopColor`, `textDecorationColor` and every sibling without enumerating
 * them.
 *
 * The custom-property alternative is first and is not decoration. Every token in this
 * codebase spells the word as a *prefix* — the names begin with two hyphens and the word
 * and then say what they are for — so a suffix-only pattern could not see any of them, and
 * the one module whose entire job is writing those values was the one module the rule was
 * blind to. Someone "fixing" the accent by writing a literal there would have turned the
 * accent picker into a no-op with every gate green, which is the exact failure this guard
 * exists to prevent. The trailing part after the word is what the suffix rule could not
 * express.
 */
const PAINTING_PROPERTY =
  '--[a-zA-Z0-9-]*[cC]olor[a-zA-Z0-9-]*|' +
  '[a-zA-Z-]*[cC]olor|background|background-?[iI]mage|' +
  'border(?:-?(?:top|right|bottom|left|Top|Right|Bottom|Left))?|' +
  'outline|fill|stroke|box-?[sS]hadow|text-?[sS]hadow';

/**
 * The property and its separator.
 *
 * The leading guard is what stops the list matching inside a longer word — without it
 * `stroke` matched in `keystroke` and `fill` in `autofill` or `refill`, so the rule that
 * had just stopped crying wolf on bare strings started crying wolf on identifiers instead.
 * The other two rules have had that guard from the start.
 *
 * Two separator shapes: bare property then colon or equals, which covers an object literal,
 * a JSX attribute and an assignment; or quoted property then comma, colon or equals, which
 * covers a quoted key and `setProperty`.
 */
const PROPERTY_INTRO =
  `(?<![\\w-])(?:${PAINTING_PROPERTY})(?:\\s*[:=]|['"\`]\\s*[,:=])`;

/**
 * Whatever sits between the separator and the literal — a ternary head, a call, nothing.
 *
 * It crosses line ends, because a hand-wrapped ternary is the ordinary way to write the
 * shape this rule most needs to catch and there is no formatter in this repo to make it
 * one line. Stopping at a line end meant the guard saw the conditional style only when it
 * happened to fit on one, which is a coin toss rather than a rule.
 *
 * What bounds it is punctuation that means "this is a different thing": a semicolon, a
 * comma — which separates one object property from the next — and a brace. Between them a
 * span cannot reach out of the value it belongs to and into a sibling. The length cap is
 * the backstop for anything those miss. The comma costs the rule an expression like a call
 * with two arguments, which is in the residue above.
 *
 * A quote does **not** bound it, and the comment used to say it did — which was not just
 * wrong but backwards. A quoted string in the expression, which is what a comparison like
 * `status === 'failed' ? …` is made of, was taken as the value: the rule read the first
 * literal after the separator, found no colour word in it, and skipped past the real one.
 * So the span swallows a complete quoted string as a unit and the greedy quantifier hands
 * back the *last* literal in range, which is the one the property is actually set to. The
 * alternation is ordered quote-first so a literal is consumed whole rather than a character
 * at a time; both branches are anchored on different characters, so there is no ambiguity
 * for the engine to backtrack through.
 */
const QUOTED_CHUNK = `'[^'\\n]*'|"[^"\\n]*"`;
const BEFORE_VALUE = `(?:${QUOTED_CHUNK}|[^'"\`;,{}]){0,120}`;

/**
 * One pattern per quote character rather than one with a backreference, so a value may
 * contain the other two. That is what catches a shorthand carrying a quoted url, and it is
 * the only way the template pattern can see through an interpolation that contains quotes.
 */
const STYLE_VALUES: readonly RegExp[] = [
  new RegExp(`${PROPERTY_INTRO}${BEFORE_VALUE}'([^'\\n]*)'`, 'g'),
  new RegExp(`${PROPERTY_INTRO}${BEFORE_VALUE}"([^"\\n]*)"`, 'g'),
  new RegExp(`${PROPERTY_INTRO}${BEFORE_VALUE}\`([^\`]*)\``, 'g'),
];
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
  for (const pattern of STYLE_VALUES) {
    for (const match of source.matchAll(pattern)) {
      const value = (match[1] ?? '').toLowerCase();
      for (const word of value.matchAll(/[a-z]+/g)) {
        if (NAMED_COLOURS.has(word[0])) {
          found.push({ kind: 'named-colour', text: match[0] });
          break;
        }
      }
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
