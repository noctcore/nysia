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
 *  - a string literal that a name known to paint has introduced, which is what an inline
 *    style is, and — since the terminal renders for real — what an xterm theme is too.
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
 * Only half the JSX case survived that, and the half that did was the half the tests
 * exercised. A prop written as a plain string was caught; the same prop written as an
 * expression container — which is how anything conditional has to be written, and so how
 * the shape this rule most needs to catch actually appears — was not, because the brace
 * is what bounds the span and it sat between the separator and the value. A status dot
 * whose colour prop is a ternary is the default idiom, and it shipped with every gate
 * green under a sentence that said JSX attributes were covered. The brace that opens a
 * container is admitted at the separator now, where it touches the equals sign; the one
 * that opens an object still bounds the span, and the closing one still stops it, so a
 * prop cannot reach the prop after it.
 *
 * That gap had a second life. Naming the ANSI colour words as introducers meant that
 * inside a braced ternary the quoted first branch read as a key introducing the second,
 * so the rule fired when the first branch was one of eight words and stayed quiet
 * otherwise — the same which-way-was-it-written asymmetry the equality separator had,
 * reappearing in the one shape no case covered. Admitting the brace removes that one:
 * both spellings match on the container rather than on the accident.
 *
 * It did not remove the general fault, and the round that said it had was reading its own
 * fix rather than the rule. A ternary was still read by whichever literal the greedy
 * quantifier reached last, so which branch held the colour still decided the answer — a
 * third spelling of the same asymmetry, in the container that had just been admitted and
 * in the object literal that had always been there. That one is closed where the span is
 * defined, by looking at every literal instead of the last.
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
 *  - a colour that reaches CSS through a variable rather than a literal: the value is a
 *    name at the point where the rule looks, and what it holds is decided somewhere else,
 *    possibly in another module. Nothing short of types can follow that, and the entry is
 *    a limit rather than a bug — the literal itself is still caught wherever it is written.
 *  - a colour name in a string that nothing in the introducer list introduces. Same limit,
 *    the other way round: the word is there but nothing says it paints. Loosening this is
 *    what the rule was narrowed away from, because it made the guard shout at modules that
 *    paint nothing.
 *  - an expression between the property and the literal that contains a semicolon, a comma
 *    or a brace, or that runs past the length cap. Those are what stop the span crossing
 *    out of the value it belongs to, and the price is a call with more than one argument.
 *    The brace that opens a JSX expression container is the exception and is admitted at
 *    the separator, because it is punctuation the language requires rather than a sign the
 *    value has ended; every brace after that still bounds the span.
 *  - a painting property the list does not name. It is a useful subset of CSS, not CSS —
 *    `text-decoration`, `column-rule`, `text-emphasis`, the `border-inline` and
 *    `border-block` families, and `filter` or `backdrop-filter` carrying a drop shadow are
 *    all absent. The sentence here used to say it covered CSS, which was simply false.
 *  - a colour key belonging to a library the list does not name. xterm's are named now,
 *    because that terminal is in this app and its keys are where the next literal would be
 *    written; a chart config's, or any other dependency's, are not. A key that merely
 *    *contains* one of the listed words is outside it too, because the boundary guard that
 *    stops `fill` matching inside `autofill` is the same guard and the same trade.
 *  - bare CSS text carried inside a string or a template — a `cssText` assignment, a
 *    tagged `css` template, a `style` attribute inside a markup string. In all three the
 *    property that introduces the colour is *inside* the literal and the colour after it is
 *    bare, so there is no quoted value left for the rule to read. Reading one means parsing
 *    the string's contents as CSS, which is a parser and not a wider pattern.
 *  - a custom property whose name is computed, which is to say a template-literal key.
 *    There is no name in the source for the pattern to match.
 *  - a setter the list above does not name, and a name and value written as a tuple for one
 *    of the ones it does to consume later. The comma separator is spelled as part of a
 *    named call now; before it was, both of those were caught, but by the accident that
 *    caught every unrelated two-argument call with a property name in front of a string.
 *    Four call shapes are named — `setProperty`, `setAttribute`, the namespaced setter whose
 *    first argument is skipped, and the typed-OM map's `set`; a project's own wrapper around
 *    any of them is the shape most likely to land here, and it is indistinguishable from the
 *    false positive that accident was — same two arguments, different intent, nothing in
 *    the text to tell them apart.
 *  - a colour carried inside a `url()`, which is set aside as a path before the words are
 *    counted. A data URI can carry a whole stylesheet, so this is the bare-CSS entry above
 *    arriving through a different door. What it paid for was closing the loudest false
 *    positive the rule had, and a percent-encoded stylesheet is not a shape anything in
 *    this tree writes. A *hash* encoded that way is the one miss with no backstop at all,
 *    which is the sentence below rather than this bullet.
 *
 * The hex and colour-function rules, which do scan whole files, are the backstop for nearly
 * every one of them, listed or not — whatever shape hides a colour from rule 4, a hash or a
 * function written in the source text is still read. What they do not read is a colour the
 * source does not spell that way, which is to say one behind an encoding. A percent-encoded
 * hash inside a data URI is the live example and the one miss with nothing underneath it:
 * the encoding hides it from the hex rule, and the bullet directly above puts the same value
 * out of the named rule's reach. This sentence said only a *named* colour could hide, and
 * the bullet it sat under had already made that false — which is the failure this file keeps
 * having, a rule widening and the paragraph explaining it staying where it was.
 *
 * In the other direction, the rule has false positives, and they are tracked separately
 * because they are loud: one fails the sweep and gets looked at, where a miss ships a pixel
 * in silence. That is why the trade above usually runs towards catching too much — but not
 * always, because a guard that cries wolf is one somebody eventually switches off:
 *
 *  - a value that merely contains a colour word. The url form is closed, since a path is
 *    the one place a colour word turns up in a value often enough to be worth knowing about.
 *    A token whose own name spells one still fires, and that is the shape most likely to.
 *  - a table keyed by colour name whose values are prose — a label map, most plausibly —
 *    which fires once per entry. The ANSI words are introducers because xterm's theme keys
 *    are spelled that way, and a key called `red` set to a sentence containing the word is
 *    indistinguishable from one set to a colour.
 *  - a quoted colour name followed by a colon, anywhere, with no painting property in front
 *    of it at all: the name reads as a quoted key and whatever literal follows reads as its
 *    value. A ternary between two colour names is one spelling of it, and only when the
 *    *first* branch is one of the introducer words — a ternary headed by any other colour
 *    name is quiet, which is the give-away that this is the key form and not a branch form.
 *    A `switch` case returning a display string is another, and more likely to be written.
 *    Inside a JSX container the same reading used to land on the right answer for the wrong
 *    reason; the brace fix stopped the rule depending on that, and this is what is left.
 *  - a comparison operand that spells a colour, since every literal in the span is now read
 *    and nothing in the text distinguishes an operand from a branch. Half of this was here
 *    before: when the operand happened to be the last literal in range it was taken as the
 *    value outright. Telling them apart is a question about syntax, which is the argument
 *    at the end of this comment.
 *  - a statement that ends without a semicolon followed by an unrelated string on the next
 *    line, since the span crosses line ends and only punctuation stops it. This tree is
 *    semicolon-terminated throughout and nothing enforces that, so it is a live trap rather
 *    than a theoretical one.
 *  - a comment that writes a property, a separator and a quoted colour — a line describing
 *    what paints rather than painting. The rule reads text and not syntax, so the two are
 *    the same thing to it. Telling them apart means knowing where the comments are, and
 *    working that out by pattern is how a scanner comes to mistake a regex literal for a
 *    comment — this module has one carrying a backtick — and fall silent over everything
 *    after it. That failure is quiet and this one is not, so this one stays. Marking a
 *    property up in prose is safe; putting a separator after it is what fires. *
 * ## What kind of list that is — read this before adding to it
 *
 * Nearly every entry above is *positional*. Not "this colour is hard to recognise" — the
 * words are a fixed list and recognising one is trivial — but "the rule did not know that
 * *this place* in the text was where the value goes". A property flush against its colon,
 * then one behind a ternary, then a quoted key, then a custom property, then a JSX
 * expression container, then whichever branch a greedy quantifier reached last: five rounds
 * of review, five new positions, each closed by spelling that one out. The false positives
 * are the same fact from the other side — an operand, a branch, a key and a comment are
 * four different things that look identical to a pattern over characters.
 *
 * So: that list is **a property of this implementation, not of the problem**. Whether a
 * string is the value of a painting property is a question about syntax, and a regular
 * expression cannot answer a question about syntax — it can be made right about a position
 * someone has already thought of, never about position itself, because it has no notion of
 * one. Nothing in the list is there because finding hardcoded colours is inherently hard.
 *
 * Which is why the next change to this rule should not be another entry. Issue #45 rewrites
 * it as a walk over the TypeScript syntax tree — JSX attributes, object-literal properties
 * and assignment targets whose key paints, then look at the value — and the positional
 * entries above do not get better documentation there, they stop existing. No spans, no
 * greedy quantifiers, no ordering accidents, and no sixth position waiting to be found. The
 * two entries that would survive are the genuinely non-syntactic ones: a colour reached
 * through a variable, which needs types, and bare CSS inside a string, which needs a CSS
 * parser rather than a TypeScript one.
 *
 * The precedent is next door. The architecture rules hand-rolled a lexer for the same class
 * of question, hit the same run of positional misses, and deleted it for a syntax-tree walk
 * this round; their rules module lost around six hundred lines in the exchange. If you are
 * reading this list in six months and it has grown again, that is the fix — not another
 * bullet.

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
 * The keys of xterm's `ITheme`, which paint but are not CSS.
 *
 * Scoping the rule to painting properties is what stopped it shouting at modules that paint
 * nothing, and the price was every library whose colour keys are its own. That price was
 * abstract while nothing in `apps/web/src` built a terminal. It is not any more: the surface
 * renders, xterm takes concrete colour strings rather than variables — so a value has to be
 * resolved at the call site — and none of its two dozen keys ends in the word the CSS half
 * of this list matches on. That combination is precisely where the next literal in this app
 * gets written, so the keys are named rather than inferred.
 *
 * Taken from `ITheme` in the pinned `@xterm/xterm` typings, not from memory, and split from
 * a string for the same reason the palette ramps are: a bracketed list of colour names is a
 * bracket span full of colour names, and rule 4 would report this module as its own worst
 * offender. The eight ANSI names are ordinary English words, which is survivable only
 * because they still have to introduce a *value* that names a colour — a key called `red`
 * set to something that does not contain a colour word stays quiet. Set to something that
 * merely mentions one, it does not; that is in the false positives, with a case.
 *
 * `background`, `cursor` and `overviewRulerBorder` overlap the CSS list either exactly or
 * inside a longer word; the boundary guard means the longer spellings have to be written
 * out, which is the same trade that keeps `fill` from matching in `autofill`.
 */
