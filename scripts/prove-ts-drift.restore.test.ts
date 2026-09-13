import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';
import { describe, expect, it } from 'vitest';

import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const repoRoot = findRepoRoot();
const identity = join(repoRoot, 'crates', 'nysia-proto', 'src', 'identity.rs');
const frame = join(repoRoot, 'crates', 'nysia-proto', 'src', 'frame.rs');
const proof = join(repoRoot, 'scripts', 'prove-ts-drift.ts');

/**
 * `scripts/prove-ts-drift.ts` mutates tracked source files and puts them back. Its restore
 * used to live in a `finally` that the post-mutation failure paths never reached, because
 * they called `process.exit`, which terminates without unwinding — so a developer whose
 * proof failed was left with a modified `crates/nysia-proto/src/identity.rs` and a doc
 * comment claiming otherwise.
 *
 * The fault seam makes those exact paths run on demand, and there is **one seam value per
 * probe**. A seam that only covered the first probe would leave every later restore tested
 * by nothing: the second probe writes to `frame.rs`, and until this test existed the only
 * evidence that path restored was someone having tried it by hand.
 *
 * Each case asserts more than "the file it mutated came back". It asserts *both* files are
 * byte-identical, because the `finally` restores every probe unconditionally and a restore
 * that only covered the faulting file would be a different bug with the same symptom.
 *
 * Asserting only "the files are unchanged" would pass trivially if the mutation never
 * happened, so the marker on stderr is checked too: it is written immediately after the
 * probe's bytes hit the disk.
 */
const probes = [
  { fault: 'identity', file: identity, label: 'identity.rs' },
  { fault: 'frame', file: frame, label: 'frame.rs' },
] as const;

describe('the ts-rs drift proof', () => {
  for (const probe of probes) {
    it(`restores every source byte for byte when it fails after mutating ${probe.label}`, () => {
      const identityBefore = readFileSync(identity);
      const frameBefore = readFileSync(frame);

      const result = spawnSync(process.execPath, [proof], {
        cwd: repoRoot,
        encoding: 'utf8',
        shell: false,
        env: { ...process.env, NYSIA_PROVE_TS_DRIFT_FAULT: probe.fault },
      });

      const output = (result.stdout ?? '') + (result.stderr ?? '');

      // The mutation really happened, to the file this case is about...
      expect(output).toContain(`prove:ts-drift: mutated ${probe.file}`);
      // ...the proof really reported failure...
      expect(result.status).toBe(1);
      expect(output).toContain('prove:ts-drift FAILED');
      // ...and every probe file came back exactly as it was. Bytes, not decoded strings: a
      // CRLF checkout rewritten to LF would compare equal as text and still be a dirty tree.
      expect(readFileSync(identity).equals(identityBefore)).toBe(true);
      expect(readFileSync(frame).equals(frameBefore)).toBe(true);
    }, 120_000);
  }

  it('rejects a fault point that names no probe', () => {
    // Without this, a renamed probe would turn the cases above into a test of nothing: the
    // seam would go unrecognised, the script would run its normal course, and the only
    // symptom would be a confusing status mismatch rather than a named cause.
    const identityBefore = readFileSync(identity);
    const frameBefore = readFileSync(frame);

    const result = spawnSync(process.execPath, [proof], {
      cwd: repoRoot,
      encoding: 'utf8',
      shell: false,
      env: { ...process.env, NYSIA_PROVE_TS_DRIFT_FAULT: 'no-such-probe' },
    });

    const output = (result.stdout ?? '') + (result.stderr ?? '');

    expect(result.status).toBe(1);
    expect(output).toContain('unknown fault point no-such-probe');
    // Nothing was mutated, so nothing needed restoring — but assert it rather than assume.
    expect(output).not.toContain('prove:ts-drift: mutated');
    expect(readFileSync(identity).equals(identityBefore)).toBe(true);
    expect(readFileSync(frame).equals(frameBefore)).toBe(true);
  }, 120_000);
});
