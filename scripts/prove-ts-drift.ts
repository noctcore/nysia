/**
 * Proof that the ts-rs drift guard trips (traps register #13).
 *
 * A gate that has never been seen to fail is indistinguishable from a gate that cannot
 * fail. This appends a new exported type to `nysia-proto`, asserts `pnpm ts-drift` exits
 * **1** — drift specifically, not 2, which would mean the crate merely stopped compiling
 * and the proof passed for the wrong reason — then restores the file and asserts the guard
 * is clean again.
 *
 * The mutation is additive on purpose: adding a `SessionKind` variant would break the
 * exhaustive `match` in its `Display` impl and turn a drift proof into a compile-error
 * proof.
 *
 * Failures are raised as exceptions rather than `process.exit`, because `exit` terminates
 * without unwinding and would leave `identity.rs` mutated on disk. Everything that can
 * fail runs inside a `try` whose `finally` writes the original bytes back.
 */
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';

import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const repoRoot = findRepoRoot();
const identity = join(repoRoot, 'crates', 'nysia-proto', 'src', 'identity.rs');
const driftGuard = join(repoRoot, 'scripts', 'ts-drift.ts');

/**
 * A test seam, read by `scripts/prove-ts-drift.restore.test.ts`.
 *
 * Set to `after-mutation` to make the proof fail on purpose immediately after it writes
 * the probe. That is the path that used to leave a developer with a mutated working tree,
 * so it is the path that has to be exercised.
 */
const FAULT = process.env['NYSIA_PROVE_TS_DRIFT_FAULT'];

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

interface GuardRun {
  readonly status: number;
  readonly output: string;
}

function runGuard(): GuardRun {
  const result = spawnSync(process.execPath, [driftGuard], {
    cwd: repoRoot,
    encoding: 'utf8',
    shell: false,
  });
  const stdout = result.stdout ?? '';
  const stderr = result.stderr ?? '';
  process.stdout.write(stdout);
  process.stderr.write(stderr);
  return { status: result.status ?? -1, output: stdout + stderr };
}

/** A failed assertion. Thrown, never exited, so the `finally` restore still runs. */
class ProofFailure extends Error {}

function fail(message: string): never {
  throw new ProofFailure(message);
}

// Bytes, not a string: reading and writing through utf8 would rewrite line endings on a
// CRLF checkout, and this script must put back exactly what it found.
const original = readFileSync(identity);

let failure: string | undefined;

try {
  if (FAULT !== 'after-mutation') {
    const before = runGuard();
    if (before.status !== 0) {
      fail(`the guard was already failing before the mutation (exit ${before.status})`);
    }
  }

  writeFileSync(identity, Buffer.concat([original, Buffer.from(PROBE, 'utf8')]));
  // The marker is what the restore test keys on: it proves the file really was mutated
  // before the failure path was taken, so a passing restore is not just a no-op.
  process.stderr.write(`prove:ts-drift: mutated ${identity}\n`);

  if (FAULT === 'after-mutation') {
    fail('injected fault (NYSIA_PROVE_TS_DRIFT_FAULT=after-mutation)');
  }

  const after = runGuard();
  if (after.status === 0) {
    fail('nysia-proto grew a new exported type and the guard still passed');
  }
  if (after.status !== 1) {
    fail(
      `expected exit 1 (drift); got ${after.status}. Exit 2 means the generator failed, ` +
        'which would make this proof pass for the wrong reason.',
    );
  }
} catch (error) {
  if (!(error instanceof ProofFailure)) throw error;
  failure = error.message;
} finally {
  writeFileSync(identity, original);
}

if (failure !== undefined) {
  process.stderr.write(`prove:ts-drift FAILED — ${failure}\n`);
  process.exit(1);
}

const restored = runGuard();
if (restored.status !== 0) {
  process.stderr.write(
    `prove:ts-drift FAILED — the guard did not go clean again after restoring ` +
      `identity.rs (exit ${restored.status})\n`,
  );
  process.exit(1);
}

process.stdout.write('prove:ts-drift OK — the guard trips on drift and clears when fixed\n');