const TERMINAL_THEME_KEYS =
  ('foreground cursor cursorAccent selectionBackground selectionForeground ' +
    'selectionInactiveBackground scrollbarSliderBackground scrollbarSliderHoverBackground ' +
    'scrollbarSliderActiveBackground overviewRulerBorder extendedAnsi ' +
    'black red green yellow blue magenta cyan white ' +
    'brightBlack brightRed brightGreen brightYellow brightBlue brightMagenta brightCyan ' +
    'brightWhite').split(' ');

/** Every name that can introduce a colour value: CSS's painting properties and xterm's. */
const COLOUR_INTRODUCER = `${PAINTING_PROPERTY}|${TERMINAL_THEME_KEYS.join('|')}`;

/**
 * Writing a property through the DOM, where the name is an argument and the separator is
 * the comma between the two.
 *
 * The call has to be named. The comma form used to accept any call whose first argument was
 * a quoted property name and whose second was a string — a test helper, an analytics event,
 * a two-element lookup table, none of which paints anything — and that was a false positive
 * inherited from the version before the rule narrowed. Naming the DOM methods that actually
 * take this shape closes it without giving up the case the comma exists for.
 *
 * Naming two of them was not enough, and the first version of this did. `setAttributeNS`
 * puts the namespace first, so the property is its *second* argument, and the typed-OM
 * `attributeStyleMap` spells the verb on its own — both were caught before the narrowing,
 * by the accident it removed, and went quiet with it. Losing coverage while closing a false
 * positive is the trade this rule is least allowed to make silently, so the namespaced form
 * skips one argument and the map is named alongside the other two. The alternatives are
 * written apart rather than as one optional group, so there is no argument for the engine
 * to try both ways round.
 *
 * The price is any setter this list does not name — a project's own `applyStyle` wrapper,
 * most likely, which is the same shape as the false positive above with a different name
 * on it — and a name and value written as a tuple for one of these to consume later. Both
 * are in the residue.
 */
