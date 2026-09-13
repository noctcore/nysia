import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';
import { describe, expect, it } from 'vitest';

import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const repoRoot = findRepoRoot();
const identity = join(repoRoot, 'crates', 'nysia-proto', 'src', 'identity.rs');
const proof = join(repoRoot, 'scripts', 'prove-ts-drift.ts');

/**
 * `scripts/prove-ts-drift.ts` mutates a tracked source file and puts it back. Its restore
 * used to live in a `finally` that the post-mutation failure paths never reached, because
 * they called `process.exit`, which terminates without unwinding — so a developer whose
 * proof failed was left with a modified `crates/nysia-proto/src/identity.rs` and a doc
 * comment claiming otherwise.
 *
 * The fault seam makes that exact path run on demand. Asserting only "the file is
 * unchanged" would pass trivially if the mutation never happened, so the marker on stderr
 * is checked too: it is written immediately after the probe is appended.
 */
describe('the ts-rs drift proof', () => {
  it('restores identity.rs byte for byte when it fails after mutating', () => {
    const before = readFileSync(identity);

    const result = spawnSync(process.execPath, [proof], {
      cwd: repoRoot,
      encoding: 'utf8',
      shell: false,
      env: { ...process.env, NYSIA_PROVE_TS_DRIFT_FAULT: 'after-mutation' },
    });

    const after = readFileSync(identity);
    const output = (result.stdout ?? '') + (result.stderr ?? '');

    // The mutation really happened...
    expect(output).toContain('prove:ts-drift: mutated');
    // ...the proof really reported failure...
    expect(result.status).toBe(1);
    expect(output).toContain('prove:ts-drift FAILED');
    // ...and the file came back exactly as it was. Bytes, not a decoded string: a CRLF
    // checkout rewritten to LF would compare equal as text and still be a dirty tree.
    expect(after.equals(before)).toBe(true);
  }, 120_000);
});
