import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { describe, expect, it } from 'vitest';

import { CargoMetadataError } from './cargoGraph.ts';
import { findRepoRoot } from './repoRoot.ts';
import {
  blankJsComments,
  blankRustComments,
  runCargoRules,
  runSourceRules,
  walk,
} from './rules.ts';

const repoRoot = findRepoRoot();
const fixture = (name: string): string => join(repoRoot, 'tools/lint-meta/fixtures', name);

describe('blankRustComments', () => {
  it('keeps every byte position so line numbers stay right', () => {
    const source = ['use std::fmt; // tauri::Builder', '/* tauri:: */ use serde;'].join('\n');
    const blanked = blankRustComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked.split('\n')).toHaveLength(2);
    // Code survives, comment bodies do not.
    expect(blanked).toContain('use std::fmt;');
    expect(blanked).toContain('use serde;');
    expect(blanked).not.toContain('tauri');
  });

  it('handles doc comments and nested block comments', () => {
    const source = [
      '//! A module doc mentioning tauri::Builder.',
      '/// So does an item doc: use tauri::Manager;',
      '/* outer tauri:: /* inner tauri:: */ still outer tauri:: */',
      'use serde::Serialize;',
    ].join('\n');

    const blanked = blankRustComments(source);
    expect(blanked).not.toContain('tauri');
    expect(blanked).toContain('use serde::Serialize;');
  });

  it('leaves a semicolon inside a comment from truncating the statement after it', () => {
    const source = '/* a stray ; in a comment */\nuse {\n    tauri,\n};\n';
    expect(blankRustComments(source)).not.toContain(';\nuse');
  });

  // The dangerous direction. A string that opens a comment hides everything after it, and
  // the gate keeps reporting success — a false negative, not a false positive.
  it.each([
    ['a block-comment opener in a string', 'const OPEN: &str = "/*";'],
    ['a line-comment opener in a string', 'const LINE: &str = "//";'],
    ['a quote inside a char literal', "const QUOTE: char = '\"';"],
    ['a raw string', 'const RAW: &str = r#"/* still code after this */"#;'],
    ['a byte string', 'const BYTES: &[u8] = b"/*";'],
    ['a C string', 'const CSTR: &CStr = c"/*";'],
    ['a raw C string', 'const CRAW: &CStr = cr#"/*"#;'],
    ['a raw byte string', 'const BRAW: &[u8] = br#"/*"#;'],
    ['an escaped quote', 'const ESC: &str = "he said \\"/*\\"";'],
  ])('does not let %s swallow the code after it', (_what, literal) => {
    const source = `${literal}\nuse tauri::Builder;\n`;
    const blanked = blankRustComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked).toContain('use tauri::Builder;');
  });

  // The other direction of the same failure: not a literal that opens a comment, but an
  // index that drifts. `[...source]` is one slot per code point while every index in the
  // scanner is a UTF-16 code unit, so one astral character shifts every later blank right
  // — and since newlines are skipped, the drift lands on real code.
  //
  // Asserting the length alone cannot catch this: rejoining the code-point array gives the
  // same string back. The content assertion is the one that matters.
  it.each([1, 4, 9, 20])('survives %i astral characters in an earlier comment', (count) => {
    const source = `//! Header ${'\u{1F525}'.repeat(count)}\nuse tauri::Builder;\nuse serde::Serialize;\n`;
    const blanked = blankRustComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked).toContain('use tauri::Builder;');
    expect(blanked).toContain('use serde::Serialize;');
    expect(blanked).not.toContain('Header');
  });

  it('refuses to return a string of a different length than it was given', () => {
    // The invariant the whole scanner rests on. If a future edit reintroduces code-point
    // iteration this throws instead of quietly erasing code.
    const astral = `//! ${'\u{1F525}'.repeat(30)}\nuse tauri::Builder;\n`;
    expect(() => blankRustComments(astral)).not.toThrow();
    expect(blankRustComments(astral)).toHaveLength(astral.length);
  });

  it('blanks what is inside a string, since a string is not an import', () => {
    expect(blankRustComments('let s = "tauri::Builder";')).not.toContain('tauri');
  });

  // An astral char literal is two UTF-16 code units. A pattern matching one leaves the
  // closing quote to pair with whatever follows; the orphaned double quote then opens a
  // string that runs to the next one in the file, erasing everything between.
  //
  // Both spellings below put a `'"'` immediately after the astral literal, with no space,
  // and that is the whole point of them. A third case, `const C: char = '🔥';`, used to
  // sit here and proved nothing: revert the surrogate-pair alternative in CHAR_LITERAL and
  // it still passes. A bare literal orphans its quote too, but with no `"` after it in the
  // file nothing opens a runaway string, so the import below survives and the test is
  // green either way. Do not re-add it — a case that cannot fail is not a case (trap 12).
  it.each([
    ['an array of char literals', `const C: [char; 2] = ['\u{1F525}','"'];`],
    ['a match pattern', `fn f(c: char) -> bool { matches!(c, '\u{1F525}'|'"') }`],
  ])('reads %s as a char literal rather than a lifetime', (_what, literal) => {
    const source = `${literal}\nuse tauri::Builder;\n`;
    const blanked = blankRustComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked).toContain('use tauri::Builder;');
  });

  it('treats a lifetime as code rather than as an unterminated char literal', () => {
    const source = "fn f<'a>(x: &'a str) -> &'a str { x }\nuse tauri::Builder;\n";
    expect(blankRustComments(source)).toContain('use tauri::Builder;');
  });
});