const SETTER_CALL =
  `(?<![\\w-])(?:set(?:Property|Attribute)|attributeStyleMap\\s*\\.\\s*set)\\s*\\(\\s*` +
  `|(?<![\\w-])setAttributeNS\\s*\\(\\s*(?:'[^'\\n]*'|"[^"\\n]*"|[^,()\\n]{0,60})\\s*,\\s*`;
const SET_PROPERTY_CALL = `(?:${SETTER_CALL})['"\`](?:${COLOUR_INTRODUCER})['"\`]\\s*,`;

/**
 * The property and its separator.
 *
 * The leading guard is what stops the list matching inside a longer word — without it
 * `stroke` matched in `keystroke` and `fill` in `autofill` or `refill`, so the rule that
 * had just stopped crying wolf on bare strings started crying wolf on identifiers instead.
 * The other two rules have had that guard from the start.
 *
 * Three shapes: bare property then colon or equals, which covers an object literal, a JSX
 * attribute and an assignment; quoted property then colon or equals, which covers a quoted
 * key; and the DOM call above.
 *
 * One equals sign, not two. A comparison reads a colour rather than writing one, and the
 * rule used to match the first of the three characters in a strict one — so the positive
 * form fired and the negated form, whose first character is not an equals sign, did not.
 * An asymmetry like that is worse than either answer on its own, because which way a
 * condition happens to be written is not a property of the code's correctness. It cost an
 * accidental catch: a ternary headed by a comparison against a name that was itself in the
 * list used to be found, and that never generalised — the negated spelling of the same
 * condition was always missed, and a colour reached through a variable is in the residue.
 */
