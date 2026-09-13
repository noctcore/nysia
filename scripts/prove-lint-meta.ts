/**
 * Proof that both lint-meta architecture rules trip (traps register #13).
 *
 * `pnpm lint` runs the rules against the real repository, where they are expected to find
 * nothing — which on its own proves nothing at all. This points the same runner at
 * `tools/lint-meta/fixtures/trips`, where each rule is broken exactly once, and at
 * `fixtures/clean`, which exercises every carve-out the rules allow.
 */
import { join } from 'node:path';
import process from 'node:process';

import { runRules, type Violation } from '../tools/lint-meta/src/rules.ts';
import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const repoRoot = findRepoRoot();
const fixture = (name: string): string => join(repoRoot, 'tools', 'lint-meta', 'fixtures', name);

const failures: string[] = [];

function expectRule(violations: readonly Violation[], rule: string): void {
  const hit = violations.filter((v) => v.rule === rule);
  if (hit.length === 0) {
    failures.push(`rule \`${rule}\` did not trip on the violating fixture`);
    return;
  }
  process.stdout.write(`  trips: ${rule} -> ${hit[0]?.file}\n`);
}

process.stdout.write('prove:lint-meta\n');

const trips = runRules(fixture('trips'));
expectRule(trips, 'no-tauri-outside-desktop');
expectRule(trips, 'core-declares-no-tauri');

const clean = runRules(fixture('clean'));
if (clean.length > 0) {
  failures.push(
    `the clean fixture reported ${clean.length} violation(s): ` +
      clean.map((v) => `${v.file} ${v.rule}`).join(', '),
  );
} else {
  process.stdout.write('  clean: apps/desktop and apps/web/src/transport carve-outs hold\n');
}

// The repo-wide scan skips the fixtures. If that exclusion ever widened, the rules would
// stop seeing real code, so assert the real tree is still being scanned.
const scanned = runRules(repoRoot);
if (scanned.length > 0) {
  failures.push(
    `the repository itself has ${scanned.length} violation(s): ` +
      scanned.map((v) => `${v.file} ${v.rule}`).join(', '),
  );
}

if (failures.length > 0) {
  for (const failure of failures) process.stderr.write(`prove:lint-meta FAILED — ${failure}\n`);
  process.exit(1);
}

process.stdout.write('prove:lint-meta OK — both rules trip, both carve-outs hold\n');