describe('walk', () => {
  it('skips the fixture tree unless asked for it', () => {
    const withoutFixtures = walk(repoRoot);
    expect(withoutFixtures.some((f) => f.startsWith('tools/lint-meta/fixtures'))).toBe(false);

    const withFixtures = walk(repoRoot, true);
    expect(withFixtures.some((f) => f.startsWith('tools/lint-meta/fixtures'))).toBe(true);
  });

  it('reports POSIX-separated paths on every platform', () => {
    expect(walk(fixture('clean')).every((f) => !f.includes('\\'))).toBe(true);
  });
});

describe('the source rules', () => {
  it('accept the carve-outs the clean fixture exercises', () => {
    // The clean fixture imports tauri from apps/web/src/transport and links it from
    // apps/desktop, and its nysia-core lib.rs discusses tauri in doc comments, a nested
    // block comment and a `crate::tauri_helpers` path. All of that is allowed.
    expect(runSourceRules(fixture('clean'))).toEqual([]);
  });

  it('trip on the violating fixture, at the exact lines', () => {
    const violations = runSourceRules(fixture('trips'));
    const byRule = (rule: string): string[] =>
      violations.filter((v) => v.rule === rule).map((v) => v.file);

    expect(byRule('no-tauri-outside-desktop')).toContain('apps/web/src/leak.ts');

    // `use ::tauri::Builder;` (line 22), `use {tauri, serde};` (25), and the multi-line
    // grouped form (28). Every literal spelling and 20 astral characters sit above them, so
    // these three assertions are what catch a scanner that stops scanning.
    const rustLines = violations
      .filter((v) => v.rule === 'no-tauri-outside-desktop' && v.file === 'crates/nysia/src/leak.rs')
      .map((v) => v.line);
    expect(rustLines).toEqual(expect.arrayContaining([22, 25, 28]));
  });

  it('pass against the real repository', () => {
    expect(runSourceRules(repoRoot)).toEqual([]);
  });
});