const PROPERTY_INTRO =
  `(?:(?<![\\w-])(?:${COLOUR_INTRODUCER})(?:\\s*:|\\s*=(?!=)\\{?|['"\`]\\s*(?::|=(?!=)))|${SET_PROPERTY_CALL})`;

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
 * wrong but backwards. All three of them, which took two goes: the chunk alternation was
 * written for the two ordinary quote characters and the class beside it excluded the third,
 * so a span could cross a string and not a template. That left the asymmetry alive in the
 * one spelling this codebase is most likely to use it in — the colour vocabulary here is
 * custom properties, so an interpolated token on one branch against a hardcoded fallback on
 * the other is the natural way to write it, and whoever wrote the branches the other way
 * round got the red gate for the same code. A quoted string in the expression, which is what a comparison like
 * `status === 'failed' ? …` is made of, was taken as the value: the rule read the first
 * literal after the separator, found no colour word in it, and skipped past the real one.
 * So the span swallows a complete quoted string as a unit. The alternation is ordered
 * quote-first so a literal is consumed whole rather than a character at a time; both
 * branches are anchored on different characters, so there is no ambiguity for the engine to
 * backtrack through.
 *
 * The span is then handed to the caller entire, and **every** literal in it is checked.
 * That is the correction to the version before this one, which took the greedy quantifier's
 * answer — the *last* literal in range — and called it the value. In a ternary that is the
 * right one only half the time. A fill written as a colour on one condition and `none` on
 * the other, which is SVG's own spelling for not painting, was silent; the same fill with
 * the branches swapped was loud. `inherit`, `transparent`, `currentcolor` and the empty
 * string all behaved the same way, and they are precisely the words the named set leaves
 * out on purpose, so they are precisely what a component writes as the other branch. It
 * was the third which-way-did-you-write-it asymmetry this rule has had, and the first two
 * were closed by spelling one more position correctly, which is why this one is not.
 *
 * Reading every literal is also what closed the escaped-quote entry that sat in the residue
 * for three rounds: the split at an escaped quote is still wrong, but no colour falls in
 * the half nobody looked at any more. It costs a false positive, below — an operand and a
 * branch are both literals in the same span, and which is which is a question about syntax.
 */
