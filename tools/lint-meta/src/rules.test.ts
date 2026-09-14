import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

import { CargoMetadataError } from './cargoGraph.ts';
import { callSites, importedValues, reExports } from './moduleReferences.ts';
import { findRepoRoot } from './repoRoot.ts';
import {
  blankRustComments,
  noUnmutedRenderer,
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

describe('a renderer that answers queries the daemon has already answered', () => {
  const surface = 'apps/web/src/transport/surface/xterm.ts';

  it('reports the module that builds a terminal and never mutes it', () => {
    const violations = noUnmutedRenderer(fixture('trips'), [surface]);
    expect(violations).toHaveLength(1);
    // The `@xterm/xterm` import, not the stylesheet under it, and not the comment above it
    // that names the mute — which is the whole reason this reads a tree rather than the text.
    expect(violations[0]?.line).toBe(8);
    expect(violations[0]?.message).toContain('muteTerminalReplies()');
  });

  it('says nothing about a module that builds one and mutes it', () => {
    expect(noUnmutedRenderer(fixture('clean'), [surface])).toEqual([]);
  });

  it('obliges no module that builds no terminal', () => {
    // The mute's own module imports nothing from xterm. A rule keyed on the mute being
    // *called* everywhere, rather than on the terminal being *built* here, would report it.
    const mute = 'apps/web/src/transport/surface/muteReplies.ts';
    expect(noUnmutedRenderer(fixture('clean'), [mute])).toEqual([]);
  });

  it('reports the real file the moment the call is removed from it', () => {
    // The finding this rule exists for: deleting `muteTerminalReplies(terminal.parser)` from
    // the shipped file left every gate green, because that file needs a DOM to construct and
    // v0.1's tests are node-only (D-18). Run against the repository as it stands the rule is
    // silent; run against the same file with the call cut out of it, it reports.
    const source = readFileSync(join(repoRoot, surface), 'utf8');
    expect(source, 'the call site this rule pins').toContain('muteTerminalReplies(terminal.parser)');
    expect(noUnmutedRenderer(repoRoot, [surface]), 'the file as it ships').toEqual([]);

    // Both halves of the deletion that was measured: the call, and the import that goes
    // orphaned with it. Cutting the import too is the case that left every gate green.
    const cut = source.replace(/^.*muteTerminalReplies.*$/gm, '');
    expect(callSites(surface, cut, 'muteTerminalReplies')).toEqual([]);

    const scratch = mkdtempSync(join(tmpdir(), 'lint-meta-mute-'));
    try {
      mkdirSync(join(scratch, 'apps/web/src/transport/surface'), { recursive: true });
      writeFileSync(join(scratch, surface), cut, 'utf8');
      expect(noUnmutedRenderer(scratch, [surface]), 'the same file with the call cut out').toHaveLength(1);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });
  it('obliges nothing of a module that only names the type', () => {
    // The over-report this rule shipped with. Both spellings erase — `import type { … }` and
    // the per-specifier `{ type … }`, which `verbatimModuleSyntax` keeps as a statement while
    // binding no value from it — so neither module can construct anything to mute. lint-meta
    // has no suppression mechanism, so a module reported here has no way out but to stop
    // importing the type it needs.
    for (const [name, spelling] of [
      ['types.ts', 'import type { Terminal }'],
      ['inline.ts', 'import { type ITerminalOptions, type Terminal }'],
    ]) {
      const file = `apps/web/src/transport/surface/${name}`;
      expect(readFileSync(join(fixture('clean'), file), 'utf8'), file).toContain(spelling);
      expect(noUnmutedRenderer(fixture('clean'), [file]), file).toEqual([]);
    }
  });

  it('obliges nothing of a script that reaches past the package for the parser alone', () => {
    // The realistic trigger, not a hypothetical one: the executed proof of this parser's
    // handler ordering imports `EscapeSequenceParser` from a deep path and constructs one.
    // That is a value, from a build of the library, and it builds no terminal.
    const file = 'scripts/ordering.test.ts';
    const source = readFileSync(join(fixture('clean'), file), 'utf8');
    expect(source, file).toContain("from '@xterm/xterm/src/common/parser/EscapeSequenceParser'");
    expect(source, 'the fixture must construct it, or it proves nothing').toContain('new EscapeSequenceParser()');
    expect(noUnmutedRenderer(fixture('clean'), [file])).toEqual([]);
  });

  it('still obliges a module that renames the class as it imports it', () => {
    // What the narrowing must not open. The rule reads what the library exports, not what
    // the importer calls it, so an alias hides nothing.
    const file = 'apps/web/src/transport/surface/aliased.ts';
    const source = [
      "import { Terminal as T } from '@xterm/xterm';",
      'export const build = (): T => new T({ cols: 80, rows: 24 });',
    ].join('\n');

    const scratch = mkdtempSync(join(tmpdir(), 'lint-meta-alias-'));
    try {
      mkdirSync(join(scratch, 'apps/web/src/transport/surface'), { recursive: true });
      writeFileSync(join(scratch, file), source, 'utf8');
      expect(noUnmutedRenderer(scratch, [file])).toHaveLength(1);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });

  it('reports a module that hands the constructor on under its own name', () => {
    // A re-export launders the specifier: whoever builds the terminal then imports it from
    // here, and a rule that matches on specifiers stops seeing the library. Reported rather
    // than obliged — this module constructs nothing, so a mute call here would be a lie.
    const file = 'apps/web/src/transport/surface/reexport.ts';
    const violations = noUnmutedRenderer(fixture('trips'), [file]);
    expect(violations).toHaveLength(1);
    expect(violations[0]?.line).toBe(7);
    expect(violations[0]?.message).toContain('re-exports Terminal');
  });
});

describe('what a module binds from another', () => {
  const read = (source: string): string[] =>
    importedValues('x.ts', source).map((value) => `${value.imported} as ${value.local}`);

  it('reads type-ness at the clause and at each specifier', () => {
    expect(read("import type { Terminal } from '@xterm/xterm';")).toEqual([]);
    expect(read("import { type Terminal } from '@xterm/xterm';")).toEqual([]);
    // One `type` specifier does not make its neighbour one.
    expect(read("import { type Terminal, FitAddon } from '@xterm/xterm';")).toEqual([
      'FitAddon as FitAddon',
    ]);
  });

  it('keeps the exported name and the local one apart', () => {
    expect(read("import { Terminal as T } from '@xterm/xterm';")).toEqual(['Terminal as T']);
    expect(read("import Term from '@xterm/xterm';")).toEqual(['default as Term']);
    expect(read("import * as xterm from '@xterm/xterm';")).toEqual(['* as xterm']);
    // A side-effect import binds nothing, which is what a stylesheet is.
    expect(read("import '@xterm/xterm/css/xterm.css';")).toEqual([]);
  });

  it('separates a re-export from an import, and erases a type-only one', () => {
    const names = (source: string): string[][] =>
      reExports('x.ts', source).map((re) => [...re.names]);

    expect(read("export { Terminal } from '@xterm/xterm';")).toEqual([]);
    expect(names("export { Terminal } from '@xterm/xterm';")).toEqual([['Terminal']]);
    expect(names("export * from '@xterm/xterm';")).toEqual([['*']]);
    // Aliasing the namespace hands on exactly as much, so it reads as exactly as much.
    expect(names("export * as xterm from '@xterm/xterm';")).toEqual([['*']]);
    expect(names("export type { Terminal } from '@xterm/xterm';")).toEqual([]);
    expect(names("export { type Terminal, FitAddon } from '@xterm/xterm';")).toEqual([
      ['FitAddon'],
    ]);
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
