import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

import { findRepoRoot } from './repoRoot.ts';
import { cargoDependencies, runRules, walk } from './rules.ts';

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
