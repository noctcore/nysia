/**
 * The one exemption lint-meta's glob rule grants, checked by running the bundler.
 *
 * `tools/lint-meta/src/moduleReferences.ts` reports every `import.meta.glob` call it cannot
 * prove harmless. Exactly one option spelling buys an exemption — `query: '?raw'`, which
 * makes the modules come back as source text — and this is what says that is true, by
 * executing the pinned Vite over it rather than reading its documentation.
 *
 * That is not a flourish. Three times in this rule's history a proof has asserted something
 * the toolchain does not have: a per-pattern `'…?raw'` spelling that is not a Vite feature,
 * a `globEager` removed before Vite 8, and a `{ raw: true }` query pinned as safe when it
 * expands to `?raw=true` — which is not Vite's raw flag, so the real module comes back. A
 * test that cannot tell the difference between a feature and a wish is worth nothing, and
 * the only way to tell is to run the thing.
 *
 * Vite is `apps/web`'s dependency, not the root's, so it is resolved from that package —
 * which also means this drives the exact version the application builds with. If it cannot
 * be resolved the test fails rather than skipping: a proof that did not run must never look
 * like one that passed.
 */

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { describe, expect, it } from 'vitest';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const fromWebApp = createRequire(join(repoRoot, 'apps/web/package.json'));

/** The marker the store module exports. Seeing it means a real module came back. */
const PROVIDER = 'PROVIDER';

type Loaded = 'the real module' | 'source text' | 'something else';

/**
 * What one `import.meta.glob` call actually hands back, from the pinned Vite.
 *
 * Built and executed through Vite's own module runner, so the glob goes through the same
 * plugin pipeline the application uses. The verdict is read off the value, not off the
 * emitted code: a module namespace carrying the provider's export is the leak this rule
 * exists to prevent, and a string is the source text that cannot leak anything.
 */
async function whatTheGlobReturns(args: string): Promise<Loaded> {
  const vite = (await import(pathToFileURL(fromWebApp.resolve('vite')).href)) as {
    createServer: (config: unknown) => Promise<{
      ssrLoadModule: (url: string) => Promise<Record<string, unknown>>;
      close: () => Promise<void>;
    }>;
  };

  const root = mkdtempSync(join(tmpdir(), 'nysia-vite-glob-'));
  try {
    mkdirSync(join(root, 'store'), { recursive: true });
    writeFileSync(join(root, 'store/StoreContext.js'), `export const ${PROVIDER} = () => 42;\n`);
    writeFileSync(
      join(root, 'entry.js'),
      `const m = import.meta.glob(${args});\n` +
        // An eager glob's values are the modules themselves; a lazy one's are loaders. The
        // harness has to take both, or it reads "eager" as "broken".
        'export const loaded = Promise.all(\n' +
        "  Object.values(m).map((entry) => (typeof entry === 'function' ? entry() : entry)),\n" +
        ');\n',
    );

    const server = await vite.createServer({
      root,
      logLevel: 'silent',
      appType: 'custom',
      server: { middlewareMode: true },
    });
    try {
      const entry = await server.ssrLoadModule('/entry.js');
      const [first] = (await entry.loaded) as unknown[];

      if (typeof first === 'string') return 'source text';
      if (first !== null && typeof first === 'object') {
        if (PROVIDER in first) return 'the real module';
        if (typeof (first as { default?: unknown }).default === 'string') return 'source text';
      }
      return 'something else';
    } finally {
      await server.close();
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe('what the pinned Vite does with a glob', () => {
  // A dev server plus a module graph per case; slower than everything else in this suite and
  // worth it, because the alternative is believing a docstring.
  const TIMEOUT = 60_000;

  it(
    'hands back the real module when nothing says otherwise',
    async () => {
      // The control. Without it, every case below could pass because the harness is broken.
      await expect(whatTheGlobReturns("'./store/*.js'")).resolves.toBe('the real module');
    },
    TIMEOUT,
  );

  it(
    "hands back source text for query: '?raw', which is the one form the rule exempts",
    async () => {
      await expect(whatTheGlobReturns("'./store/*.js', { query: '?raw' }")).resolves.toBe(
        'source text',
      );
    },
    TIMEOUT,
  );

  it(
    'still hands back source text with the inert options beside it',
    async () => {
      // `import` and `eager` are the two options the rule treats as deciding nothing. This
      // is the shape `apps/web` actually writes.
      await expect(
        whatTheGlobReturns("'./store/*.js', { query: '?raw', import: 'default', eager: true }"),
      ).resolves.toBe('source text');
    },
    TIMEOUT,
  );

  it(
    'hands back the real module for a raw query written as an object with a true value',
    async () => {
      // The case that motivated the inversion: this expands to `?raw=true`, and Vite's raw
      // flag is `raw` followed by `&` or the end of the query. The rule used to exempt it
      // because the key was spelled `raw`.
      await expect(whatTheGlobReturns("'./store/*.js', { query: { raw: true } }")).resolves.toBe(
        'the real module',
      );
    },
    TIMEOUT,
  );

  it(
    'hands back the real module when a base moves where the pattern resolves',
    async () => {
      // `base` is one of the options the rule cannot evaluate, so it reports rather than
      // guessing. This is what it would be guessing about.
      await expect(whatTheGlobReturns("'./*.js', { base: './store' }")).resolves.toBe(
        'the real module',
      );
    },
    TIMEOUT,
  );
});
