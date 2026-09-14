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
 *
 * All four read this package's own source. What a dependency's stylesheet paints is outside
 * every one of them and inside the built output, so that is tracked separately, at
 * {@link DEPENDENCY_STYLESHEETS} — which is where reading the built CSS rather than the
 * source led.
 */

import ts from 'typescript';

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
 * results while still catching the bare CSS colour function on its own.
 */
const COLOUR_FUNCTION =
  /(?<![\w-])(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color-mix|color)\s*\(/g;

/**
 * Rule 3: a Tailwind palette utility.
 *
 * The ramp names are split from a string rather than written as an array literal on
 * purpose — an array of colour names is itself a bracketed span full of colour names, and
 * rule 4's arbitrary-value half would report this module as its own worst offender.
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
 * Rule 4a: a bare CSS colour name inside a Tailwind arbitrary value.
 *
 * Bracketed and never quoted, which is what makes it safe to look for in raw text: these
 * words are ordinary English, and "the red build turned green" is not a violation. The
 * quote characters in the class are what keep an ordinary array of strings out.
 */
const BRACKET_SPAN = /\[[^\]'"`]*\]/g;

/**
 * Rule 4b: a bare CSS colour name written as the value of something that paints, read off
 * the TypeScript syntax tree.
 *
 * ## Why this is a parser and not a pattern
 *
 * Seven rounds of review each found a *position* the pattern that used to do this read
 * wrongly, and the list is in the order they were found: a property flush against its
 * colon; one behind a ternary; a quoted key, where the quote between the name and the colon
 * broke the match; a custom property, whose name spells the word as a prefix; a JSX
 * expression container, where the brace sat between the separator and the value; whichever
 * branch a greedy quantifier reached last; and a branch written as a template, which the
 * span could not cross when the other two quotes were fine.
 *
 * That run is not luck. Whether a string is the value of a painting property is a question
 * about syntax, and a regular expression can be made right about a position somebody has
 * already thought of, never about position itself, because it has no notion of one. The
 * false positives said it from the other side: an operand, a branch, a key, a `case` label
 * and a comment are five different things that look identical to a pattern over characters,
 * and three of them were reported as violations.
 *
 * So the file is parsed and walked. The precedent is next door in `tools/lint-meta`, whose
 * architecture rules hand-rolled a lexer for the same class of question, hit the same run of
 * positional misses, and deleted it for a syntax-tree walk — `moduleReferences.ts` is the
 * shape this follows, read rather than imported: lint-meta is not a dependency of `apps/web`
 * and making it one would be a cross-package edge for four helpers.
 *
 * Take the same lesson and not more than it: **a parser fixes where you look, not what you
 * are looking for.** Which names paint, which library's keys paint, which wrapper calls
 * paint and what a value hides are vocabulary, and the tree has to be told all of it — with
 * the lists in this file, which is why they are still here.
 *
 * ## What the walk looks at
 *
 * A **painted site** is a place the tree says a name is being set to something:
 *
 *  - a JSX attribute, `<path fill={…} />`, however the value is written;
 *  - an object-literal property, including a quoted key and a computed key that is a
 *    literal — all three are the same node with a different `name`;
 *  - a variable or class-field declaration, `const color = …`;
 *  - a default, wherever one can be written: a parameter's, a destructured binding's, an
 *    enum member's. `function Dot({ color = 'red' })` is the ordinary React spelling of a
 *    hardcoded colour, and the three of them are one branch because the language writes
 *    them the same way. The pattern this replaced caught all of them by accident — it
 *    matched an equals sign and did not care which kind — and the first version of this
 *    walk dropped them, which is a gate getting narrower than the one it replaced;
 *  - a destructuring declaration, which is read through the thing being taken apart rather
 *    than through a name: `const [color, setColor] = useState('red')` is where the other
 *    live React idiom keeps a colour, and it is the one declaration shape whose value is
 *    not written next to the name that receives it. See {@link patternPaints};
 *  - an assignment, `el.style.color = …`, through a member, an index or a bare name;
 *  - one of the four DOM setters at {@link writtenProperty}, where the name is an argument.
 *
 * The name is then checked against the vocabulary — {@link PAINTING_PROPERTY} and
 * {@link TERMINAL_THEME_KEYS} — and if it paints, {@link valueStrings} reads the value.
 *
 * Three things fall out of that for free, and each was a bug with its own bullet before:
 *
 *  - **a comment is trivia**, so a line that describes painting no longer reports as one.
 *    This module can finally write down the shapes it hunts for — but only the ones rule 4
 *    owns, because rules 1 to 3 still read whole files as text and a hex, a colour function
 *    or a palette class written here would still be found. That is not a leak, it is what
 *    makes those three the backstop.
 *  - **a statement is a statement**, so a missing semicolon is the parser's problem rather
 *    than a span running into the next line.
 *  - **a key is a key**, so a quoted colour name followed by a colon — a `case` label, a
 *    label table, a ternary between two colour names — is not a property introducing a
 *    value any more. That was the loudest false positive the rule had.
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
 * the unlisted ones are precisely the shapes nobody thought of. Read this as the known blind
 * spots, which is useful, and not as the boundary of them, which it never was.
 *
 * Ten entries. Eight are vocabulary or value questions that survive a change of technique,
 * and they carried over from the pattern this replaced — but read them rather than counting
 * on that sentence, because two of the eight did move. The computed-key entry narrowed: a
 * key computed from a literal reads like any other key now, and only a name assembled at run
 * time is left. The library-key entry kept its shape and changed its reason: `pointBackground`
 * is outside the vocabulary because the vocabulary does not name it, where before it was a
 * word-boundary guard holding it out. The last two are new: the ninth is the price of the
 * walk being narrow, and the tenth was missed by the pattern too and had simply never been
 * written down:
 *
 *  - a colour that reaches CSS through a variable rather than a literal: the value is a
 *    name at the point where the rule looks, and what it holds is decided somewhere else,
 *    possibly in another module. Nothing short of types can follow that, and the entry is
 *    a limit rather than a bug — the literal itself is still caught wherever it is written.
 *  - a colour name in a string that nothing in the vocabulary introduces. Same limit, the
 *    other way round: the word is there but nothing says it paints. Loosening this is what
 *    the rule was narrowed away from, because it made the guard shout at modules that
 *    paint nothing.
 *  - a painting property the list does not name. It is a useful subset of CSS, not CSS —
 *    `text-decoration`, `column-rule`, `text-emphasis`, the `border-inline` and
 *    `border-block` families, and `filter` or `backdrop-filter` carrying a drop shadow are
 *    all absent. The sentence here used to say it covered CSS, which was simply false.
 *  - a colour key belonging to a library the list does not name. xterm's are named now,
 *    because that terminal is in this app and its keys are where the next literal would be
 *    written; a chart config's, or any other dependency's, are not. `pointBackground` is
 *    the shape to picture: it is not `background`, and the tree compares whole names, so
 *    naming it is the only way in.
 *  - bare CSS text carried inside a string or a template — a `cssText` assignment, a
 *    tagged `css` template, a `style` attribute inside a markup string. In all three the
 *    property that introduces the colour is *inside* the literal and the colour after it is
 *    bare, so there is no value node left for the walk to read. Reading one means parsing
 *    the string's contents as CSS, which is a different parser and not a wider walk — this
 *    is the one entry a TypeScript syntax tree cannot help with even in principle.
 *  - a custom property whose name is only known at run time, which is to say a computed key
 *    holding a template with a substitution in it. A computed key that *is* a literal reads
 *    like any other key now; one assembled from a variable has no name to compare.
 *  - a setter the list at {@link writtenProperty} does not name, and a name and value
 *    written as a tuple for one of the four to consume later. A project's own wrapper around
 *    any of them is the shape most likely to land here, and the tree cannot tell it from
 *    `track('color', 'gold')` — same two arguments, different intent, nothing but the name
 *    to go on.
 *  - a colour carried inside a `url()`, which is set aside as a path before the words are
 *    counted. A data URI can carry a whole stylesheet, so this is the bare-CSS entry above
 *    arriving through a different door. What it paid for was closing the loudest false
 *    positive the rule had, and a percent-encoded stylesheet is not a shape anything in
 *    this tree writes. A *hash* encoded that way is the one miss with no backstop at all,
 *    which is the sentence below rather than this bullet.
 *  - a colour the value only produces by running something: a function body, a tagged
 *    template, a getter. {@link valueStrings} follows the shapes a value is *made* of and
 *    stops everywhere else, and stopping is the point — it is what keeps a condition's
 *    operand and a lookup's key out of the answer. This one is new with the walk. The
 *    pattern it replaces read every literal in a span of characters, so it caught some of
 *    these by accident and reported the operands and the keys for the same reason — but
 *    only some: a colour inside a statement body, `(() => { const c = 'red'; return c; })()`,
 *    was missed by that pattern too, because a semicolon bounded its span. Half of the entry
 *    the rewrite deleted moved here rather than being solved.
 *  - an assignment whose operator is not a plain `=`. `el.style.color ??= 'red'` and its
 *    `||=` sibling paint, and neither is a site: only {@link ts.SyntaxKind.EqualsToken} is.
 *    The pattern this replaced missed them for its own reason — it wanted one equals sign
 *    and these have two characters in front of it — so this is a blind spot both
 *    implementations have and neither had written down. It is listed rather than closed
 *    because widening the site set is a decision, and a decision belongs in a diff of its
 *    own rather than in the margin of a refactor.
 *
 * The hex and colour-function rules, which do scan whole files, are the backstop for nearly
 * every one of them, listed or not — whatever shape hides a colour from rule 4, a hash or a
 * function written in the source text is still read. What they do not read is a colour the
 * source does not spell that way, which is to say one behind an encoding. A percent-encoded
 * hash inside a data URI is the live example and the one miss with nothing underneath it:
 * the encoding hides it from the hex rule, and the `url()` bullet above puts the same value
 * out of rule 4's reach.
 *
 * ## In the other direction
 *
 * The rule has false positives too, and they are tracked separately because they are loud:
 * one fails the sweep and gets looked at, where a miss ships a pixel in silence. That is why
 * the trade usually runs towards catching too much — but not always, because a guard that
 * cries wolf is one somebody eventually switches off. Three are left of the six the pattern
 * had; the comment, the missing semicolon and the quoted key are in the section above, as
 * things the tree answers rather than things to live with.
 *
 *  - a value that merely contains a colour word. The `url()` form is closed, since a path is
 *    the one place a colour word turns up in a value often enough to be worth knowing about.
 *    A token whose own name spells one still fires, and that is the shape most likely to.
 *  - a table keyed by one of the eight ANSI names whose values are prose — a label map, most
 *    plausibly — which fires once per entry. Those words are in the vocabulary because
 *    xterm's theme keys are spelled that way, and `{ red: 'Red alert' }` is the same tree as
 *    `{ red: 'crimson' }`. This is the half of the old quoted-key entry the parser does not
 *    answer: `on ? 'red' : 'gray'` is a branch and stopped firing, but a key really is a key
 *    here, and which keys paint is vocabulary.
 *  - a literal in a call the value is computed from. `token('--color-fg', 'red')` is exactly
 *    why arguments are read — a hardcoded fallback behind a token lookup is the live shape
 *    in this app — and `label('Red alert')` under a painting property is the same tree. Of
 *    the four spellings the old entry named, the tree removed two: a comparison operand is
 *    in a condition and an index is a lookup, and neither is walked. A conditional's other
 *    branch is still read, deliberately: either branch can be the value.
 */

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
 * exists to prevent.
 *
 * Anchored at both ends, because the tree hands over a whole name rather than a place in a
 * line. That is what retired the leading word-boundary guard the pattern needed to stop
 * `fill` matching inside `autofill` and `stroke` inside `keystroke` — a key is its whole
 * self here, so there is no inside to match in.
 */
const PAINTING_PROPERTY = new RegExp(
  '^(?:' +
    '--[a-zA-Z0-9-]*[cC]olor[a-zA-Z0-9-]*|' +
    '[a-zA-Z-]*[cC]olor|background|background-?[iI]mage|' +
    'border(?:-?(?:top|right|bottom|left|Top|Right|Bottom|Left))?|' +
    'outline|fill|stroke|box-?[sS]hadow|text-?[sS]hadow' +
    ')$',
);

/**
 * The keys of xterm's `ITheme`, which paint but are not CSS.
 *
 * Scoping the rule to painting properties is what stopped it shouting at modules that paint
 * nothing, and the price was every library whose colour keys are its own. That price was
 * abstract while nothing in `apps/web/src` built a terminal. It is not any more: the surface
 * renders, xterm takes concrete colour strings rather than variables — so a value has to be
 * resolved at the call site — and none of its two dozen keys ends in the word the CSS half
 * of this vocabulary matches on. That combination is precisely where the next literal in
 * this app gets written, so the keys are named rather than inferred.
 *
 * Taken from `ITheme` in the pinned `@xterm/xterm` typings, not from memory, and split from
 * a string for the same reason the palette ramps are: a bracketed list of colour names is a
 * bracket span full of colour names, and rule 4a would report this module as its own worst
 * offender. The eight ANSI names are ordinary English words, which is survivable only
 * because they still have to introduce a *value* that names a colour — a key called `red`
 * set to something that does not contain a colour word stays quiet. Set to something that
 * merely mentions one, it does not; that is in the false positives, with a case.
 *
 * `background`, `cursor` and `overviewRulerBorder` overlap the CSS half either exactly or
 * inside a longer word. Whole-name comparison means the longer spellings have to be written
 * out, which is the same trade that keeps `pointBackground` outside the vocabulary.
 */
const TERMINAL_THEME_KEYS = new Set(
  ('foreground cursor cursorAccent selectionBackground selectionForeground ' +
    'selectionInactiveBackground scrollbarSliderBackground scrollbarSliderHoverBackground ' +
    'scrollbarSliderActiveBackground overviewRulerBorder extendedAnsi ' +
    'black red green yellow blue magenta cyan white ' +
    'brightBlack brightRed brightGreen brightYellow brightBlue brightMagenta brightCyan ' +
    'brightWhite').split(' '),
);

/** Every name that can introduce a colour value: CSS's painting properties, and xterm's. */
function paints(name: string): boolean {
  return PAINTING_PROPERTY.test(name) || TERMINAL_THEME_KEYS.has(name);
}

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
 * A `url()` argument, which is a path and not a paint.
 *
 * Both quoted forms and the bare one, because CSS accepts all three. The argument is
 * dropped before the words are counted; everything around it still counts, so a shorthand
 * that names an image *and* a colour is caught on the colour.
 *
 * Only `url()`. Not `var()`: a custom property takes a fallback, and a colour written into
 * one is a colour, which is the whole reason the custom-property spelling is in the
 * vocabulary.
 */
const URL_ARGUMENT = /url\(\s*(?:'[^']*'|"[^"]*"|[^)'"]*)\)/g;

/** Whether a value spells a colour once its image paths are set aside. */
function namesAColour(value: string): boolean {
  const painted = value.toLowerCase().replace(URL_ARGUMENT, ' ');
  for (const word of painted.matchAll(/[a-z]+/g)) {
    if (NAMED_COLOURS.has(word[0])) {
      return true;
    }
  }
  return false;
}

/**
 * How to parse a file, from its extension.
 *
 * `.ts` must not be parsed as TSX: `<T>value` is a type assertion there and JSX here, and
 * getting that backwards turns valid code into a parse error — which loses every site in
 * the file after it, silently, because a guard that finds nothing looks exactly like a file
 * with nothing in it.
 */
function scriptKindFor(file: string): ts.ScriptKind {
  if (file.endsWith('.tsx') || file.endsWith('.jsx')) return ts.ScriptKind.TSX;
  if (file.endsWith('.ts') || file.endsWith('.mts') || file.endsWith('.cts')) {
    return ts.ScriptKind.TS;
  }
  return ts.ScriptKind.JSX;
}

/**
 * The expression inside whatever is wrapped around it.
 *
 * `('x')`, `'x' as string`, `'x' satisfies string`, `<string>'x'` and `'x'!` are all the
 * string `'x'` at run time, and a walk that did not know it would read each of them as
 * "not a literal" and go quiet. These five are every wrapper a parsed tree can hold; a
 * wrapper kind nobody has thought of fails closed, which is to say it reads as an
 * expression the walk does not follow and lands in the last residue bullet.
 */
function unwrap(node: ts.Expression): ts.Expression {
  let current = node;
  while (
    ts.isParenthesizedExpression(current) ||
    ts.isAsExpression(current) ||
    ts.isSatisfiesExpression(current) ||
    ts.isTypeAssertionExpression(current) ||
    ts.isNonNullExpression(current)
  ) {
    current = current.expression;
  }
  return current;
}

/** The cooked text of a string or no-substitution template literal, if that is what it is. */
function literalText(node: ts.Node | undefined): string | undefined {
  if (node === undefined || !ts.isExpression(node)) return undefined;
  const inner = unwrap(node);
  return ts.isStringLiteralLike(inner) ? inner.text : undefined;
}

/**
 * The name a member is declared under, however it is written.
 *
 * `color`, `'color'` and `['color']` are one node kind with three spellings of `name`, and
 * the quote or the bracket between the name and the colon is what used to break the match.
 * A computed name assembled at run time — a template with a substitution — has no name to
 * return, which is the residue entry it belongs to.
 */
function declaredName(name: ts.Node): string | undefined {
  if (ts.isIdentifier(name) || ts.isStringLiteralLike(name)) return name.text;
  if (ts.isComputedPropertyName(name)) return literalText(name.expression);
  return undefined;
}

/**
 * Whether a parameter, a destructured binding or an enum member names something that
 * paints.
 *
 * A binding element carries two names when it renames — `{ color: c }` reads `color` and
 * binds `c` — and a default written on it is the value of both. Either one is enough:
 * `{ color: c = 'red' }` fixes what the property `color` resolves to, and
 * `{ tier: color = 'red' }` fixes what the name `color` holds. The pattern this replaced
 * caught both, because it matched on the equals sign and did not care which name was in
 * front of it.
 */
function bindingPaints(
  node: ts.ParameterDeclaration | ts.BindingElement | ts.EnumMember,
): boolean {
  const bound = declaredName(node.name);
  if (bound !== undefined && paints(bound)) return true;
  if (!ts.isBindingElement(node) || node.propertyName === undefined) return false;
  const source = declaredName(node.propertyName);
  return source !== undefined && paints(source);
}

/**
 * Whether a destructuring pattern binds a painting name at its top level.
 *
 * This is what reaches `const [color, setColor] = useState('red')`, where the colour is in
 * neither name nor a default but in the thing being taken apart. A pattern is the one
 * declaration shape whose value is not written next to the name that receives it.
 *
 * An array pattern is positional and nothing here checks which slot a name sits in, so
 * `const [setColor, color] = useState('red')` reads the same. That is imprecision rather
 * than the which-way-was-it-written asymmetry this rule keeps being held for: both
 * spellings fire, and knowing that the first element is the value and the second the setter
 * is a fact about `useState` and not about syntax.
 */
function patternPaints(pattern: ts.BindingPattern): boolean {
  return pattern.elements.some(
    (element) => ts.isBindingElement(element) && bindingPaints(element),
  );
}

/** The name an assignment writes to, through a member, an index or a bare identifier. */
function assignedName(target: ts.Expression): string | undefined {
  const inner = unwrap(target);
  if (ts.isIdentifier(inner)) return inner.text;
  if (ts.isPropertyAccessExpression(inner)) return inner.name.text;
  if (ts.isElementAccessExpression(inner)) return literalText(inner.argumentExpression);
  return undefined;
}

/**
 * Writing a property through the DOM, where the name is an argument rather than a key.
 *
 * The call has to be named. An earlier rule accepted any call whose first argument was a
 * quoted property name and whose second was a string — a test helper, an analytics event, a
 * two-element lookup table, none of which paints anything — and narrowing it to these four
 * is what closed that. `setAttributeNS` puts the namespace first, so the name is its
 * *second* argument, and the typed-OM map spells the verb on its own; both were lost the
 * first time this was narrowed, because only two names were written down.
 *
 * The receiver is deliberately not checked for the first three: `setProperty` on something
 * that is not a `CSSStyleDeclaration` is not a shape this tree writes, and requiring
 * `el.style.` in front would miss the helper that takes the declaration as a parameter.
 * `set` is the exception — the verb is far too common on its own, so the map has to be named.
 */
function writtenProperty(
  call: ts.CallExpression,
): { readonly name: string; readonly value: ts.Node | undefined } | undefined {
  const callee = unwrap(call.expression);

  let verb: string;
  let receiver: ts.Expression | undefined;
  if (ts.isIdentifier(callee)) {
    verb = callee.text;
  } else if (ts.isPropertyAccessExpression(callee)) {
    verb = callee.name.text;
    receiver = callee.expression;
  } else {
    return undefined;
  }

  let nameAt: number;
  if (verb === 'setProperty' || verb === 'setAttribute') {
    nameAt = 0;
  } else if (verb === 'setAttributeNS') {
    nameAt = 1;
  } else if (
    verb === 'set' &&
    receiver !== undefined &&
    assignedName(receiver) === 'attributeStyleMap'
  ) {
    nameAt = 0;
  } else {
    return undefined;
  }

  const name = literalText(call.arguments[nameAt]);
  return name === undefined ? undefined : { name, value: call.arguments[nameAt + 1] };
}

/** The `??`, `||`, `&&` and `+` of an expression, each of which can hand back an operand. */
const VALUE_OPERATORS = new Set<ts.SyntaxKind>([
  ts.SyntaxKind.QuestionQuestionToken,
  ts.SyntaxKind.BarBarToken,
  ts.SyntaxKind.AmpersandAmpersandToken,
  ts.SyntaxKind.PlusToken,
]);

/**
 * Every string a value expression could hand to the property, collected into `into`.
 *
 * This follows the shapes a value is *made* of and stops at everything else. What it
 * follows is the list below; what it refuses is the point of the exercise, because the
 * refusals are the false positives the character span could not avoid:
 *
 *  - a conditional's **condition** is tested, never painted, so `isShade('red') ? … : …`
 *    contributes nothing and both branches contribute everything. The pattern read the
 *    condition and reported it.
 *  - a comparison hands back a boolean, so `status === 'failed'` contributes nothing
 *    wherever it sits. Only the four operators in {@link VALUE_OPERATORS} pass an operand
 *    through.
 *  - an element access's **index** selects a value, it is not one, so `palette['gold']`
 *    is quiet where the pattern reported the key.
 *  - an object literal is not descended into, because the walk that called this already
 *    visits every property in the file. Descending would report a nested paint twice.
 *
 * Call arguments *are* followed, and that is a deliberate asymmetry: `token('--color-fg',
 * 'red')` is a hardcoded fallback behind a token lookup, which is the live shape in this
 * app and a true positive. It costs the false positive of the same name.
 */
function valueStrings(node: ts.Node | undefined, into: string[]): void {
  if (node === undefined) return;

  if (ts.isStringLiteralLike(node)) {
    into.push(node.text);
  } else if (ts.isTemplateExpression(node)) {
    into.push(node.head.text);
    for (const span of node.templateSpans) {
      valueStrings(span.expression, into);
      into.push(span.literal.text);
    }
  } else if (ts.isConditionalExpression(node)) {
    valueStrings(node.whenTrue, into);
    valueStrings(node.whenFalse, into);
  } else if (ts.isBinaryExpression(node)) {
    if (VALUE_OPERATORS.has(node.operatorToken.kind)) {
      valueStrings(node.left, into);
      valueStrings(node.right, into);
    }
  } else if (ts.isArrayLiteralExpression(node)) {
    for (const element of node.elements) valueStrings(element, into);
  } else if (ts.isCallExpression(node) || ts.isNewExpression(node)) {
    for (const argument of node.arguments ?? []) valueStrings(argument, into);
  } else if (ts.isJsxExpression(node)) {
    valueStrings(node.expression, into);
  } else if (ts.isAwaitExpression(node)) {
    // `await x` hands back whatever `x` produces, so it passes a value through the way a
    // parenthesis does. It cannot invent a literal — a colour behind a real promise is a
    // colour behind a variable, which is the first residue entry — so following it only
    // finds literals that were written down. Found by running this walk and the pattern it
    // replaced over a cross-product of every context, name and value spelling and diffing
    // the answers; it was the last shape the pattern caught and the walk did not, and no
    // reviewer or author had thought of it.
    valueStrings(node.expression, into);
  } else if (ts.isExpression(node)) {
    const inner = unwrap(node);
    if (inner !== node) valueStrings(inner, into);
  }
}

/** The name of a JSX attribute, which may carry a namespace in front of it. */
function attributeName(node: ts.JsxAttribute): string {
  return ts.isIdentifier(node.name) ? node.name.text : node.name.name.text;
}

/** The offending source, on one line and short enough to read in a failure message. */
function excerpt(node: ts.Node, sourceFile: ts.SourceFile): string {
  const written = node.getText(sourceFile).replace(/\s+/g, ' ').trim();
  return written.length > 120 ? `${written.slice(0, 119)}…` : written;
}

/**
 * Every painted site in `source` whose value names a colour, in the order they are written.
 *
 * The file is parsed, never executed, and a parse TypeScript cannot complete still yields a
 * tree — it recovers rather than throwing. A file broken enough for that to lose a site is
 * a file `pnpm typecheck` rejects, so nothing here needs to guess at malformed input.
 */
function findPaintedValues(file: string, source: string): string[] {
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    scriptKindFor(file),
  );

  const found: string[] = [];
  const report = (node: ts.Node, value: ts.Node | undefined): void => {
    const strings: string[] = [];
    valueStrings(value, strings);
    if (strings.some(namesAColour)) found.push(excerpt(node, sourceFile));
  };

  const visit = (node: ts.Node): void => {
    if (ts.isJsxAttribute(node)) {
      if (paints(attributeName(node))) report(node, node.initializer);
    } else if (ts.isPropertyAssignment(node) || ts.isPropertyDeclaration(node)) {
      const name = declaredName(node.name);
      if (name !== undefined && paints(name)) report(node, node.initializer);
    } else if (ts.isVariableDeclaration(node)) {
      if (ts.isIdentifier(node.name)) {
        if (paints(node.name.text)) report(node, node.initializer);
      } else if (patternPaints(node.name)) {
        report(node, node.initializer);
      }
    } else if (
      ts.isParameter(node) ||
      ts.isBindingElement(node) ||
      ts.isEnumMember(node)
    ) {
      if (bindingPaints(node)) report(node, node.initializer);
    } else if (
      ts.isBinaryExpression(node) &&
      node.operatorToken.kind === ts.SyntaxKind.EqualsToken
    ) {
      const name = assignedName(node.left);
      if (name !== undefined && paints(name)) report(node, node.right);
    } else if (ts.isCallExpression(node)) {
      const written = writtenProperty(node);
      if (written !== undefined && paints(written.name)) report(node, written.value);
    }

    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return found;
}

/**
 * Every hardcoded colour in `source`.
 *
 * `currentcolor`, `transparent` and `inherit` are deliberately absent from the named list:
 * all three follow whatever the theme sets, which is the opposite of the problem.
 *
 * `file` decides how the source is parsed and nothing else. It defaults to a `.tsx` name
 * because the shapes this guard exists to catch are written in components; the sweep passes
 * the real path, which is what keeps a `.ts` file's type assertions from being read as JSX.
 */
export function findColourLiterals(
  source: string,
  file = 'inline.tsx',
): readonly ColourLiteral[] {
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
    if (namesAColour(span[0])) {
      found.push({ kind: 'named-colour', text: span[0] });
    }
  }
  for (const site of findPaintedValues(file, source)) {
    found.push({ kind: 'named-colour', text: site });
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
 * its own patterns is a guard that can stop being wired up without anyone noticing (trap 12).
 */
export function scanForColourLiterals(
  files: readonly ScannedFile[],
): readonly ColourViolation[] {
  return files
    .filter((file) => !TOKEN_DEFINITION_MODULES.includes(file.path))
    .flatMap((file) =>
      findColourLiterals(file.source, file.path).map((literal) => ({
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
 * This module is not on the list, and the constraint that keeps it off is worth keeping: a
 * guard that has to exempt itself has stopped being checkable. It is a weaker constraint
 * than it was — rule 4 reads a tree now, so the shapes it hunts for can be written down in
 * a comment, and several are. Rules 1 to 3 still read whole files as text, so a hex, a
 * colour function and a palette class remain unwritable here in any position at all.
 */
export const TOKEN_DEFINITION_MODULES: readonly string[] = ['src/theme/themes.ts'];

/**
 * The one stylesheet `src` owns, which is the Tailwind `@theme` block itself.
 *
 * Its contents cannot be scanned: vitest runs with CSS processing off, so a `?raw` import
 * of a stylesheet comes back empty, and turning it on is a change to the shared root
 * `vitest.config.ts`. The guard covers the same ground a different way — it asserts this is
 * the only stylesheet *under `src`*, so a second one cannot appear without failing here and
 * forcing the colours in it to be reviewed.
 *
 * The qualifier is the point, and it used to be missing: the sentence here claimed the only
 * stylesheet in `apps/web`, which is a claim about the bundle, and the check behind it is a
 * glob rooted at this file. A stylesheet a dependency ships and a module imports by package
 * name is outside that glob and inside the built CSS. There is one today — reading the built
 * output rather than the source is what found it — and it carries a hex background, a hex
 * foreground, a colour function and a data URI painting a path, none of which any rule here
 * has ever seen. {@link DEPENDENCY_STYLESHEETS} is the honest half of the claim.
 */
export const TOKEN_DEFINITION_STYLESHEETS: readonly string[] = ['src/index.css'];

/**
 * Every dependency stylesheet a TypeScript module pulls in for its side effect, by the
 * specifier that pulls it.
 *
 * Read the qualifier as narrowly as it is written, because the previous sentence in this
 * file that did not carry one is what this list exists to answer. It covers the one shape
 * the sweep can see: a bare `import` of a `.css` specifier that is not a path, in a file
 * the glob already reads. It does not cover a binding import, a dynamic one, or — the live
 * gap — an `@import` inside `index.css`, which is how Tailwind itself and five font
 * stylesheets arrive. Those are visible in the built CSS as the generated `--tw-*` colour
 * fallbacks and nowhere in the source this module reads. Widening the pattern would not
 * reach them: the stylesheet's *contents* are unreadable here for the reason
 * {@link TOKEN_DEFINITION_STYLESHEETS} gives, so that half needs the built output and a
 * person looking at it.
 *
 * Not an allowlist of things that are *fine*, either: the one entry paints pixels this
 * guard cannot check and has no token in it. It is a list of what is known, so the set
 * cannot grow by accident down the one road it does watch. A dependency stylesheet arrives
 * in one line of somebody else's module, bundles into the built CSS, and is invisible to
 * every rule above — the same failure mode as the accent module being invisible to the
 * property prefix, arriving from outside the package instead of from inside it.
 *
 * Adding an entry is the review: it says somebody looked at what that stylesheet paints and
 * at whether the theme can reach it.
 *
 * Which means this list fails in *other people's* diffs, by design, and the entry it holds
 * today names a file another module owns. Whoever adds the next side-effect import of a
 * dependency stylesheet meets this as a red test in their own PR, so the assertion carries
 * a message saying what it wants rather than only what it found.
 */
export const DEPENDENCY_STYLESHEETS: readonly string[] = ['@xterm/xterm/css/xterm.css'];

/**
 * A stylesheet imported for its side effect by package name rather than by path.
 *
 * Relative specifiers are excluded because the glob already sees those. A binding import
 * and a dynamic one are excluded because neither is how a stylesheet is pulled in for its
 * side effect, and pretending to cover them would be the overclaim again; both are in the
 * residue. This module's own source cannot match the pattern either: it wants literal
 * whitespace where the source has the two characters that stand for it.
 */
const DEPENDENCY_STYLESHEET_IMPORT = /import\s+['"]((?!\.)[^'"]+\.css)['"]/g;

/** Which dependency stylesheets `files` pull in, deduplicated and ordered. */
export function findDependencyStylesheets(
  files: readonly ScannedFile[],
): readonly string[] {
  const found = new Set<string>();
  for (const file of files) {
    for (const match of file.source.matchAll(DEPENDENCY_STYLESHEET_IMPORT)) {
      found.add(match[1] ?? '');
    }
  }
  return [...found].sort();
}
