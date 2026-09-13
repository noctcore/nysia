/**
 * Proof that the ts-rs drift guard trips (traps register #13).
 *
 * A gate that has never been seen to fail is indistinguishable from a gate that cannot
 * fail. This appends a new exported type to `nysia-proto`, asserts `pnpm ts-drift` exits
 * **1** — drift specifically, not 2, which would mean the crate merely stopped compiling
 * and the proof passed for the wrong reason — then restores the file from memory and
 * asserts the guard is clean again.
 *
 * The mutation is additive on purpose: adding a `SessionKind` variant would break the
 * exhaustive `match` in its `Display` impl and turn a drift proof into a compile-error
 * proof.
 */
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';

import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const repoRoot = findRepoRoot();
const identity = join(repoRoot, 'crates', 'nysia-proto', 'src', 'identity.rs');
const driftGuard = join(repoRoot, 'scripts', 'ts-drift.ts');

const PROBE = `
/// Temporary type appended by \`pnpm prove:ts-drift\`. If you are reading this in a commit,
/// the proof script died between mutating the file and restoring it — delete this block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct DriftProbe {
    /// Present only so the exported type has a field.
    pub probe: u32,
}
`;

function runGuard(): number {
  const result = spawnSync(process.execPath, [driftGuard], {
    cwd: repoRoot,
    encoding: 'utf8',
    shell: false,
  });
  process.stdout.write(result.stdout ?? '');
  process.stderr.write(result.stderr ?? '');
  return result.status ?? -1;
}

function fail(message: string): never {
  process.stderr.write(`prove:ts-drift FAILED — ${message}\n`);
  process.exit(1);
}

const original = readFileSync(identity, 'utf8');

try {
  const before = runGuard();
  if (before !== 0) {
    fail(`the guard was already failing before the mutation (exit ${before})`);
  }

  writeFileSync(identity, original + PROBE, 'utf8');
  const after = runGuard();
  if (after === 0) {
    fail('nysia-proto grew a new exported type and the guard still passed');
  }
  if (after !== 1) {
    fail(`expected exit 1 (drift); got ${after}. Exit 2 means the generator failed, which ` +
      'would make this proof pass for the wrong reason.');
  }
} finally {
  writeFileSync(identity, original, 'utf8');
}

const restored = runGuard();
if (restored !== 0) {
  fail(`the guard did not go clean again after restoring identity.rs (exit ${restored})`);
}

process.stdout.write('prove:ts-drift OK — the guard trips on drift and clears when fixed\n');
