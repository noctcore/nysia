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

/** The four shapes, written only here — the guard module deliberately contains none. */
const OFFENDERS = [
  { kind: 'hex', snippet: 'style={{ color: "#ff0000" }}' },
  { kind: 'function', snippet: 'style={{ color: "rgb(255 0 0)" }}' },
  { kind: 'palette-class', snippet: 'className="text-red-500"' },
  { kind: 'named-colour', snippet: 'className="[color:red]"' },
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
