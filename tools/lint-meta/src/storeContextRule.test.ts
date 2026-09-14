import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

import { describe, expect, it } from 'vitest';

import { runSourceRules, type Violation } from './rules.ts';

/**
 * A throwaway webview tree with a real store in it.
 *
 * The store modules are not decoration. The glob half of the rule matches patterns against
 * the files that exist rather than reading an extension out of the pattern, so a glob can
 * only be judged against a tree that has something to find.
 */
function scan(file: string, source: string): Violation[] {
  const root = mkdtempSync(join(tmpdir(), 'nysia-store-rule-'));
  try {
    const write = (relative: string, text: string): void => {
      const absolute = join(root, relative);
      mkdirSync(dirname(absolute), { recursive: true });
      writeFileSync(absolute, text);
    };
    write('apps/web/src/store/StoreContext.ts', 'export const StoreContext = {};\n');
    write('apps/web/src/store/StoreProvider.tsx', 'export const P = () => null;\n');
    write('apps/web/src/index.css', ':root { color: red; }\n');
    write(file, source);
    return runSourceRules(root).filter((v) => v.rule === 'no-store-context-outside-store');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const PROBE = 'apps/web/src/chrome/Probe.tsx';

/*
 * Rule (d) over a syntax tree.
 *
 * Every call below is a spelling that defeated some round of the hand-written scanner it
 * replaced: a template literal, a specifier on its own line, a line opening with a block
 * comment, a string holding a comment opener, a regex holding a backtick, a substitution
 * holding one, an ordinary JSX line. None of them is a case any more — a comment is trivia,
 * a string is a StringLiteral, and `import(…)` is a call whatever else shares its line — but
 * a spelling that once got through is the cheapest regression test there is.
 */
describe('rule (d) over parsed source', () => {
  it.each([
    ['a plain dynamic import', "await import('../store/StoreContext')"],
    ['a template literal specifier', 'await import(`../store/StoreContext`)'],
    ['a specifier on its own line', "await import(\n  '../store/StoreContext'\n)"],
    ['a require call', "require('../store/StoreContext')"],
    ['a block comment opening the line', "/* lazily */ await import('../store/StoreContext')"],
    ['a single-dot segment', "await import('../store/./StoreContext')"],
    ['a doubled slash', "await import('../store//StoreContext')"],
    // The over-claim this round found: ESLint's `**/StoreContext.*` catches a query suffix
    // and this did not, while the doc said the two had the same reach. In a specifier `?`
    // opens Vite's query — the opposite of what it means in a glob pattern.
    ['a query suffix', "await import('../store/StoreContext.ts?raw')"],
  ])('reports %s', (_what, call) => {
    expect(scan(PROBE, `export const s = ${call};\n`)).toHaveLength(1);
  });

  it.each([
    ['a string that only looks like a call', `export const s = "await import('./StoreContext')";`],
    ['a line comment saying it', "// await import('../store/StoreContext')\nexport const s = 1;"],
    [
      'a block comment saying it',
      "/*\n * await import('../store/StoreContext')\n */\nexport const s = 1;",
    ],
    [
      'a doc link naming it',
      "/** {@link import('../store/StoreContext').StoreContext} */\nexport const s = 1;",
    ],
    [
      'a JSX line beside a template holding a comment opener',
      'const R = () => (<><Icon n={i} /><Tag c={`w-1/2`} /></>);\nconst o = `/*`;\nexport const s = [R, o];\n/* ordinary */',
    ],
  ])('does not report %s', (_what, source) => {
    expect(scan(PROBE, `${source}\n`)).toEqual([]);
  });

  it('says nothing about a specifier that is not a literal', () => {
    // The honest limit, and one a parser narrows rather than closes: it can say the
    // specifier is computed, never what it will compute to. ESLint is equally blind here.
    const computed =
      "const where = '../store/' + 'StoreContext';\nexport const s = await import(where);\n";
    expect(scan(PROBE, computed)).toEqual([]);
  });

  it('leaves the carve-outs alone', () => {
    const call = "export const s = await import('./StoreContext');\n";
    expect(scan('apps/web/src/store/loader.ts', call)).toEqual([]);
    expect(scan('apps/web/src/main.tsx', call)).toEqual([]);
  });

  it('reports the line the call is on', () => {
    const source = `// one\n// two\nexport const s = await import('../store/StoreContext');\n`;
    expect(scan(PROBE, source)[0]?.line).toBe(3);
  });
});

/*
 * A glob returns modules its pattern never names, so "can this one?" is put to a matcher
 * over the files that exist rather than answered by reading the pattern.
 *
 * Reading it by hand split `*.t?x` on the question mark as though a glob carried a URL
 * query, exempting a call that returns `StoreProvider.tsx`. In a glob `?` is the
 * single-character wildcard. The same version documented a per-pattern `'…?raw'` spelling
 * Vite does not have — as a glob it matches no file, which is how the premise survived its
 * own proof, so the query is read from the options here and nowhere else.
 */
describe('rule (d) and glob imports', () => {
  const glob = (args: string, file = PROBE): Violation[] =>
    scan(file, `export const m = import.meta.glob(${args});\n`);

  it.each([
    ['a question mark, which is a wildcard and not a query', "'../store/*.t?x'"],
    ['a bracket class', "'../store/*.[tj]s'"],
    ['a brace list naming a module extension', "'../store/*.{css,ts}'"],
    ['an unrestricted pattern', "'../store/*'"],
    ['a directory wildcard', "'../**'"],
    ['a plain module pattern', "'../store/*.ts'"],
    ['the eager spelling', "'../store/*.ts'"],
    // Every array case puts the innocent pattern FIRST, which is the input that got through.
    ['a stylesheet in front of the store', "['../**/*.css', '../store/*.ts']"],
    ['a negation in front of the store', "['!../store/ignored.css', '../store/*.t?x']"],
    ['patterns spread across lines', "[\n  '../**/*.css',\n  '../store/*.ts',\n]"],
  ])('reports %s', (_what, args) => {
    expect(glob(args)).toHaveLength(1);
  });

  it('reports the globEager spelling too', () => {
    expect(scan(PROBE, "export const m = import.meta.globEager('../store/*.ts');\n")).toHaveLength(
      1,
    );
  });

  it('reports a pattern built at runtime, which says nothing about what it returns', () => {
    expect(scan(PROBE, 'const p = q;\nexport const m = import.meta.glob(p);\n')).toHaveLength(1);
  });

  it.each([
    // The positive controls. Without them, "ask the matcher" could just be "report every
    // glob" with a proof wrapped round it.
    ['every pattern reaches only stylesheets', "'../**/*.css'"],
    ['an array of them', "['../**/*.css', '../**/*.svg']"],
    ['the options make the whole call raw', "'../**/*.{ts,tsx}', { query: '?raw' }"],
    ['the query is written without its mark', "'../store/*.ts', { query: 'raw' }"],
    ['the query is an object', "'../store/*.ts', { query: { raw: '' } }"],
    ['the older as-raw spelling', "'../store/*.ts', { as: 'raw' }"],
    ['the pattern reaches nothing at all', "'../nowhere/*.ts'"],
  ])('allows a glob where %s', (_what, args) => {
    expect(glob(args)).toEqual([]);
  });

  it('is not fooled by a local that merely looks like import.meta', () => {
    // `import.meta` is a MetaProperty, which no identifier can impersonate — so this needs
    // no defensive spelling the way a regex over text did.
    const source = "const importMeta = { glob: (p: string) => p };\nexport const m = importMeta.glob('../store/*.ts');\n";
    expect(scan(PROBE, source)).toEqual([]);
  });

  it('leaves the carve-outs alone', () => {
    expect(glob("'./*.ts'", 'apps/web/src/store/loader.ts')).toEqual([]);
    expect(glob("'./store/*.ts'", 'apps/web/src/main.tsx')).toEqual([]);
  });
});
