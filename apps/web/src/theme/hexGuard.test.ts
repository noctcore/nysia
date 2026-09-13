import { describe, expect, it } from 'vitest';

import {
  TOKEN_DEFINITION_MODULES,
  TOKEN_DEFINITION_STYLESHEETS,
  findHexLiterals,
} from './hexGuard';

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

const scanned = Object.entries(modules)
  .map(([path, source]) => ({ path: normalize(path), source: String(source) }))
  .filter(({ path }) => !path.endsWith('.test.ts') && !path.startsWith('src/generated/'));

describe('findHexLiterals', () => {
  it('finds the hex colour forms a component could hardcode', () => {
    expect(findHexLiterals('color:#abc')).toEqual(['#abc']);
    expect(findHexLiterals('background:#f2b35b;border-color:#2A3140')).toEqual([
      '#f2b35b',
      '#2A3140',
    ]);
    expect(findHexLiterals('outline:#f2b35b24')).toEqual(['#f2b35b24']);
  });

  it('does not fire on tokens, oklch or a bare hash', () => {
    expect(findHexLiterals('color: var(--color-acc)')).toEqual([]);
    expect(findHexLiterals('oklch(78% 0.12 180)')).toEqual([]);
    expect(findHexLiterals('# heading')).toEqual([]);
    expect(findHexLiterals('issue #12345')).toEqual([]);
  });
});

describe('hardcoded colour guard', () => {
  it('scans the whole package, so an empty sweep cannot pass vacuously', () => {
    expect(scanned.length).toBeGreaterThan(8);
    expect(scanned.map((f) => f.path)).toContain('src/App.tsx');
  });

  it('still finds a hex literal in every allowlisted token module', () => {
    // Trap 12: without this, an allowlist entry left behind after a refactor would keep
    // quietly exempting a file that no longer defines tokens.
    for (const allowed of TOKEN_DEFINITION_MODULES) {
      const file = scanned.find((f) => f.path === allowed);
      expect(file, `${allowed} is allowlisted but was not scanned`).toBeDefined();
      expect(findHexLiterals(file?.source ?? '').length, allowed).toBeGreaterThan(0);
    }
  });

  it('finds no hex literal in any other module', () => {
    const offenders = scanned
      .filter(({ path }) => !TOKEN_DEFINITION_MODULES.includes(path))
      .flatMap(({ path, source }) => findHexLiterals(source).map((hex) => `${path}: ${hex}`));
    expect(offenders).toEqual([]);
  });

  it('keeps the token stylesheet the only stylesheet in the package', () => {
    expect(Object.keys(stylesheets).map(normalize).sort()).toEqual(
      [...TOKEN_DEFINITION_STYLESHEETS].sort(),
    );
  });
});
