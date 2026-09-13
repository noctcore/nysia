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

    expect(cargoDependencies(manifest).sort()).toEqual([
      'anyhow',
      'rusqlite',
      'serde',
      'tempfile',
      'windows',
    ]);
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
    expect(runRules(fixture('clean'))).toEqual([]);
  });

  it('trip on the violating fixture, once per rule', () => {
    const violations = runRules(fixture('trips'));
    expect(violations.map((v) => v.rule).sort()).toEqual([
      'core-declares-no-tauri',
      'no-tauri-outside-desktop',
    ]);

    const tauriImport = violations.find((v) => v.rule === 'no-tauri-outside-desktop');
    expect(tauriImport?.file).toBe('apps/web/src/leak.ts');
    expect(tauriImport?.line).toBeGreaterThan(0);

    const coreDependency = violations.find((v) => v.rule === 'core-declares-no-tauri');
    expect(coreDependency?.file).toBe('crates/nysia-core/Cargo.toml');
    expect(coreDependency?.message).toContain('tauri');
  });

  it('pass against the real repository', () => {
    expect(runRules(repoRoot)).toEqual([]);
  });
});