describe('the cargo dependency rules', () => {
  it('read every manifest spelling cargo accepts', () => {
    const violations = runCargoRules(fixture('cargo/violating'));
    const byRule = (rule: string): string[] =>
      violations.filter((v) => v.rule === rule).map((v) => v.file).sort();

    // A quoted key in a `[ dependencies ]` header with whitespace and a trailing comment,
    // and a rename under a quoted table header. The hand-written parser saw neither.
    expect(byRule('no-tauri-in-rust-crates')).toEqual([
      // Reported for declaring tauri...
      'crates/nysia-hook/Cargo.toml',
      'crates/nysia-proto/Cargo.toml',
      // Renamed the other way round: the key is tauri, the crate behind it is not.
      'crates/nysia-shim/Cargo.toml',
      // ...and this one for being invisible to cargo at all; see the next test.
      'crates/orphan/Cargo.toml',
    ]);
    expect(violations.some((v) => v.message.includes('package = "tauri"'))).toBe(true);
  });

  it('name the transitive chain through a crate whose [package] carries a comment', () => {
    const chains = runCargoRules(fixture('cargo/violating'))
      .filter((v) => v.rule === 'no-tauri-reaching-rust-crates')
      .map((v) => v.message);
    expect(chains.some((m) => m.includes('nysia-core -> nysia-proto -> tauri'))).toBe(true);
  });

  it('seed the reach search from every crate, not from nysia-core alone', () => {
    // The daemon binary reaching tauri through apps/desktop. Its manifest never names
    // tauri, so rule (b) is blind to it, and a search seeded from core alone never looked.
    const chain = runCargoRules(fixture('cargo/violating')).find(
      (v) => v.rule === 'no-tauri-reaching-rust-crates' && v.file === 'crates/nysia/Cargo.toml',
    );
    expect(chain?.message).toContain('nysia -> nysia-desktop -> tauri');
  });

  it('report a crate cargo does not know about rather than skipping it', () => {
    // Reading the graph instead of every manifest on disk narrows the rule in exactly one
    // way, and this is it. A crate outside [workspace] members compiles into nothing today,
    // but a rule that quietly stops looking is the defect this tool exists to catch.
    const orphan = runCargoRules(fixture('cargo/violating')).find(
      (v) => v.file === 'crates/orphan/Cargo.toml',
    );
    expect(orphan?.message).toContain('is not a workspace member');
  });

  it('stay silent when only apps/desktop links tauri', () => {
    expect(runCargoRules(fixture('cargo/clean'))).toEqual([]);
  });

  it('refuse to report zero when cargo cannot read the workspace', () => {
    // The failure this whole tool exists to prevent: a check that did not happen looking
    // exactly like a check that passed.
    expect(() => runCargoRules(fixture('cargo/unresolvable'))).toThrow(CargoMetadataError);
  });

  it('pass against the real repository', () => {
    expect(runCargoRules(repoRoot)).toEqual([]);
  });
});

/*
 * Rule (d) reads whole files, so it has to know where a comment ends. The dangerous
 * direction is over-blanking: a literal the scanner does not recognise opens a comment it
 * never closes, every call below goes invisible, and the gate keeps reporting success.
 */
describe('blankJsComments', () => {
  it('keeps every byte position so line numbers stay right', () => {
    const source = ["const a = 1; // import('./store/StoreContext')", '/* x */ const b = 2;'].join(
      '\n',
    );
    const blanked = blankJsComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked.split('\n')).toHaveLength(2);
    expect(blanked).toContain('const a = 1;');
    expect(blanked).toContain('const b = 2;');
    expect(blanked).not.toContain('StoreContext');
  });

  it('leaves the code after a block comment on the same line', () => {
    // The #19 spelling. The old guard threw away any line whose trim started with `/*`.
    const source = "/* lazily */ const s = await import('../store/StoreContext');\n";
    expect(blankJsComments(source)).toContain("import('../store/StoreContext')");
  });

  it.each([
    ['a block-comment opener in a string', 'const OPEN = "/*";'],
    ['a line-comment opener in a string', "const LINE = '//';"],
    ['a block-comment opener in a template', 'const T = `/*`;'],
    ['an escaped quote before one', 'const E = "he said \\"/*\\"";'],
    ['a URL', "const U = 'https://example.test/*';"],
    ['both quote characters inside a regex', "const R = /['\"]/;"],
  ])('does not let %s swallow the call after it', (_what, literal) => {
    const source = `${literal}\nconst s = await import('../store/StoreContext');\n`;
    const blanked = blankJsComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked).toContain("import('../store/StoreContext')");
  });

  it('blanks a comment that spans lines, and only that', () => {
    const source = [
      '/*',
      " * await import('../store/StoreContext') — prose about the ban.",
      ' */',
      "const s = await import('../store/StoreContext');",
    ].join('\n');
    const blanked = blankJsComments(source);

    expect(blanked).toHaveLength(source.length);
    expect(blanked.split('\n')[1]?.trim()).toBe('');
    expect(blanked).toContain("const s = await import('../store/StoreContext');");
  });

  it('refuses to return a string of a different length than it was given', () => {
    const astral = `// ${'\u{1F525}'.repeat(30)}\nconst s = await import('./StoreContext');\n`;
    expect(blankJsComments(astral)).toHaveLength(astral.length);
    expect(blankJsComments(astral)).toContain("import('./StoreContext')");
  });
});

