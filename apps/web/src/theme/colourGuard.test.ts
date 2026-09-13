import { describe, expect, it } from 'vitest';

import {
  TOKEN_DEFINITION_MODULES,
  TOKEN_DEFINITION_STYLESHEETS,
  findColourLiterals,
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

/** The five shapes, written only here — the guard module deliberately contains none. */
const OFFENDERS = [
  { kind: 'hex', snippet: 'style={{ color: "#ff0000" }}' },
  { kind: 'function', snippet: 'style={{ color: "rgb(255 0 0)" }}' },
  { kind: 'palette-class', snippet: 'className="text-red-500"' },
  { kind: 'named-colour', snippet: 'className="[color:red]"' },
  { kind: 'named-colour', snippet: "style={{ color: 'red' }}" },
  // The concrete failure a colon-flush pattern let through: on main this injected line
  // turned the sweep red, and it would not have here.
  { kind: 'named-colour', snippet: "style={{ color: failed ? 'red' : undefined }}" },
  // The custom-property case, which hid the one module whose job is writing token values.
  { kind: 'named-colour', snippet: `setProperty('--color-acc', 'red');` },
] as const;

describe('findColourLiterals', () => {
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

  it('finds a named colour in an inline style, the same as hex and a function', () => {
    // The gap this closes: the bracket-only rule let an inline style naming a colour ship
    // with every gate green, while the same style naming a hex was caught.
    for (const style of [
      "style={{ color: 'red' }}",
      'style={{ background: "rebeccapurple" }}',
      "style={{ borderColor: 'DarkSlateGray' }}",
    ]) {
      expect(findColourLiterals(style).map((c) => c.kind), style).toEqual(['named-colour']);
    }
  });

  it('finds a colour among the other tokens of a shorthand', () => {
    // The rule used to see only a whole one-word string, so a shorthand naming a colour
    // alongside a width and a style went straight through.
    expect(
      findColourLiterals("style={{ border: '1px solid red' }}").map((c) => c.kind),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals('style={{ boxShadow: "0 0 8px rgba red" }}').map((c) => c.kind),
    ).toContain('named-colour');
  });

  it('finds a colour in a template literal, now that a property has to introduce it', () => {
    // Backticks were excluded while any one-word string counted, because a doc comment
    // marks up code with them. With a property in front they are no more ambiguous than
    // the other two quotes.
    expect(findColourLiterals('style={{ color: `red` }}').map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
  });

  it('finds a colour behind an expression, not only one flush against the colon', () => {
    // The ordinary React conditional style, and the shape that a colon-flush pattern
    // silently stopped seeing — a component writing one of these on main went red and on
    // the first version of the property prefix stayed green.
    for (const conditional of [
      "style={{ color: active ? 'red' : 'gray' }}",
      'style={{ color: failed ? "red" : undefined }}',
      "style={{ backgroundColor: pick(state) ?? 'navy' }}",
    ]) {
      expect(findColourLiterals(conditional).map((c) => c.kind), conditional).toContain(
        'named-colour',
      );
    }
  });

  it('finds a colour inside a template whose interpolation carries quotes', () => {
    const shorthand = "style={{ border: `1px solid ${on ? 'red' : 'gray'}` }}";
    expect(findColourLiterals(shorthand).map((c) => c.kind)).toContain('named-colour');
  });

  it('finds a colour on a JSX attribute, which separates with an equals sign', () => {
    for (const attribute of ['fill="red"', 'stroke="navy"', 'color="red"']) {
      expect(findColourLiterals(attribute).map((c) => c.kind), attribute).toEqual([
        'named-colour',
      ]);
    }
  });

  it('finds a colour assigned through the style object', () => {
    expect(findColourLiterals("el.style.color = 'red';").map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(
      findColourLiterals("el.style.setProperty('color', 'red');").map((c) => c.kind),
    ).toEqual(['named-colour']);
  });

  it('finds a colour under a quoted key', () => {
    // The quote between the property and the colon used to break the match.
    expect(findColourLiterals("{ 'color': 'red' }").map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
    expect(findColourLiterals('{ "background-color": "red" }').map((c) => c.kind)).toEqual([
      'named-colour',
    ]);
  });

  it('finds a colour beside a url quoted inside the same value', () => {
    // Missed by every earlier version of the rule, because the inner quote closed the
    // value early. One pattern per quote character is what lets the value carry the others,
    // whichever way round they are nested.
    expect(
      findColourLiterals(`style={{ background: "url('a.png') no-repeat red" }}`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour']);
    expect(
      findColourLiterals(`style={{ background: 'url("a.png") no-repeat red' }}`).map(
        (c) => c.kind,
      ),
    ).toEqual(['named-colour']);
  });

  it('finds a colour written to a custom property', () => {
    // The blind spot that hid `theme/tokens.ts` entirely: every token in this codebase
    // spells the word as a prefix, and the pattern wanted it as a suffix. The module whose
    // whole job is writing those values was the one the rule could not see into, so
    // hard-coding the accent there turned the picker into a no-op with every gate green.
    for (const written of [
      `{ '--color-acc': 'red' }`,
      `{ "--color-status-failed": "red" }`,
      `target.setProperty('--color-acc', 'red');`,
      `root.style.setProperty('--accent-color', 'navy');`,
    ]) {
      expect(findColourLiterals(written).map((c) => c.kind), written).toContain(
        'named-colour',
      );
    }
  });

  it('leaves the token names themselves alone', () => {
    // The same module is full of custom-property names next to each other. A name is not
    // a value, and none of these words is a colour.
    expect(findColourLiterals(`['--color-bg0', '--color-fg3', '--color-acc35']`)).toEqual([]);
    expect(findColourLiterals(`{ '--color-bg0': surface.bg0, '--color-fg': surface.fg }`)).toEqual(
      [],
    );
  });

  it('does not match a painting property inside a longer word', () => {
    // `fill` in `autofill`, `stroke` in `keystroke`. Fixing the bare-string false
    // positives must not introduce an identifier-shaped set of them instead.
    for (const innocent of [
      "{ autofill: 'gold' }",
      "{ keystroke: 'tan' }",
      "{ refill: 'tan' }",
      "{ unfilled: 'navy' }",
    ]) {
      expect(findColourLiterals(innocent), innocent).toEqual([]);
    }
  });

  it('does not let the expression wander into the next statement', () => {
    // The span between the separator and the literal stops at a semicolon, a quote or a
    // line end, so a painting property cannot reach a literal that has nothing to do
    // with it.
    expect(findColourLiterals(['const color = pick();', "const tier = 'gold';"].join('\n'))).toEqual(
      [],
    );
    expect(findColourLiterals("const color = pick(); const tier = 'gold';")).toEqual([]);
  });

  it('stays quiet on a string that no style property introduces', () => {
    // The rule this replaced flagged any single-word quoted string, which would have
    // fired on protocol literals in `src/transport` — a colour guard shouting at a module
    // that paints nothing is a guard someone switches off.
    for (const innocent of [
      "const tier = 'gold';",
      "if (kind === 'silver') return;",
      "shell === 'tan'",
      "{ label: 'navy' }",
    ]) {
      expect(findColourLiterals(innocent), innocent).toEqual([]);
    }
  });

  it('does not mistake prose or an ordinary short string for a colour', () => {
    expect(findColourLiterals('// the red build turned green again')).toEqual([]);
    expect(findColourLiterals('/** paints it `red` when it fails */')).toEqual([]);
    expect(findColourLiterals("const label = 'red alert';")).toEqual([]);
    expect(findColourLiterals("t('Tasks')")).toEqual([]);
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

  it('trips on each of the four shapes, injected into a real file', () => {
    // Trap 12, done properly: this runs the *real* sweep over the *real* tree with one
    // line added, rather than handing a string to the regex. A guard that is quietly
    // unwired — scanning an empty file list, or filtering away everything — passes a
    // fixture test and fails this one.
    expect(new Set(OFFENDERS.map((o) => o.kind)).size).toBe(4);
    for (const { kind, snippet } of OFFENDERS) {
      const poisoned = scanned.map((file) =>
        file.path === 'src/App.tsx'
          ? { ...file, source: `${file.source}\n// injected: ${snippet}\n` }
          : file,
      );
      const violations = scanForColourLiterals(poisoned);
      expect(violations, kind).toHaveLength(1);
      expect(violations[0]?.file, kind).toBe('src/App.tsx');
      expect(violations[0]?.kind, kind).toBe(kind);
    }
  });

  it('still finds a colour literal in every allowlisted token module', () => {
    // Without this, an allowlist entry left behind after a refactor would keep quietly
    // exempting a file that no longer defines tokens.
    for (const allowed of TOKEN_DEFINITION_MODULES) {
      const file = scanned.find((candidate) => candidate.path === allowed);
      expect(file, `${allowed} is allowlisted but was not scanned`).toBeDefined();
      expect(findColourLiterals(file?.source ?? '').length, allowed).toBeGreaterThan(0);
    }
  });

  it('does not exempt the guard module itself', () => {
    // It describes the shapes it hunts for without writing any of them down. If that ever
    // stops being true the sweep above fails, which is the point: a guard that has to
    // allowlist itself has stopped being checkable.
    expect(TOKEN_DEFINITION_MODULES).not.toContain('src/theme/colourGuard.ts');
  });

  it('keeps the token stylesheet the only stylesheet in the package', () => {
    expect(Object.keys(stylesheets).map(normalize).sort()).toEqual(
      [...TOKEN_DEFINITION_STYLESHEETS].sort(),
    );
  });
});
