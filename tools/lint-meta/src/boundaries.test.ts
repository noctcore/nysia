import { Minimatch } from 'minimatch';
import { describe, expect, it } from 'vitest';

import {
  BUNDLED_EXTENSIONS,
  DESKTOP,
  STORE,
  STORE_CONTEXT_ALLOWED,
  TAURI_ALLOWED,
  TRANSPORT,
  WEBVIEW,
  WEB_ENTRY,
  directoryPrefixes,
  eslintFile,
  eslintFiles,
  matches,
  matchesAny,
  type Boundary,
} from './boundaries.ts';

/**
 * The two renderings of one boundary must decide the same paths.
 *
 * `eslintFile()` becomes a flat-config `files` glob that ESLint matches with minimatch;
 * `matches()` is what the lint-meta rules walk. They are two functions over one declaration,
 * and this asserts they cannot answer differently — the file-level half of the end-to-end
 * check in `scripts/prove-eslint-bans.ts`.
 */
describe('the glob and the matcher agree', () => {
  const PATHS: readonly string[] = [
    'apps/web/src/main.ts',
    'apps/web/src/main.tsx',
    'apps/web/src/main.mts',
    'apps/web/src/main.cjs',
    // The defect: a file whose name merely begins with the entry point's (#20).
    'apps/web/src/main.helper.tsx',
    'apps/web/src/main.config.ts',
    'apps/web/src/main/index.ts',
    'apps/web/src/mainly.ts',
    // Not a bundled extension, so not a `files` glob's business either.
    'apps/web/src/main.css',
    'apps/web/src/main.json',
    'apps/web/src/store/hooks.ts',
    'apps/web/src/store/deep/nested.tsx',
    'apps/web/src/chrome/TabStrip.tsx',
    'apps/web/src/transport/bridge.ts',
    'apps/desktop/src/bridge.ts',
    'crates/nysia-core/src/lib.rs',
  ];

  const BOUNDARIES: readonly Boundary[] = [WEBVIEW, DESKTOP, TRANSPORT, STORE, WEB_ENTRY];

  it.each(BOUNDARIES.map((b) => [eslintFile(b), b] as const))('%s', (glob, boundary) => {
    const matcher = new Minimatch(glob, { dot: true });
    for (const path of PATHS) {
      // A directory boundary covers every extension, where its glob covers only the bundled
      // ones — so compare on the paths a `files` glob can speak about at all.
      const extension = path.slice(path.lastIndexOf('.') + 1);
      if (!BUNDLED_EXTENSIONS.includes(extension)) continue;
      expect([path, matches(path, boundary)]).toEqual([path, matcher.match(path)]);
    }
  });
});

describe('a file boundary is the file, not a prefix', () => {
  it('covers the entry point in every bundled extension', () => {
    for (const extension of BUNDLED_EXTENSIONS) {
      expect(matches(`apps/web/src/main.${extension}`, WEB_ENTRY)).toBe(true);
    }
  });

  it.each([
    // The exact shape that was allowlisted by lint-meta and banned by ESLint (#20).
    'apps/web/src/main.helper.tsx',
    'apps/web/src/main.config.ts',
    'apps/web/src/main/index.ts',
    'apps/web/src/mainly.ts',
    // Not a module: nothing here can hand back the provider.
    'apps/web/src/main.css',
    'apps/web/src/main.json',
  ])('does not cover %s', (path) => {
    expect(matches(path, WEB_ENTRY)).toBe(false);
    expect(matchesAny(path, STORE_CONTEXT_ALLOWED)).toBe(false);
  });
});

describe('a directory boundary', () => {
  it.each([
    ['apps/web/src/transport/bridge.ts', true],
    ['apps/web/src/transport/surface/xterm.ts', true],
    ['apps/web/src/transport', true],
    // A sibling directory whose name starts the same way. A bare prefix match would take it.
    ['apps/web/src/transporter/x.ts', false],
    ['apps/web/src/chrome/TabStrip.tsx', false],
  ] as const)('%s -> %s', (path, expected) => {
    expect(matches(path, TRANSPORT)).toBe(expected);
  });

  it('renders as a path prefix for the cargo rules, trailing slash included', () => {
    expect(directoryPrefixes(TAURI_ALLOWED)).toEqual([
      'apps/desktop/',
      'apps/web/src/transport/',
    ]);
  });

  it('drops file boundaries rather than approximating them as prefixes', () => {
    // `apps/web/src/main.` as a prefix is the #20 defect written down. A boundary with no
    // prefix form is left out instead, and `matches()` is what answers for it.
    expect(directoryPrefixes(STORE_CONTEXT_ALLOWED)).toEqual(['apps/web/src/store/']);
  });
});

describe('the extension set', () => {
  it('renders into every flat-config glob the same way', () => {
    const suffix = `{${BUNDLED_EXTENSIONS.join(',')}}`;
    for (const glob of eslintFiles([WEBVIEW, ...STORE_CONTEXT_ALLOWED])) {
      expect(glob.endsWith(`.${suffix}`)).toBe(true);
    }
  });

  it('carries the extensions Vite resolves and none that cannot import', () => {
    // `.mts` and `.cts` are here because Vite 8 resolves `.mts` by default, so such a file
    // bundles; leaving them out once left a file covered by neither layer.
    expect(BUNDLED_EXTENSIONS).toContain('mts');
    expect(BUNDLED_EXTENSIONS).toContain('cts');
    expect(BUNDLED_EXTENSIONS).not.toContain('json');
    expect(BUNDLED_EXTENSIONS).not.toContain('css');
  });
});
