import ts from 'typescript';
import { describe, expect, it } from 'vitest';

import {
  INITIALIZED_DECLARATIONS,
  INITIALIZERS_HANDLED_BY_HAND,
  DEPENDENCY_STYLESHEETS,
  TOKEN_DEFINITION_MODULES,
  TOKEN_DEFINITION_STYLESHEETS,
  findColourLiterals,
  findDependencyStylesheets,
  scanForColourLiterals,
  type ScannedFile,
} from './colourGuard';

/*
 * `import.meta.glob` rather than `node:fs`: `apps/web` is a browser bundle and ESLint bans
 * node builtins across the whole package, tests included. Vite's glob is a compile-time
 * transform, so it works in the node-only vitest project (D-18) without a DOM.
 */
const modules = import.meta.glob('../**/*.{ts,tsx}', {
  query: '?raw',
  import: 'default',
  eager: true,
});
const stylesheets = import.meta.glob('../**/*.css');

/**
 * Repo-relative, POSIX separators, so a failure message is clickable on both runners.
 *
 * Vite reports a sibling as `./name` and everything else relative to this file, which
 * lives in `src/theme` — hence the two rewrites.
 */
function normalize(globPath: string): string {
  return `src/${globPath.replace(/^\.\.\//, '').replace(/^\.\//, 'theme/')}`;
}

const scanned: readonly ScannedFile[] = Object.entries(modules)
  .map(([path, source]) => ({ path: normalize(path), source: String(source) }))
  .filter(({ path }) => !path.endsWith('.test.ts') && !path.startsWith('src/generated/'));

/** A backtick, so a fixture can carry one without ending the template it is written in. */
const TICK = '`';

/**
 * The shapes, written only here — the guard module writes down no colour of its own.
 *
 * Every snippet is valid TSX, because the sweep appends it to a real module as *code*. It
 * used to be appended as a comment, which worked while the rule read characters and stopped
 * working the moment it read a syntax tree: a comment is trivia, so the whole list would
 * have gone quiet at once and the sweep would still have passed, having proved nothing. The
 * comment form is now a case of its own, under "what the parser closed".
 *
 * More entries than kinds on purpose: the assertion below counts the four kinds a scan can
 * report, and the extra snippets are the specific shapes each round of this review found
 * missing, kept so the real sweep exercises them rather than only the fixture tests.
 */
const OFFENDERS = [
  { kind: 'hex', snippet: `export const H = () => <i style={{ color: '#ff0000' }} />;` },
  {
    kind: 'function',
    snippet: `export const F = () => <i style={{ color: 'rgb(255 0 0)' }} />;`,
  },
  { kind: 'palette-class', snippet: `export const P = () => <i className="text-red-500" />;` },
  { kind: 'named-colour', snippet: `export const A = () => <i className="[color:red]" />;` },
  { kind: 'named-colour', snippet: `export const I = () => <i style={{ color: 'red' }} />;` },
  // The concrete failure a colon-flush pattern let through: on main this injected line
  // turned the sweep red, and it would not have before that round.
  {
    kind: 'named-colour',
    snippet: `export const C = () => <i style={{ color: failed ? 'red' : undefined }} />;`,
  },
  // The custom-property case, which hid the one module whose job is writing token values.
  {
    kind: 'named-colour',
    snippet: `document.body.style.setProperty('--color-acc', 'red');`,
  },
  // The comparison case. `ProjectsSidebar` already writes a string-equality ternary, so
  // this shape is one edit away from being real rather than hypothetical.
  {
    kind: 'named-colour',
    snippet:
      `export const S = () => <i style={{ color: status === 'failed' ? 'red' : undefined }} />;`,
  },
  // The xterm theme key. Not a painting property, not CSS, and the one place in this app
  // where a colour string still has to be written out rather than referenced.
  {
    kind: 'named-colour',
    snippet: `export const term = new Terminal({ theme: { foreground: 'white' } });`,
  },
  // The braced JSX prop. The first branch is deliberately not one of the ANSI words, so
  // this proves the container and not the quoted-key accident that used to stand in for it.
  {
    kind: 'named-colour',
    snippet: `export const D = () => <Dot stroke={active ? 'navy' : undefined} />;`,
  },
] as const;

/*
 * The declaration family, asked of TypeScript rather than listed from memory.
 *
 * Three rounds of review each found a declaration kind the rule did not know about, and
 * every one of them was "a declaration binds a name to an initializer" wearing a different
 * node kind. The fix was to stop hand-listing the kinds and call TypeScript's own predicate;
 * this is what stops that fix quietly rotting when the compiler is upgraded.
 *
 * It is deliberately a check and not an assertion about shapes. A test that parses one
 * example per kind proves only the kinds somebody thought to write down, which is the
 * failure it is meant to prevent.
 */
describe('the declaration family', () => {
  /**
   * Every `ts.SyntaxKind`, as a name and a number.
   *
   * The enum carries reverse mappings and a set of `First…`/`Last…` aliases that share
   * numbers with real kinds, so the numbers are taken from the numeric half and named back
   * through the enum — which is exactly how the guard would see them.
   */
  const everyKind = Object.values(ts.SyntaxKind).filter(
    (value): value is ts.SyntaxKind => typeof value === 'number',
  );

  it('enumerates a whole compiler worth of kinds, so the sweep is not vacuous', () => {
    expect(everyKind.length).toBeGreaterThan(300);
  });

  it('reads exactly the declaration kinds TypeScript says have an initializer', () => {
    // `hasOnlyExpressionInitializer` switches on `node.kind` alone, so a bare kind is enough
    // to ask it about every member of the enum rather than about the handful of shapes
    // anybody happened to write a fixture for. The cast says that out loud: this is asking
    // the predicate a question about a kind, not handing it a real node.
    const accepted = everyKind
      .filter((kind) => ts.hasOnlyExpressionInitializer({ kind } as unknown as ts.Node))
      .map((kind) => ts.SyntaxKind[kind])
      .sort();

    expect(accepted).toEqual([...INITIALIZED_DECLARATIONS].sort());
  });

  it('names every node that carries an initializer the predicate does not accept', () => {
    // The complement, which is what makes the pair a closed claim rather than half of one.
    // These are the interfaces in TypeScript's public typings with an `initializer` or
    // `objectAssignmentInitializer` field, minus the ones above. Each is either handled at
    // its own site or explained in the residue; the guard's comment says which and why.
    for (const kind of INITIALIZERS_HANDLED_BY_HAND) {
      expect(INITIALIZED_DECLARATIONS, kind).not.toContain(kind);
      expect(ts.SyntaxKind[kind as keyof typeof ts.SyntaxKind], kind).toBeTypeOf('number');
    }
    expect(INITIALIZERS_HANDLED_BY_HAND).toHaveLength(5);
  });

  it('reads a declaration kind through the predicate and not through a list', () => {
    // The behavioural half: one shape per kind the predicate accepts, so the branch that
    // calls it is exercised for the whole family rather than asserted about.
    for (const [kind, source] of [
      ['VariableDeclaration', `const color = 'red';`],
      ['Parameter', `function paint(fill = 'red') {}`],
      ['BindingElement', `const { color = 'red' } = props;`],
      ['PropertyDeclaration', `class A { color = 'red'; }`],
      ['PropertyAssignment', `const s = { color: 'red' };`],
      ['EnumMember', `enum Swatch { color = 'red' }`],
    ] as const) {
      expect(INITIALIZED_DECLARATIONS, kind).toContain(kind);
      expect(findColourLiterals(source).map((c) => c.kind), kind).toEqual(['named-colour']);
    }
  });
});

