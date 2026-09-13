import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

import { findRepoRoot } from './repoRoot.ts';
import { blankRustComments, cargoDependencies, runRules, walk } from './rules.ts';

const repoRoot = findRepoRoot();
const fixture = (name: string): string => join(repoRoot, 'tools/lint-meta/fixtures', name);

describe('cargoDependencies', () => {
  it('reads every dependency table shape cargo accepts', () => {
    const manifest = [
      '[package]',
      'name = "x"',
      'tauri = "this is a package key, not a dependency"',
      '',
      '[dependencies]',
      'serde = { workspace = true }',
      '# tauri = "2.11"',
      'anyhow.workspace = true',
      '',
      '[dev-dependencies]',
      'tempfile = "3"',
      '',
      "[target.'cfg(windows)'.dependencies]",
      'windows = "0.62"',
      '',
      '[dependencies.rusqlite]',
      'version = "0.40"',
    ].join('\n');

    expect(cargoDependencies(manifest).map((d) => d.key).sort()).toEqual([
      'anyhow',
      'rusqlite',
      'serde',
      'tempfile',
      'windows',
    ]);
  });

  it('resolves renamed dependencies to the crate they actually pull', () => {
    const manifest = [
      '[package]',
      'name = "x"',
      '',
      '[dependencies]',
      'ui = { package = "tauri", version = "2.11" }',
      'serde = { workspace = true }',
      '',
      '[dependencies.shell]',
      'package = "tauri-plugin-opener"',
      'version = "2"',
    ].join('\n');

    const byKey = new Map(cargoDependencies(manifest).map((d) => [d.key, d.package]));
    expect(byKey.get('ui')).toBe('tauri');
    expect(byKey.get('shell')).toBe('tauri-plugin-opener');
    // An entry without a `package` key keeps its own name.
    expect(byKey.get('serde')).toBe('serde');
  });

  it('ignores [workspace.dependencies], which is a version table and not a dependency', () => {
    const manifest = [
      '[workspace]',
      'members = ["crates/nysia"]',
      '',
      '[workspace.dependencies]',
      'tauri = { version = "2.11" }',
      '',
      '[workspace.dependencies.windows]',
      'version = "0.62"',
    ].join('\n');

    expect(cargoDependencies(manifest)).toEqual([]);
  });
});

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

describe('the architecture rules', () => {
  it('accept the carve-outs the clean fixture exercises', () => {
    // The clean fixture links tauri from apps/desktop, both in its manifest and its
    // source, and imports it from apps/web/src/transport. All of that is allowed.
    expect(runRules(fixture('clean'))).toEqual([]);
  });

  it('trip on the violating fixture', () => {
    const violations = runRules(fixture('trips'));
    const byRule = (rule: string): string[] =>
      violations.filter((v) => v.rule === rule).map((v) => v.file);

    expect(byRule('no-tauri-outside-desktop')).toContain('apps/web/src/leak.ts');

    // The Rust spellings the previous line-anchored regex walked straight past:
    // `use ::tauri::Builder;` (line 7), `use {tauri, serde};` (10), and the multi-line
    // grouped form (13). Any of them would have put the UI toolkit in the daemon.
    const rustLines = violations
      .filter((v) => v.rule === 'no-tauri-outside-desktop' && v.file === 'crates/nysia/src/leak.rs')
      .map((v) => v.line);
    expect(rustLines).toEqual(expect.arrayContaining([7, 10, 13]));
    // Every crate outside apps/desktop is inspected, not just the chain rooted at
    // nysia-core. crates/nysia can reach tauri straight out of [workspace.dependencies]
    // without editing a single shared file, so it has to be checked on its own.
    expect(byRule('no-tauri-in-rust-crates').sort()).toEqual([
      'crates/nysia-core/Cargo.toml',
      'crates/nysia/Cargo.toml',
    ]);
  });

  it('trip on the transitive fixture and name the chain', () => {
    const violations = runRules(fixture('trips-transitive'));

    const direct = violations.find((v) => v.rule === 'no-tauri-in-rust-crates');
    expect(direct?.file).toBe('crates/nysia-proto/Cargo.toml');

    const chain = violations.find((v) => v.rule === 'no-tauri-reaching-core');
    expect(chain?.file).toBe('crates/nysia-proto/Cargo.toml');
    expect(chain?.message).toContain('nysia-core -> nysia-proto');
  });

  it('pass against the real repository', () => {
    expect(runRules(repoRoot)).toEqual([]);
  });
});