const QUOTED_CHUNK = `'[^'\\n]*'|"[^"\\n]*"|\`[^\`]*\``;
const BEFORE_VALUE = `(?:${QUOTED_CHUNK}|[^'"\`;,{}]){0,120}`;

/**
 * One pattern per quote character rather than one with a backreference, so a value may
 * contain the other two. That is what catches a shorthand carrying a quoted url, and it is
 * the only way the template pattern can see through an interpolation that contains quotes.
 */
const STYLE_VALUES: readonly RegExp[] = [
  new RegExp(`${PROPERTY_INTRO}(${BEFORE_VALUE}'[^'\\n]*')`, 'g'),
  new RegExp(`${PROPERTY_INTRO}(${BEFORE_VALUE}"[^"\\n]*")`, 'g'),
  new RegExp(`${PROPERTY_INTRO}(${BEFORE_VALUE}\`[^\`]*\`)`, 'g'),
];

/**
 * Every quoted literal inside the matched span, in the order they were written.
 *
 * The same alternation the span itself uses, so a literal is taken whole and the same way:
 * leftmost wins, which is what stops the inner quotes of a value being read as delimiters
 * of their own.
 */
const CANDIDATE_VALUE = /'[^'\n]*'|"[^"\n]*"|`[^`]*`/g;
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
 * Both quoted forms and the bare one, because CSS accepts all three and the value the rule
 * captured may already have had one quote character spent on its own delimiters. The
 * argument is dropped before the words are counted; everything around it still counts, so a
 * shorthand that names an image *and* a colour is caught on the colour.
 *
 * Only `url()`. Not `var()`: a custom property takes a fallback, and a colour written into
 * one is a colour, which is the whole reason the custom-property spelling is in the intro.
 */
const URL_ARGUMENT = /url\(\s*(?:'[^']*'|"[^"]*"|[^)'"]*)\)/g;

/** Whether any literal the property could be set to spells a colour. */
function someCandidateNamesAColour(span: string): boolean {
  for (const candidate of span.matchAll(CANDIDATE_VALUE)) {
    if (namesAColour(candidate[0].slice(1, -1))) {
      return true;
    }
  }
  return false;
}

/** Whether a captured value spells a colour once its image paths are set aside. */
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
    if (namesAColour(span[0])) {
      found.push({ kind: 'named-colour', text: span[0] });
    }
  }
  /*
   * The three patterns read the same properties, one per quote character, so a value whose
   * branches use different quotes is matched by more than one of them. They start at the
   * same index because they start at the same property, which is what makes them one
   * violation rather than two; the longest match is kept because it is the one that reached
   * furthest through the value.
   */
  const byProperty = new Map<number, string>();
  for (const pattern of STYLE_VALUES) {
    for (const match of source.matchAll(pattern)) {
      if (!someCandidateNamesAColour(match[1] ?? '')) {
        continue;
      }
      const seen = byProperty.get(match.index);
      if (seen === undefined || match[0].length > seen.length) {
        byProperty.set(match.index, match[0]);
      }
    }
  }
  for (const [, text] of [...byProperty].sort(([a], [b]) => a - b)) {
    found.push({ kind: 'named-colour', text });
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