describe('the three rules that read whole files as text', () => {
  it('finds a hex colour in every length CSS accepts', () => {
    expect(findColourLiterals('color:#abc').map((c) => c.text)).toEqual(['#abc']);
    expect(findColourLiterals('background:#f2b35b;border:#2A3140').map((c) => c.text)).toEqual(
      ['#f2b35b', '#2A3140'],
    );
    expect(findColourLiterals('outline:#f2b35b24').map((c) => c.text)).toEqual(['#f2b35b24']);
  });

  it('finds every colour function, not just the ones in the token tables', () => {
    for (const fn of ['rgb', 'rgba', 'hsl', 'hsla', 'hwb', 'lab', 'lch', 'oklab', 'oklch']) {
      expect(findColourLiterals(`color: ${fn}(1 2 3)`).map((c) => c.kind), fn).toEqual([
        'function',
      ]);
    }
    expect(findColourLiterals('color-mix(in oklch, a, b)').map((c) => c.kind)).toEqual([
      'function',
    ]);
  });

  it('finds a palette utility under any colour property, with a modifier or without', () => {
    for (const cls of ['text-white', 'bg-red-500', 'border-slate-200/50', 'ring-sky-400']) {
      expect(findColourLiterals(`className="${cls}"`).map((c) => c.kind), cls).toEqual([
        'palette-class',
      ]);
    }
  });

  it('finds a named colour inside an arbitrary value', () => {
    expect(findColourLiterals('className="[color:red]"').map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(findColourLiterals('className="bg-[rebeccapurple]"').map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
  });

  it('reads a hex through a shape the syntax rule cannot see, which is why they are three', () => {
    // The backstop claim, made concrete: a colour inside bare CSS text is invisible to the
    // tree — the property that introduces it is *inside* the literal — and the hex rule
    // reads it anyway, because it reads characters and does not care where they sit.
    expect(findColourLiterals(`el.style.cssText = 'color: #ff0000';`).map((c) => c.kind)).toEqual(
      ['hex'],
    );
  });
});

describe('the rule that reads a syntax tree', () => {
  it('finds a named colour in an inline style, the same as hex and a function', () => {
    // The gap this closes: the bracket-only rule let an inline style naming a colour ship
    // with every gate green, while the same style naming a hex was caught.
    for (const style of [
      `<div style={{ color: 'red' }} />`,
      `<div style={{ background: "rebeccapurple" }} />`,
      `<div style={{ borderColor: 'DarkSlateGray' }} />`,
    ]) {
      expect(findColourLiterals(style).map((c) => c.kind), style).toEqual(['named-colour']);
    }
  });

  it('finds a colour among the other tokens of a shorthand', () => {
    // An early rule saw only a whole one-word string, so a shorthand naming a colour
    // alongside a width and a style went straight through.
    expect(
      findColourLiterals(`<div style={{ border: '1px solid red' }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`<div style={{ boxShadow: "0 0 8px rgba red" }} />`).map((c) => c.kind),
    ).toContain('named-colour');
  });

  it('finds a colour in a template literal, the same as in a quoted string', () => {
    // Backticks were excluded while any one-word string counted, because a doc comment
    // marks up code with them. A tree does not have that problem: a template is a literal
    // node and a doc comment is trivia.
    expect(
      findColourLiterals(`<div style={{ color: ${TICK}red${TICK} }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('finds a colour behind an expression, not only one flush against the colon', () => {
    // The ordinary React conditional style, and the shape that a colon-flush pattern
    // silently stopped seeing — a component writing one of these went red on one round and
    // stayed green on the next.
    for (const conditional of [
      `<div style={{ color: active ? 'red' : 'gray' }} />`,
      `<div style={{ color: failed ? "red" : undefined }} />`,
      `<div style={{ backgroundColor: pick(state) ?? 'navy' }} />`,
    ]) {
      expect(findColourLiterals(conditional).map((c) => c.kind), conditional).toContain(
        'named-colour',
      );
    }
  });

  it('reads the branches of a comparison and not the thing being compared', () => {
    // A comparison is what a conditional style is usually written around, and a span of
    // characters could not tell its operand from its result. The tree can: the condition is
    // tested, the branches are painted, and only the branches are read.
    for (const compared of [
      `<div style={{ color: status === 'failed' ? 'red' : undefined }} />`,
      `<div style={{ background: kind === "agent" ? "navy" : undefined }} />`,
      `<div style={{ color: mode === 'dark' ? theme.a : 'gray' }} />`,
    ]) {
      expect(findColourLiterals(compared).map((c) => c.kind), compared).toContain(
        'named-colour',
      );
    }
  });

  it('still ignores a comparison whose branches are not colours', () => {
    expect(findColourLiterals(`<div style={{ color: mode === 'dark' ? a : b }} />`)).toEqual([]);
  });

  it('finds a colour inside a template whose interpolation carries quotes', () => {
    const shorthand = `<div style={{ border: ${TICK}1px solid \${on ? 'red' : 'gray'}${TICK} }} />`;
    expect(findColourLiterals(shorthand).map((c) => c.kind)).toContain('named-colour');
  });

  it('reads the fixed text of a template as well as its substitutions', () => {
    // A template is text with holes in it, and the colour is as likely to be in the text as
    // in a hole — a shorthand interpolating a width around a fixed colour is the ordinary
    // way this gets written. Both ends of the split are read: what comes before the first
    // hole, and what comes after each one.
    expect(
      findColourLiterals(
        `<div style={{ background: ${TICK}red url(\${path})${TICK} }} />`,
      ).map((c) => c.kind),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(
        `<div style={{ border: ${TICK}\${width}px solid red${TICK} }} />`,
      ).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('finds a colour on a JSX attribute, which separates with an equals sign', () => {
    for (const attribute of [
      `<path fill="red" />`,
      `<path stroke="navy" />`,
      `<Dot color="red" />`,
      // A namespaced attribute is named by its local part, which is the shape SVG's
      // `xlink:` family takes. Nothing here paints through one today; reading the name
      // rather than the whole token is one branch, and this is what pins it.
      `<path xlink:fill="red" />`,
    ]) {
      expect(findColourLiterals(attribute).map((c) => c.kind), attribute).toEqual([
        'named-colour',
      ]);
    }
  });

  it('reads both sides of the operators that hand back an operand', () => {
    // A fallback, a guard and a concatenation: each can be the value, so each side of each
    // is read. A comparison is not on that list, which is what keeps an operand out of the
    // answer wherever it sits — the case for that is under "what the parser closed".
    for (const operated of [
      `<div style={{ color: chosen || 'red' }} />`,
      `<div style={{ color: on && 'red' }} />`,
      `<div style={{ border: width + 'px solid red' }} />`,
    ]) {
      expect(findColourLiterals(operated).map((c) => c.kind), operated).toEqual([
        'named-colour',
      ]);
    }
  });

  it('reads a value through the wrappers that do not change it', () => {
    // Five spellings of the same string at run time. A walk that did not know them would
    // read each as "not a literal" and go quiet — silently, which is the failure mode this
    // guard is least allowed to have.
    for (const wrapped of [
      `<div style={{ color: ('red') }} />`,
      `<div style={{ color: 'red' as string }} />`,
      `<div style={{ color: 'red' satisfies string }} />`,
      `<div style={{ color: (fallback ?? 'red')! }} />`,
    ]) {
      expect(findColourLiterals(wrapped).map((c) => c.kind), wrapped).toEqual([
        'named-colour',
      ]);
    }
    // The fifth is TypeScript's older assertion syntax, which only exists in a `.ts` file.
    expect(
      findColourLiterals(`const color = <string>'red';`, 'src/a.ts').map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('reads a value an await hands back', () => {
    // `await x` produces whatever `x` produces, so it passes a value through the way a
    // parenthesis does. This was the last shape the rule it replaced caught and the walk
    // did not, and neither a reviewer nor the author thought of it: it turned up by running
    // both implementations over a cross-product of every context, painting name and value
    // spelling and diffing the answers, which is the only method here that finds a position
    // nobody has already imagined.
    for (const awaited of [
      `async function f() { el.style.color = await 'red'; }`,
      `async function f() { el.style.color = await (on ? 'red' : 'gray'); }`,
      `async function f() { const s = { color: await 'red' }; }`,
      `async function f({ color = await 'red' }) {}`,
    ]) {
      expect(findColourLiterals(awaited).map((c) => c.kind), awaited).toEqual([
        'named-colour',
      ]);
    }
    // A promise of a colour is a colour behind a name, which is the first residue entry.
    expect(
      findColourLiterals(`async function f() { el.style.color = await load(); }`),
    ).toEqual([]);
  });

  it('reports a nested paint once, not once per value that encloses it', () => {
    // The walk over the file already visits every property, so the walk over a *value*
    // stops at an object literal rather than descending into it. Descending would report
    // the inner paint twice — once as itself and once as part of the outer value.
    expect(
      findColourLiterals(`const sx = { background: { color: 'red' } };`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('reports the site on one line, short enough to read in a failure message', () => {
    const wrapped = `<div style={{
  color: active
    ? 'red'
    : 'gray',
}} />`;
    expect(findColourLiterals(wrapped)[0]?.text).toBe(`color: active ? 'red' : 'gray'`);
    const long = `<div style={{ color: on ? 'red' : ${TICK}${'var(--color-acc) '.repeat(12)}${TICK} }} />`;
    const reported = findColourLiterals(long)[0]?.text ?? '';
    expect(reported.length).toBeLessThanOrEqual(120);
    expect(reported.endsWith('…')).toBe(true);
  });

  it('finds a colour on a JSX attribute written as an expression container', () => {
    // The braces are the ordinary way to write this, and for a long time only the plain
    // string attribute was caught — and only the plain string attribute was tested, which
    // is how a claim that JSX attributes were covered survived. A status dot whose colour
    // prop is a conditional is the default React idiom, and it shipped a painted pixel with
    // every gate green.
    for (const attribute of [
      `<Dot color={'red'} />`,
      `<Dot color={"red"} />`,
      `<path fill={failed ? 'red' : 'gray'} />`,
      `<path stroke={active ? 'navy' : undefined} />`,
      `<stop stopColor={c ?? 'gold'} />`,
      `<Dot
  color={
    failed ? 'red' : 'gray'
  }
/>`,
    ]) {
      expect(findColourLiterals(attribute).map((c) => c.kind), attribute).toContain(
        'named-colour',
      );
    }
  });

  it('reads a colour branch whose sibling is a non-paint keyword', () => {
    // A greedy quantifier hands back the *last* quoted literal in range, and in a ternary
    // that is the wrong one half the time. Every keyword below is one the named set leaves
    // out on purpose, because each follows the theme rather than fixing a colour — and each
    // is what a component naturally writes as the other branch. `none` is SVG's own default
    // non-paint value, so a fill that paints on one condition and does not on the other is
    // the ordinary spelling, and it was the silent one. Reversing the branches made it loud,
    // which is the tell. Both branches are value positions to a tree, so neither order can
    // be the quiet one.
    for (const branch of ["'none'", "'inherit'", "'transparent'", "'currentColor'", "''"]) {
      const first = `<path fill={on ? 'red' : ${branch}} />`;
      const second = `<path fill={on ? ${branch} : 'red'} />`;
      expect(findColourLiterals(first).map((c) => c.kind), first).toContain('named-colour');
      expect(findColourLiterals(second).map((c) => c.kind), second).toContain('named-colour');
    }
  });

  it('reads a colour branch whose sibling is a template literal', () => {
    // The third quote character. A span could cross the other two and not this one, so a
    // branch written as a template blocked it and the colour on the far side went unread —
    // and the same code with the branches swapped fired. This is the shape this codebase
    // will actually write it in: the colour vocabulary here is custom properties, so an
    // interpolated token on one branch against a hardcoded fallback on the other is the
    // ordinary way to reach for one.
    for (const styled of [
      `<path fill={on ? ${TICK}var(--color-acc)${TICK} : 'red'} />`,
      `<path fill={on ? 'red' : ${TICK}var(--color-acc)${TICK}} />`,
      `<div style={{ background: on ? ${TICK}var(--color-acc)${TICK} : 'navy' }} />`,
      `<div style={{ background: on ? 'navy' : ${TICK}var(--color-acc)${TICK} }} />`,
      `el.style.color = on ? ${TICK}var(--color-acc)${TICK} : 'red';`,
      `el.style.setProperty('color', on ? ${TICK}var(--c)${TICK} : 'red');`,
    ]) {
      expect(findColourLiterals(styled).map((c) => c.kind), styled).toContain('named-colour');
    }
  });

  it('reports one violation per painted site, whichever quotes the branches use', () => {
    // Three patterns used to read the same property, one per quote character, so a value
    // with a branch in each was reported once per pattern that could see a colour — two
    // findings for one literal, deduplicated by hand afterwards. A site is one node, so
    // there is nothing left to deduplicate.
    for (const mixed of [
      `<div style={{ color: on ? 'red' : "navy" }} />`,
      `<div style={{ color: on ? 'red' : "steel" }} />`,
      `<div style={{ color: on ? 'red' : ${TICK}var(--x)${TICK} }} />`,
    ]) {
      expect(findColourLiterals(mixed).map((c) => c.kind), mixed).toEqual(['named-colour']);
    }
  });

  it('reads a colour branch in an object literal the same way', () => {
    for (const style of [
      `<div style={{ fill: on ? 'red' : 'none' }} />`,
      `<div style={{ color: failed ? 'navy' : 'inherit' }} />`,
      `el.style.color = on ? 'red' : 'transparent';`,
    ]) {
      expect(findColourLiterals(style).map((c) => c.kind), style).toContain('named-colour');
    }
  });

  it('reads a braced conditional the same way whichever branch is the colour', () => {
    // Naming the ANSI colour words as introducers had a side effect nothing tested: in a
    // braced ternary the quoted first branch read as a key introducing the second, so the
    // rule fired when the first branch was one of eight words and stayed quiet otherwise.
    // All four of these are one attribute with two branches to a tree.
    for (const conditional of [
      `<Dot color={on ? 'red' : 'gray'} />`,
      `<Dot color={on ? 'red' : undefined} />`,
      `<Dot color={on ? 'crimson' : 'gray'} />`,
      `<Dot color={on ? 'crimson' : undefined} />`,
    ]) {
      expect(findColourLiterals(conditional).map((c) => c.kind), conditional).toEqual([
        'named-colour',
      ]);
    }
  });

  it('does not let one attribute read the value of the attribute after it', () => {
    // Two attributes are two nodes, so there is no span to bound and nothing to bound it
    // with. The character rule needed the closing brace for this.
    expect(findColourLiterals(`<Dot color={pick()} label={'gold'} />`)).toEqual([]);
    expect(findColourLiterals(`<Dot fill={shade} /> <Tag kind={'silver'} />`)).toEqual([]);
  });

  it('finds a colour assigned through a member, an index or a bare name', () => {
    expect(findColourLiterals(`el.style.color = 'red';`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(
      findColourLiterals(`el.style.setProperty('color', 'red');`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    // The index form, which the character rule never saw: the bracket sat between the name
    // and the equals sign the same way the quote sat between a key and its colon.
    expect(findColourLiterals(`el.style['color'] = 'red';`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    // And the bare name, which is what a destructured or re-assigned binding looks like.
    expect(findColourLiterals(`color = 'red';`).map((c) => c.kind)).toEqual(['named-colour']);
  });

  it('finds a colour through the other two setters that take a property name', () => {
    // The namespaced setter puts its namespace first, so the name is the second argument
    // rather than the first, and the typed-OM map spells the verb on its own. Both were
    // lost the first time the comma form was narrowed to a named call, because only two
    // names were written down.
    for (const written of [
      `el.setAttributeNS(null, 'fill', 'red');`,
      `el.setAttributeNS('http://www.w3.org/2000/svg', 'stroke', 'navy');`,
      `el.attributeStyleMap.set('fill', 'red');`,
    ]) {
      expect(findColourLiterals(written).map((c) => c.kind), written).toContain(
        'named-colour',
      );
    }
  });

  it('finds a colour under a key however the key is spelled', () => {
    // Bare, quoted and computed-from-a-literal are one node kind with three spellings of
    // `name`. The quote used to break the match and the bracket was never handled at all.
    for (const keyed of [
      `const s = { color: 'red' };`,
      `const s = { 'color': 'red' };`,
      `const s = { "background-color": "red" };`,
      `const s = { ['color']: 'red' };`,
      `const s = { [${TICK}--color-acc${TICK}]: 'red' };`,
    ]) {
      expect(findColourLiterals(keyed).map((c) => c.kind), keyed).toEqual(['named-colour']);
    }
  });

  it('finds a colour either side of a quote the value escapes', () => {
    // This was a residue entry for three rounds, in both its directions: a character rule
    // reads an escaped quote as a delimiter it is not, and whichever half the quantifier
    // handed back decided the answer. Unescaping a literal is the parser's job, so the
    // question stops existing rather than being answered. Both orders, both quotes.
    for (const escaped of [
      `const s = { background: 'red url(\\'a.png\\')' };`,
      `const s = { background: "red url(\\"a.png\\")" };`,
      `const s = { background: 'url(\\'a.png\\') red' };`,
      `const s = { background: "url(\\"a.png\\") red" };`,
    ]) {
      expect(findColourLiterals(escaped).map((c) => c.kind), escaped).toContain(
        'named-colour',
      );
    }
  });

  it('finds a colour beside a url quoted inside the same value', () => {
    expect(
      findColourLiterals(`<div style={{ background: "url('a.png') no-repeat red" }} />`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`<div style={{ background: 'url("a.png") no-repeat red' }} />`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour']);
  });

  it('finds a colour under an xterm theme key', () => {
    // The next place in this app a colour literal will actually be written: the terminal
    // renders for real, xterm takes concrete strings rather than variables, and none of
    // its keys is a CSS painting property. Scoping the rule to painting properties — which
    // is what stopped it crying wolf on `src/transport` — made every one of these
    // invisible, so the keys are named explicitly.
    for (const themed of [
      `new Terminal({ theme: { foreground: 'white' } });`,
      `const t = { cursor: 'red' };`,
      `const t = { cursorAccent: 'navy' };`,
      `const t = { selectionBackground: 'gold' };`,
      `const t = { selectionForeground: "tan" };`,
      `const t = { selectionInactiveBackground: 'silver' };`,
      `const t = { scrollbarSliderBackground: 'gray' };`,
      `const t = { scrollbarSliderHoverBackground: 'gray' };`,
      `const t = { scrollbarSliderActiveBackground: 'gray' };`,
      `const t = { overviewRulerBorder: 'crimson' };`,
      `const t = { black: 'gold' };`,
      `const t = { brightWhite: 'ivory' };`,
      `const t = { brightMagenta: 'orchid' };`,
      `const t = { extendedAnsi: ['tan'] };`,
    ]) {
      expect(findColourLiterals(themed).map((c) => c.kind), themed).toContain('named-colour');
    }
  });

  it('leaves the theme keys alone when they are read off the tokens in force', () => {
    // `transport/surface/xterm.ts` writes exactly this shape, and it is correct: the value
    // is a token lookup with a follow-the-theme fallback, not a colour.
    expect(
      findColourLiterals(`const t = { foreground: token('--color-fg', 'inherit') };`),
    ).toEqual([]);
    expect(
      findColourLiterals(
        `const t = { selectionBackground: token('--color-acc35', 'transparent') };`,
      ),
    ).toEqual([]);
  });

  it('reads the arguments of a call the value is computed from', () => {
    // The other half of the case above, and the reason arguments are walked at all: the
    // fallback behind a token lookup is a real place to write a colour by hand, and the
    // character rule could not reach past the comma to see it.
    expect(
      findColourLiterals(`const t = { foreground: token('--color-fg', 'red') };`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`<div style={{ color: mix(a, b) ?? 'red' }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`<div style={{ color: run({ x: 1 }) ?? 'red' }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`const s = { color: new Shade('red') };`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('finds a colour written as a default, wherever a default can be written', () => {
    // The eighth position, and the only one found by comparing against the rule this
    // replaced rather than by reading this one. A pattern matching `=` did not care what
    // kind of equals sign it had, so it caught every shape below; the first version of the
    // walk enumerated sites and left all of them out. A gate that got narrower than the one
    // it replaced is the one outcome the rewrite was not allowed to have.
    //
    // The first two are the ordinary React default-prop spelling of a hardcoded colour,
    // which is exactly what this gate exists to catch.
    for (const defaulted of [
      `function Dot({ color = 'red' }: Props) {}`,
      `const Dot = ({ color = 'red' }: Props) => null;`,
      `const { color = 'red' } = props;`,
      `function paint(fill = 'red') {}`,
      `enum Swatch { color = 'red' }`,
      // A parameter property, which is a parameter and a field at once.
      `class A { constructor(private color = 'red') {} }`,
      // Nested, and inside an array pattern: a binding element is a binding element at any
      // depth, which is the thing a span of characters had to be taught one shape at a time.
      `function f({ style: { color = 'red' } = {} }) {}`,
      `const [{ color = 'red' }] = list;`,
      // Renamed, both directions. A binding carries two names and a default belongs to
      // both, so either one painting is enough.
      `const { color: c = 'red' } = props;`,
      `const { tier: color = 'red' } = props;`,
    ]) {
      expect(findColourLiterals(defaulted).map((c) => c.kind), defaulted).toEqual([
        'named-colour',
      ]);
    }
  });

  it('finds a colour written as a default in a destructuring assignment', () => {
    // Not a declaration: `({ color = 'red' } = props)` is an assignment whose target is
    // written as an object literal, so the default hangs off a shorthand property and is
    // spelled `objectAssignmentInitializer`. That one different field name is why it sits
    // outside TypeScript's predicate and is named by hand at its site.
    expect(
      findColourLiterals(`let color; ({ color = 'red' } = props);`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    // The renamed form is a property assignment whose value is itself an assignment, which
    // is read because an assignment evaluates to what was assigned.
    expect(
      findColourLiterals(`({ color: c = 'red' } = props);`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    // A shorthand with no default is a read, not a write.
    expect(findColourLiterals(`const s = { color };`)).toEqual([]);
  });

  it('finds a colour on a private field, declared or assigned', () => {
    // A private name carries its hash in the node text and the vocabulary is written without
    // one, so the hash comes off where the name is read. `#color` paints exactly what
    // `color` paints — the hash is how the language spells privacy, not part of the name.
    expect(findColourLiterals(`class A { #color = 'red' }`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(findColourLiterals(`this.#color = 'red';`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    // And a private name that does not paint stays quiet, so the hash-stripping is not a
    // blanket widening.
    expect(findColourLiterals(`class A { #tier = 'gold' }`)).toEqual([]);
  });

  it('reads the value of an assignment used as a value', () => {
    // An assignment evaluates to what was assigned, so a chained write and an argument
    // written as one are read. The left side is where the value went, not what it is, which
    // is why only the right side is followed.
    expect(
      findColourLiterals(`el.style.color = fallback = 'red';`).map((c) => c.kind),
    ).toContain('named-colour');
    expect(
      findColourLiterals(`const s = { color: (c = 'red') };`).map((c) => c.kind),
    ).toEqual(['named-colour']);
    expect(findColourLiterals(`const s = { color: (c = shade) };`)).toEqual([]);
  });

  it('stays quiet on a default that is not a colour, and on a binding with none', () => {
    for (const innocent of [
      `function paint(fill = tokenFor(x)) {}`,
      `function Dot({ color = 'inherit' }: Props) {}`,
      `const { color } = props;`,
      `enum Swatch { color }`,
      `function Tag({ tier = 'gold' }: Props) {}`,
    ]) {
      expect(findColourLiterals(innocent), innocent).toEqual([]);
    }
  });

  it('finds a colour a destructuring declaration takes apart', () => {
    // The other live React idiom for holding a colour, and the one declaration shape whose
    // value is not written next to the name that receives it: there is no default here and
    // no key, only a pattern and the thing it is applied to. Missed by the rule this
    // replaced as well, so this is new coverage rather than restored coverage.
    for (const held of [
      `const [color, setColor] = useState('red');`,
      `const [background, setBackground] = useState(initial ?? 'navy');`,
      `const { fill } = pick('red');`,
      `const [color] = ['red'];`,
    ]) {
      expect(findColourLiterals(held).map((c) => c.kind), held).toEqual(['named-colour']);
    }
  });

  it('does not read a pattern that binds nothing which paints', () => {
    // The pattern is what admits the value, so a pattern full of names that do not paint
    // leaves the initializer alone however loudly it spells a colour.
    expect(findColourLiterals(`const [tier, setTier] = useState('red');`)).toEqual([]);
    expect(findColourLiterals(`const { label } = pick('navy');`)).toEqual([]);
    // And an ordinary destructure of something with no literal in it stays quiet either way.
    expect(findColourLiterals(`const { color, background } = useTheme();`)).toEqual([]);
  });

  it('reports a default once, not once for the binding and once for the declaration', () => {
    // `const { color = 'red' } = props` is a binding element with a default inside a
    // declaration whose pattern paints. Both branches can see it; only one reports.
    expect(
      findColourLiterals(`const { color = 'red' } = props;`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('finds a colour in a declaration and in a class field', () => {
    // Neither is a property of anything, and both are how a component keeps a colour to
    // hand before it paints with it.
    expect(findColourLiterals(`const color = 'red';`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(findColourLiterals(`class Swatch { borderColor = 'navy'; }`).map((c) => c.kind)).toEqual(
      ['named-colour'],
    );
  });

  it('does not fire on a url whose path names a colour', () => {
    // A path is not a paint. The word scan used to see straight through the parentheses,
    // so a background referencing an image file whose name happens to carry a colour word
    // failed the sweep — and a false positive in a guard is how the next person in a hurry
    // learns to reach for the suppression rather than the fix.
    expect(
      findColourLiterals(`<div style={{ background: "url('/img/red-banner.png')" }} />`),
    ).toEqual([]);
    expect(
      findColourLiterals(`<div style={{ backgroundImage: 'url(/assets/tan.png)' }} />`),
    ).toEqual([]);
    expect(
      findColourLiterals(`<div style={{ background: 'url("/i/gold.svg")' }} />`),
    ).toEqual([]);
    // The bracket rule reads an arbitrary value the same way and had the same hole.
    expect(findColourLiterals('className="bg-[url(/img/red.png)]"')).toEqual([]);
  });

  it('does not fire on a comparison against a colour, either way round', () => {
    // Reading a colour is not writing one, and one equals sign is not three. The character
    // rule matched the first of the three in a strict comparison, so the positive form
    // fired and the negated form did not — which way a condition happens to be written is
    // not a property of the code's correctness.
    for (const compared of [
      `if (color === 'red') return;`,
      `if (borderColor == 'red') return;`,
      `if (color !== 'red') return;`,
      `if (background === "navy") return;`,
      `const dark = foreground === 'white';`,
      // And a comparison is still a comparison in value position: the four operators that
      // pass an operand through are an allowlist, so this hands back a boolean rather than
      // a colour however it is written down.
      `el.style.color = mode === 'red';`,
    ]) {
      expect(findColourLiterals(compared), compared).toEqual([]);
    }
  });

  it('does not fire on a call whose arguments are a property name and a colour', () => {
    // The name-as-argument form exists for `setProperty`, and an early version accepted any
    // call at all — or no call, since a two-element array is the same three characters.
    // Neither writes a pixel, and both are shapes a test helper or a lookup table writes
    // without thinking about colour at all.
    for (const innocent of [
      `track('color', 'gold');`,
      `t('background', 'tan');`,
      `const pair = ['color', 'gold'];`,
      `expect(rule('fill', 'navy')).toBe(1);`,
      // `set` is far too common a verb to take on its own, so the typed-OM map has to be
      // named in front of it. An ordinary cache writing a two-argument entry is the shape
      // that would otherwise be caught.
      `cache.set('color', 'gold');`,
    ]) {
      expect(findColourLiterals(innocent), innocent).toEqual([]);
    }
  });

  it('finds a colour written to a custom property', () => {
    // The blind spot that hid `theme/tokens.ts` entirely: every token in this codebase
    // spells the word as a prefix, and the pattern wanted it as a suffix. The module whose
    // whole job is writing those values was the one the rule could not see into, so
    // hard-coding the accent there turned the picker into a no-op with every gate green.
    for (const written of [
      `const s = { '--color-acc': 'red' };`,
      `const s = { "--color-status-failed": "red" };`,
      `target.setProperty('--color-acc', 'red');`,
      `root.style.setProperty('--accent-color', 'navy');`,
      // The bare call, which is what a destructured setter looks like. The verb is what is
      // named, not the receiver — requiring `el.style.` in front would miss the helper that
      // takes the declaration as a parameter, which is the shape `theme/tokens.ts` writes.
      `setProperty('--color-acc', 'red');`,
    ]) {
      expect(findColourLiterals(written).map((c) => c.kind), written).toContain(
        'named-colour',
      );
    }
  });

  it('leaves the token names themselves alone', () => {
    // The same module is full of custom-property names next to each other. A name is not
    // a value, and none of these words is a colour.
    expect(
      findColourLiterals(`const names = ['--color-bg0', '--color-fg3', '--color-acc35'];`),
    ).toEqual([]);
    expect(
      findColourLiterals(`const s = { '--color-bg0': surface.bg0, '--color-fg': surface.fg };`),
    ).toEqual([]);
  });

  it('finds a colour in a hand-wrapped conditional', () => {
    // There is no formatter in this repo, so a ternary split over three lines is the
    // ordinary way to write the shape this rule most needs to catch — stopping at a line
    // end meant catching it only when it happened to fit on one.
    const wrapped = `<div style={{
  color: active
    ? 'red'
    : 'gray',
}} />`;
    expect(findColourLiterals(wrapped).map((c) => c.kind)).toEqual(['named-colour']);
  });

  it('does not read a sibling property, however the two are laid out', () => {
    // One property is one node. A comma had to stand in for that, and a comma is also what
    // separates the arguments of a call, which is what put the token fallback out of reach.
    const siblings = `const s = {
  color: computeColour(),
  tier: 'gold',
};`;
    expect(findColourLiterals(siblings)).toEqual([]);
  });

  it('does not match a painting property inside a longer word', () => {
    // `fill` in `autofill`, `stroke` in `keystroke`. A character rule needed a word-boundary
    // guard for this, and the guard was what kept `pointBackground` out of the vocabulary
    // too; a tree compares whole names, so only the vocabulary decides now.
    for (const innocent of [
      `const s = { autofill: 'gold' };`,
      `const s = { keystroke: 'tan' };`,
      `const s = { refill: 'tan' };`,
      `const s = { unfilled: 'navy' };`,
    ]) {
      expect(findColourLiterals(innocent), innocent).toEqual([]);
    }
  });

  it('does not let a value reach into the next statement', () => {
    expect(
      findColourLiterals([`const color = pick();`, `const tier = 'gold';`].join('\n')),
    ).toEqual([]);
    expect(findColourLiterals(`const color = pick(); const tier = 'gold';`)).toEqual([]);
  });

  it('stays quiet on a string that no painting name introduces', () => {
    // An early rule flagged any single-word quoted string, which would have fired on
    // protocol literals in `src/transport` — a colour guard shouting at a module that
    // paints nothing is a guard someone switches off.
    for (const innocent of [
      `const tier = 'gold';`,
      `if (kind === 'silver') return;`,
      `const same = shell === 'tan';`,
      `const s = { label: 'navy' };`,
    ]) {
      expect(findColourLiterals(innocent), innocent).toEqual([]);
    }
  });

  it('does not mistake prose or an ordinary short string for a colour', () => {
    expect(findColourLiterals('// the red build turned green again')).toEqual([]);
    expect(findColourLiterals(`/** paints it ${TICK}red${TICK} when it fails */`)).toEqual([]);
    expect(findColourLiterals(`const label = 'red alert';`)).toEqual([]);
    expect(findColourLiterals(`t('Tasks');`)).toEqual([]);
  });

  it('does not fire on tokens, on the colours that follow the theme, or on prose', () => {
    expect(findColourLiterals('color: var(--color-acc)')).toEqual([]);
    expect(findColourLiterals('className="bg-transparent text-current text-inherit"')).toEqual(
      [],
    );
    expect(findColourLiterals('className="border-line2 text-fg3 bg-acc14"')).toEqual([]);
    // Arbitrary values that are not colours, and the geometry the chrome is full of.
    expect(findColourLiterals('className="text-[11px] h-[22px] w-[38px]"')).toEqual([]);
    expect(
      findColourLiterals('className="grid-cols-[var(--spacing-rail)_222px_1fr]"'),
    ).toEqual([]);
    // Ordinary English, and an issue number.
    expect(findColourLiterals('the red build turned green again')).toEqual([]);
    expect(findColourLiterals('issue #12345')).toEqual([]);
  });

  it('parses a .ts file as TypeScript and a .tsx file as JSX', () => {
    // `<T>value` is a type assertion in one and an element in the other, and getting it
    // backwards is a parse error that takes the rest of the file's sites down with it —
    // silently, because a file the guard cannot read looks exactly like a clean one. The
    // sweep passes the real path for this reason; the default is `.tsx` because the shapes
    // this guard exists to catch are written in components.
    const assertion = `const swatch = <Record<string, string>>{ color: 'red' };`;
    expect(findColourLiterals(assertion, 'src/a.ts').map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(findColourLiterals(assertion, 'src/a.tsx')).toEqual([]);
  });
});

/*
 * What the parser closed, one case per entry, each paired with the real thing it sits next
 * to.
 *
 * These are the shapes the character rule reported as violations and the tree does not.
 * Every one of them is a *loud* failure that has now stopped happening, so each pair is
 * written the same way: the quiet half proves the false positive is gone, and the loud half
 * proves the same spelling still fires where it means something. Without the second half a
 * rule that had simply stopped working would pass this file.
 */
describe('what the parser closed', () => {
  it('no longer reads a quoted colour name followed by a colon as a key', () => {
    // The loudest false positive the rule had, and it had two spellings: a conditional
    // between two colour names, where the first branch read as a key introducing the
    // second, and a label table mapping colour names to display strings, where the `case`
    // label did. Both are quiet; a key that really is a key still fires.
    expect(findColourLiterals(`const c = on ? 'red' : 'gray';`)).toEqual([]);
    expect(
      findColourLiterals(
        `function label(k) { switch (k) { case 'red': return 'Red alert'; } }`,
      ),
    ).toEqual([]);
    expect(findColourLiterals(`<Dot color={on ? 'red' : 'gray'} />`).map((c) => c.kind)).toEqual(
      ['named-colour'],
    );
    expect(findColourLiterals(`const s = { color: 'red' };`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
  });

  it('no longer reads a comment that describes painting as painting', () => {
    // A character rule cannot tell a line that talks about painting from one that paints,
    // and telling them apart by pattern is how a scanner comes to mistake a regex literal
    // for a comment and fall silent over everything after it. A parser knows where the
    // comments are because it has to.
    expect(
      findColourLiterals(`// background: the swatch stays 'red' until the run lands`),
    ).toEqual([]);
    expect(
      findColourLiterals(`/** Paints ${TICK}background${TICK} as 'red' when the probe fails. */`),
    ).toEqual([]);
    expect(
      findColourLiterals(
        `// background: the swatch stays 'red' until the run lands\nel.style.background = 'red';`,
      ).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('no longer runs a value across a statement that forgot its semicolon', () => {
    // The price of letting a span cross line ends, in a tree that is semicolon-terminated
    // by convention and nothing more. The parser does the insertion itself.
    const unterminated = `const color = pick()
const tier = 'gold'`;
    expect(findColourLiterals(unterminated)).toEqual([]);
    expect(findColourLiterals(`const color = 'gold'`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
  });

  it('no longer reads a comparison operand or a lookup key as the value', () => {
    // Two of the four spellings the old false-positive entry named. A condition is tested
    // and an index selects; neither is painted, and neither is walked. The other two — an
    // argument and a conditional's other branch — are still read, on purpose, and have
    // their own cases above.
    //
    // The first two are each held shut twice over, so the third case is the one that pins
    // the condition on its own: a predicate's argument is a literal the walk *would* read
    // anywhere else, and the only reason it is quiet here is that a condition is not a
    // value. Take that away and this goes red while the comparisons stay green.
    expect(findColourLiterals(`<div style={{ color: isShade('red') ? a : b }} />`)).toEqual([]);
    expect(findColourLiterals(`<div style={{ color: tier === 'gold' ? a : 'inherit' }} />`)).toEqual(
      [],
    );
    expect(findColourLiterals(`<div style={{ color: palette['gold'] ?? shade }} />`)).toEqual([]);
    expect(
      findColourLiterals(`<div style={{ color: labelFor(kind) === 'navy' ? a : b }} />`),
    ).toEqual([]);
    // The same three shapes with a colour actually in the value still fire.
    expect(
      findColourLiterals(`<div style={{ color: tier === 'gold' ? a : 'crimson' }} />`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`<div style={{ color: palette[k] ?? 'gold' }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });
});

/*
 * The corpus, which is the claim.
 *
 * Round two's commit message said a sweep "reports zero remaining shapes the old rule caught
 * and this one does not". The sweep was a local script, so nobody could audit it — only
 * contradict it, which is what happened in the next review. The rule that came out of that
 * is worth more than the sweep was: **no completeness claim survives unless the check that
 * produces it is committed and runnable.**
 *
 * So the corpus is here. It is a cross-product, and it is deliberately two matrices rather
 * than one, because the two axes fail independently:
 *
 *  - a **site** is a place a name is bound to a value. Every site below is checked with one
 *    ordinary colour, so removing a site kind turns this red.
 *  - a **value spelling** is a way of writing the thing bound. Every spelling below is
 *    checked at one ordinary site, and the ones the walk does not follow are listed by name
 *    next to the residue entry that explains each, so narrowing the value walk turns this
 *    red and widening it turns the *miss* list red.
 *
 * Both matrices run every cell through the same `findColourLiterals` a reviewer can call.
 * What this cannot do is compare against the rule this replaced — that implementation is
 * gone once this merges — so the comparison that found `await` and the four shapes in the
 * span-coincidence residue entry is recorded in those entries rather than claimed here.
 */
describe('the corpus', () => {
  /** Every place the guard treats as a site, one ordinary colour in each. */
  const SITES = [
    `const s = { color: {V} };`,
    `const s = { 'color': {V} };`,
    `const s = { ['color']: {V} };`,
    `const s = { '--color-acc': {V} };`,
    `<div style={{ color: {V} }} />`,
    `<path fill={{V}} />`,
    `<path fill="red" />`,
    `el.style.color = {V};`,
    `el.style['color'] = {V};`,
    `color = {V};`,
    `this.#color = {V};`,
    `const color = {V};`,
    `class A { color = {V}; }`,
    `class A { #color = {V}; }`,
    `function f(fill = {V}) {}`,
    `function f({ color = {V} }) {}`,
    `const g = ({ color = {V} }) => null;`,
    `const { color = {V} } = props;`,
    `const { color: c = {V} } = props;`,
    `const { tier: color = {V} } = props;`,
    `const [{ color = {V} }] = list;`,
    `function f([color] = [{V}]) {}`,
    `const [color, setColor] = useState({V});`,
    `enum E { color = {V} }`,
    `let color; ({ color = {V} } = props);`,
    `({ color: c = {V} } = props);`,
    `el.style.setProperty('color', {V});`,
    `el.setAttribute('fill', {V});`,
    `el.setAttributeNS(null, 'fill', {V});`,
    `el.attributeStyleMap.set('fill', {V});`,
    `setProperty('--color-acc', {V});`,
  ] as const;

  /** Every way of writing a value that the walk follows. */
  const READ = [
    `'red'`,
    `"red"`,
    '`red`',
    '`${width}px solid red`',
    `'1px solid red'`,
    `('red')`,
    `'red' as string`,
    `'red' satisfies string`,
    `(fallback ?? 'red')!`,
    `on ? 'red' : 'gray'`,
    `on ? 'gray' : 'red'`,
    `on ? 'red' : undefined`,
    `status === 'failed' ? 'red' : undefined`,
    `pick() ?? 'red'`,
    `chosen || 'red'`,
    `on && 'red'`,
    `width + 'px solid red'`,
    `token('--c', 'red')`,
    `String('red')`,
    `new Shade('red')`,
    `(c = 'red')`,
    `mix(a, b) ?? 'red'`,
  ] as const;

  /**
   * Every way of writing a value the walk does **not** follow, against the residue entry
   * that explains it. A cell that starts firing means the walk widened and an entry is now
   * fiction; a cell that stops being listed means it was closed and the entry should go.
   */
  const NOT_READ = [
    { value: `chosen`, entry: 'a colour that arrives through a variable' },
    { value: `(() => 'red')()`, entry: 'produced by running something' },
    { value: `(() => { const c = 'red'; return c; })()`, entry: 'produced by running something' },
    { value: 'css`red`', entry: 'produced by running something' },
    { value: `['red', 'gray'][+on]`, entry: 'matched by span coincidence' },
    { value: `palette('red').hex()`, entry: 'matched by span coincidence' },
    { value: `f('red')(x)`, entry: 'matched by span coincidence' },
    { value: `new X('red').y`, entry: 'matched by span coincidence' },
    { value: `tier === 'gold' ? a : b`, entry: 'a comparison is not a value' },
    { value: `palette['gold']`, entry: 'an index is a lookup, not a value' },
    { value: `'url(/img/red.png)'`, entry: 'a colour carried inside a url' },
  ] as const;

  /**
   * A body to sit in, so a statement-shaped site is legal where an expression-shaped one is.
   *
   * `await` and `yield` are deliberately absent from the matrices above: they are legal in
   * an expression position and not in a class-field initializer, an enum member or a
   * parameter default, so putting them in the cross-product would mean asserting over cells
   * that do not parse. They have their own case, which covers them at three sites.
   */
  function wrap(source: string): string {
    return `function _() { ${source} }`;
  }

  /**
   * The sites with somewhere to put an arbitrary expression.
   *
   * A plain-string JSX attribute is the one site that is not one: `fill="red"` takes a
   * string and nothing else, and a conditional written there is a syntax error rather than
   * a shape the guard could read. It is a real site with its own code path — the attribute's
   * initializer is a literal rather than a braced container — so it stays in the site matrix
   * and sits out the value matrices, which is what the placeholder marks.
   */
  const EXPRESSION_SITES = SITES.filter((site) => site.includes('{V}'));

  it('is a real cross-product and not a handful of cases', () => {
    expect(SITES).not.toEqual(EXPRESSION_SITES);
    expect(EXPRESSION_SITES.length * (READ.length + NOT_READ.length)).toBeGreaterThan(900);
  });

  it('catches an ordinary colour at every site it claims to read', () => {
    const quiet = SITES.map((site) => wrap(site.replace('{V}', `'red'`))).filter(
      (source) => findColourLiterals(source).length === 0,
    );
    expect(quiet).toEqual([]);
  });

  it('catches every value spelling it claims to follow, at every site', () => {
    const quiet: string[] = [];
    for (const site of EXPRESSION_SITES) {
      for (const value of READ) {
        const source = wrap(site.replace('{V}', value));
        if (findColourLiterals(source).length === 0) quiet.push(source);
      }
    }
    expect(quiet).toEqual([]);
  });

  it('stays quiet on every value spelling it does not follow, at every site', () => {
    const loud: string[] = [];
    for (const site of EXPRESSION_SITES) {
      for (const { value, entry } of NOT_READ) {
        const source = wrap(site.replace('{V}', value));
        if (findColourLiterals(source).length > 0) loud.push(`${entry}: ${source}`);
      }
    }
    expect(loud).toEqual([]);
  });
});

/*
 * The residue list from `colourGuard.ts`, one case per entry.
 *
 * These prove that each *listed* miss is real, and that is the whole of what they prove.
 * They cannot show the list is complete — the missing entries are by definition the shapes
 * nobody thought to write a case for, which is the direction every round of this review has
 * failed in. The doc says so now rather than claiming exhaustiveness.
 *
 * What they do buy is that the list cannot rot: widen the rule and the matching case goes
 * red, so closing a gap costs one bullet and one case, and forgetting to is not an option.
 * Both directions have already happened here — a gap closing, and an example quietly
 * ceasing to demonstrate its own entry.
 *
 * Eight of the twelve are the vocabulary and value questions that survived the rewrite,
 * because a syntax tree fixes where you look and not what you are looking for. Two of those
 * eight moved rather than staying put — the computed key narrowed to a name assembled at
 * run time, and the library key kept its shape while its reason changed — and the guard's
 * own comment says which. The other four arrived a different way each: the walk's own
 * narrowness, a logical assignment that both implementations miss and nobody had written
 * down, the four shapes the old rule matched by span coincidence, and what a `for…of`
 * iterates.
 *
 * The misses that are *not* here are the ones each round was held for, and they are absent
 * on purpose. A default — a parameter's, a destructured binding's, an enum member's, a
 * shorthand assignment's — and a private field were all caught by the pattern and dropped by
 * a walk that hand-listed declaration kinds. Those are a **site kind**, not a residue entry:
 * the fix restores coverage rather than documenting its absence, and asking TypeScript which
 * declarations have an initializer is what closed the class rather than the instances.
 */
describe('the documented residue', () => {
  it('misses a colour that arrives through a variable', () => {
    expect(findColourLiterals(`<div style={{ color: chosen }} />`)).toEqual([]);
  });

  it('misses a colour name that no painting property introduces', () => {
    expect(findColourLiterals(`const tier = 'gold';`)).toEqual([]);
  });

  it('misses a painting property the list does not name', () => {
    // A useful subset of CSS, not CSS. The comment used to claim it covered CSS, and these
    // five are what made that false.
    for (const css of [
      `const s = { textDecoration: 'underline red' };`,
      `const s = { columnRule: '1px solid red' };`,
      `const s = { textEmphasis: 'dot red' };`,
      `const s = { borderInlineStart: '1px solid red' };`,
      `const s = { filter: 'drop-shadow(0 0 2px red)' };`,
    ]) {
      expect(findColourLiterals(css), css).toEqual([]);
    }
  });

  it('misses a colour key belonging to a library the list does not name', () => {
    // xterm's keys came off this entry because that terminal is in this app. Nothing else
    // here has a colour key of its own, and `pointBackground` is not `background`: the tree
    // compares whole names, so naming it is the only way in.
    expect(findColourLiterals(`const s = { pointBackground: 'navy' };`)).toEqual([]);
    expect(findColourLiterals(`const s = { gridLine: 'silver' };`)).toEqual([]);
    expect(findColourLiterals(`const s = { series: [{ area: 'gold' }] };`)).toEqual([]);
  });

  it('misses bare CSS text carried inside a string or a template', () => {
    // In all three the property is *inside* the literal, and the walk looks for a property
    // introducing one. This is the entry a TypeScript parser cannot help with even in
    // principle: reading it means parsing the string's contents as CSS.
    expect(findColourLiterals(`el.style.cssText = 'color: red';`)).toEqual([]);
    expect(findColourLiterals(`const s = css${TICK}color: red;${TICK};`)).toEqual([]);
    expect(findColourLiterals(`html += '<div style="color: red"></div>';`)).toEqual([]);
  });

  it('misses a custom property whose name is only known at run time', () => {
    // A computed key holding a literal reads like any other key now; one assembled from a
    // variable has no name to compare, and that is all that is left of this entry.
    expect(findColourLiterals(`const s = { [${TICK}--color-\${key}${TICK}]: 'red' };`)).toEqual(
      [],
    );
  });

  it('misses a setter it does not name, and a tuple written for one it does', () => {
    // Four call shapes are named. A project's own wrapper is indistinguishable from the
    // false positive naming them closed — same two arguments, different intent, nothing but
    // the name to go on.
    expect(findColourLiterals(`applyStyle(el, 'color', 'red');`)).toEqual([]);
    expect(findColourLiterals(`paint('background', 'navy');`)).toEqual([]);
    expect(findColourLiterals(`const pairs = [['--color-acc', 'red']];`)).toEqual([]);
    expect(
      findColourLiterals(`for (const [k, v] of pairs) root.style.setProperty(k, v);`),
    ).toEqual([]);
  });

  it('misses a colour carried inside a url', () => {
    // The price of closing the loudest false positive the rule had: the argument is set
    // aside as a path before the words are counted, and a data URI can carry a whole
    // stylesheet through the same door. Percent-encoded, so nothing else in the value is
    // left for the rule to see.
    expect(
      findColourLiterals(
        `<div style={{ background: 'url(data:image/svg+xml,%3Csvg%20fill%3Dred/%3E)' }} />`,
      ),
    ).toEqual([]);
  });

  it('misses a colour the value only produces by running something', () => {
    // New with the walk, and the price of it being narrow. The walk follows the shapes a
    // value is made of — literals, both branches of a conditional, the sides of `??`, `||`,
    // `&&` and `+`, an array's elements, a call's arguments, a template's substitutions —
    // and stops everywhere else. Stopping is what keeps a condition's operand and a
    // lookup's key out of the answer, which is the entry directly above this one in the
    // other direction.
    expect(findColourLiterals(`<div style={{ color: (() => 'red')() }} />`)).toEqual([]);
    expect(findColourLiterals(`const s = { color: css${TICK}red${TICK} };`)).toEqual([]);
    // And the half of the deleted entry that moved here rather than being solved. The
    // rewrite's own PR first called that entry "purely a span bound", which was true of the
    // comma and the brace and not of the semicolon: a statement body hid a colour from the
    // pattern too, for a different reason, and hides it from the walk now.
    expect(
      findColourLiterals(`const s = { color: (() => { const c = 'red'; return c; })() };`),
    ).toEqual([]);
  });

  it('misses what the old rule matched by span coincidence', () => {
    // A span of characters running from a painting property to the end of an expression
    // contains every literal in between, whatever part each one plays. The rule this
    // replaced reported all four of these, and it had no reading of any of them: following
    // them means deciding that an argument reaches the value through a return, a call and a
    // property access, which is a claim about what a function does rather than about syntax.
    //
    // The same coincidence reported a comparison operand and a lookup key as violations, and
    // those were closed as false positives in the same move. This is that trade seen from
    // the other side — a documented trade, not a silent regression.
    for (const coincidence of [
      `const s = { color: ['red', 'gray'][+on] };`,
      `const s = { color: palette('red').hex() };`,
      `const s = { color: f('red')(x) };`,
      `const s = { color: new X('red').y };`,
    ]) {
      expect(findColourLiterals(coincidence), coincidence).toEqual([]);
    }
    // The argument of a call whose result *is* the value is still read, which is the line
    // this entry draws: one more step of indirection is where the reading stops.
    expect(
      findColourLiterals(`const s = { color: palette('red') };`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('misses what a for-of iterates', () => {
    // The loop head takes a declaration list, so `color` is an ordinary VariableDeclaration
    // with no initializer and the colours sit in the statement's `expression` — a different
    // field, holding a collection rather than a value. Missed by the pattern this replaced
    // as well, so nothing regressed; it is listed because the guard now claims the whole
    // initializer family and this is the member it does not read.
    expect(findColourLiterals(`for (const color of ['red']) {}`)).toEqual([]);
    // The neighbouring loop shape, where the name does have an initializer, is read.
    expect(
      findColourLiterals(`for (let color = 'red'; ; ) {}`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('misses an assignment whose operator is not a plain equals sign', () => {
    // A blind spot both implementations have and neither had written down: these paint, and
    // only `=` is a site. The rule this replaced missed them for its own reason — it wanted
    // one equals sign and these carry two characters in front of it — so nothing regressed
    // here, it had just never been said out loud. Listed rather than closed, because
    // widening the site set is a decision that belongs in its own diff.
    expect(findColourLiterals(`el.style.color ??= 'red';`)).toEqual([]);
    expect(findColourLiterals(`el.style.color ||= 'red';`)).toEqual([]);
    // The plain assignment next to them still fires, so this is a gap and not a dead rule.
    expect(findColourLiterals(`el.style.color = 'red';`).map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
  });

  it('misses a hex the source does not spell as one', () => {
    // The one miss with no backstop under it. The hex rule reads whole files and would
    // catch this anywhere else; percent-encoding hides the hash from it, and the url entry
    // puts the same value out of rule 4's reach, so both halves of the backstop miss the
    // same literal for different reasons.
    expect(
      findColourLiterals(
        `<div style={{ background: 'url(data:image/svg+xml,%3Csvg fill=%23ff0000/%3E)' }} />`,
      ),
    ).toEqual([]);
  });
});

/*
 * The false positives that survived the rewrite, one case per entry.
 *
 * Three of the six the character rule had. The other three — the comment, the missing
 * semicolon and the quoted key — are in "what the parser closed" above, because a syntax
 * tree answers them outright. These three it does not: two are vocabulary and one is the
 * deliberate price of reading a call's arguments.
 */
describe('the false positives that are left', () => {
  it('does fire on a literal in a call the value is computed from', () => {
    // The same walk that catches a hardcoded fallback behind a token lookup catches a
    // display string behind a formatter. Nothing in the tree separates them: both are an
    // argument to a call whose result is the value.
    expect(
      findColourLiterals(`<div style={{ color: label('Red alert') }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('does fire on a table keyed by an ANSI theme name whose values are prose', () => {
    // The price of the eight ANSI words being in the vocabulary: a key called `red` set to
    // a sentence that contains the word is the same tree as one set to a colour. Once per
    // entry. This is the half of the old quoted-key entry the parser does not answer — a
    // ternary between two colour names stopped firing, but a key really is a key here, and
    // which keys paint is vocabulary.
    expect(
      findColourLiterals(`const LABELS = { red: 'Red alert', green: 'Green light' };`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour', 'named-colour']);
  });

  it('does fire on a value that merely contains a colour word', () => {
    // A false positive, but a loud one. Nothing silent can ship a pixel.
    //
    // The url form came off this entry, because a path is the one place a colour word
    // turns up in a value often enough to be worth knowing. What is left is everything
    // else that spells one — most plausibly a token whose own name carries it, which is a
    // token a theme file is free to define.
    expect(
      findColourLiterals(`<div style={{ color: 'var(--brand-red-500)' }} />`).map((c) => c.kind),
    ).toEqual(['named-colour']);
  });
});

describe('hardcoded colour guard', () => {
  it('scans the whole package, so an empty sweep cannot pass vacuously', () => {
    expect(scanned.length).toBeGreaterThan(8);
    expect(scanned.map((file) => file.path)).toContain('src/App.tsx');
    expect(scanned.map((file) => file.path)).toContain('src/theme/colourGuard.ts');
  });

  it('finds nothing in the tree as it stands', () => {
    expect(scanForColourLiterals(scanned)).toEqual([]);
  });

  it('trips on each of the four kinds, injected into a real file', () => {
    // Trap 12, done properly: this runs the *real* sweep over the *real* tree with one
    // declaration added, rather than handing a string to the rules. A guard that is quietly
    // unwired — scanning an empty file list, or filtering away everything — passes a
    // fixture test and fails this one.
    expect(new Set(OFFENDERS.map((o) => o.kind)).size).toBe(4);
    for (const { kind, snippet } of OFFENDERS) {
      const poisoned = scanned.map((file) =>
        file.path === 'src/App.tsx'
          ? { ...file, source: `${file.source}\n${snippet}\n` }
          : file,
      );
      const violations = scanForColourLiterals(poisoned);
      expect(violations, kind).toHaveLength(1);
      expect(violations[0]?.file, kind).toBe('src/App.tsx');
      expect(violations[0]?.kind, kind).toBe(kind);
    }
  });

  it('parses each file by its own extension, not by a guess', () => {
    // The sweep hands `findColourLiterals` the real path, and this is the only thing that
    // proves it does. A `.ts` file parsed as TSX is a parse error at the first type
    // assertion, and the sites after it go quiet — which looks exactly like a clean file.
    // The fixture test next to `scriptKindFor` shows the two kinds differ; nothing but this
    // shows the sweep is the one telling them apart.
    const assertion = `const swatch = <Record<string, string>>{ color: 'red' };`;
    const poisoned = scanned.map((file) =>
      file.path === 'src/theme/tokens.ts'
        ? { ...file, source: `${file.source}\n${assertion}\n` }
        : file,
    );
    const violations = scanForColourLiterals(poisoned);
    expect(violations).toHaveLength(1);
    expect(violations[0]?.file).toBe('src/theme/tokens.ts');
  });

  it('does not trip when the same offender is injected as a comment', () => {
    // The injection above used to be written as a comment, and that is exactly what the
    // syntax rule stopped reading. Keeping it here as its own case is what stops the list
    // above quietly reverting to a form that proves nothing about rule 4 — and the hex
    // half shows the other three rules are unaffected, because they read characters and a
    // hash in a comment is still a hash.
    const commented = (snippet: string): readonly unknown[] =>
      scanForColourLiterals(
        scanned.map((file) =>
          file.path === 'src/App.tsx'
            ? { ...file, source: `${file.source}\n// injected: ${snippet}\n` }
            : file,
        ),
      );
    expect(commented(`<i style={{ color: 'red' }} />`)).toEqual([]);
    expect(commented(`<i style={{ color: '#ff0000' }} />`)).toHaveLength(1);
  });

  it('still finds a colour literal in every allowlisted token module', () => {
    // Without this, an allowlist entry left behind after a refactor would keep quietly
    // exempting a file that no longer defines tokens.
    for (const allowed of TOKEN_DEFINITION_MODULES) {
      const file = scanned.find((candidate) => candidate.path === allowed);
      expect(file, `${allowed} is allowlisted but was not scanned`).toBeDefined();
      expect(
        findColourLiterals(file?.source ?? '', allowed).length,
        allowed,
      ).toBeGreaterThan(0);
    }
  });

  it('does not exempt the guard module itself', () => {
    // It writes down none of the shapes the three text rules hunt for. Rule 4 reads a tree,
    // so the shapes *it* hunts for can be written in a comment and several are — which is
    // the one thing the rewrite relaxed here, and the sweep above is what proves the rest
    // still holds.
    expect(TOKEN_DEFINITION_MODULES).not.toContain('src/theme/colourGuard.ts');
  });

  it('keeps the token stylesheet the only stylesheet under src', () => {
    expect(Object.keys(stylesheets).map(normalize).sort()).toEqual(
      [...TOKEN_DEFINITION_STYLESHEETS].sort(),
    );
  });

  it('names every dependency stylesheet a module pulls in for its side effect', () => {
    // The glob above is rooted at this file, so it proves something about `src` and nothing
    // about the bundle. Reading the built CSS is what showed the difference: a dependency
    // stylesheet imported by package name lands in it carrying a hex background, a hex
    // foreground, a colour function and a data URI painting a path, and no rule in this
    // module has ever seen any of them.
    //
    // This is the one assertion here that fails on somebody else's diff: add a side-effect
    // import of a dependency stylesheet anywhere under `apps/web/src` and it goes red in
    // your PR, not in this one. That is the point of it, and the message says what to do.
    expect(
      findDependencyStylesheets(scanned),
      'a module now imports a dependency stylesheet that DEPENDENCY_STYLESHEETS in ' +
        'src/theme/colourGuard.ts does not name. That file paints pixels no rule in this ' +
        'guard can read, so it is reviewed by hand: look at what it paints and whether the ' +
        'theme can reach it, then add its specifier to that list in the same diff.',
    ).toEqual([...DEPENDENCY_STYLESHEETS].sort());
  });

  it('does not see a stylesheet that arrives any other way', () => {
    // The residue for the list above, and the reason its sentence carries a qualifier. A
    // binding import and a dynamic one are not how a stylesheet is pulled in for its side
    // effect; an `@import` inside `index.css` is, and it is how Tailwind and five font
    // stylesheets actually arrive here. None of the three is visible to a pattern run over
    // TypeScript source, and the last one shows up in the built CSS as the generated
    // `--tw-*` colour fallbacks.
    const elsewhere = [
      { path: 'src/a.ts', source: "import styles from 'some-lib/a.css';" },
      { path: 'src/b.ts', source: "await import('some-lib/b.css');" },
      { path: 'src/c.ts', source: "const href = 'some-lib/c.css';" },
    ];
    expect(findDependencyStylesheets(elsewhere)).toEqual([]);
  });

  it('trips when a module pulls in a stylesheet the list does not name', () => {
    // Trap 12 again: the check runs over the real tree with one import added, so a list
    // that has stopped being wired up fails here rather than passing vacuously.
    const poisoned = scanned.map((file) =>
      file.path === 'src/App.tsx'
        ? { ...file, source: `${file.source}
import 'some-lib/dist/theme.css';
` }
        : file,
    );
    expect(findDependencyStylesheets(poisoned)).not.toEqual([...DEPENDENCY_STYLESHEETS].sort());
    expect(findDependencyStylesheets(poisoned)).toContain('some-lib/dist/theme.css');
  });
});