/*
 * `import.meta.glob` returns modules the specifier never names, so no specifier test can
 * decide it. Rule (d) reports one by default and reads the two exits off the call.
 */
describe('rule (d) and glob imports', () => {
  /** One webview file in a throwaway tree, run through the real rule. */
  const scan = (relative: string, source: string): string[] => {
    const root = mkdtempSync(join(tmpdir(), 'nysia-rules-'));
    try {
      const absolute = join(root, relative);
      mkdirSync(dirname(absolute), { recursive: true });
      writeFileSync(absolute, source);
      return runSourceRules(root)
        .filter((v) => v.rule === 'no-store-context-outside-store')
        .map((v) => v.message);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  };

  const WEBVIEW_FILE = 'apps/web/src/chrome/Probe.ts';

  it.each([
    ['an unrestricted glob', "export const m = import.meta.glob('../store/*');"],
    ['a glob over bundled extensions', "export const m = import.meta.glob('../**/*.{ts,tsx}');"],
    ['the eager spelling', "export const m = import.meta.globEager('../**/*.ts');"],
    // The query belongs to a different call; reading it off this one would be the bug.
    [
      'a raw glob beside a live one',
      "export const m = [\n  import.meta.glob('../a/*.ts', { query: '?raw' }),\n" +
        "  import.meta.glob('../store/*.ts'),\n];",
    ],
  ])('reports %s', (_what, source) => {
    expect(scan(WEBVIEW_FILE, source)).toContain(
      'glob-imports modules it does not name, so it may hand back the store provider; ' +
        'only apps/web/src/store/** and apps/web/src/main.* may touch it. Read the files as ' +
        "text with query: '?raw', or pin the glob to an extension that cannot be a module",
    );
  });

  it.each([
    [
      'source text rather than modules',
      "export const m = import.meta.glob('../**/*.{ts,tsx}', {\n  query: '?raw',\n" +
        "  import: 'default',\n  eager: true,\n});",
    ],
    ['an extension that cannot be a module', "export const m = import.meta.glob('../**/*.css');"],
    ['a query written into the pattern', "export const m = import.meta.glob('../**/*.ts?raw');"],
  ])('allows a glob returning %s', (_what, source) => {
    expect(scan(WEBVIEW_FILE, source)).toEqual([]);
  });

  it('says nothing about a specifier that is not a literal', () => {
    // The honest limit, pinned so it is a known gap rather than a surprise: reading source
    // text cannot resolve a name. ESLint's no-restricted-imports is equally blind to the
    // static equivalent, and `rules.ts` says so at the rule (#19).
    const computed = "const where = '../store/' + 'StoreContext';\nexport const s = await import(where);\n";
    expect(scan(WEBVIEW_FILE, computed)).toEqual([]);
  });

  it('leaves the carve-outs alone', () => {
    expect(scan('apps/web/src/store/loader.ts', "export const m = import.meta.glob('./*.ts');")).toEqual([]);
    expect(scan('apps/web/src/main.tsx', "export const m = import.meta.glob('./store/*.ts');")).toEqual([]);
  });
});
