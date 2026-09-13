/**
 * The architecture-rule runner, wired into `pnpm lint`.
 *
 * Three rules today (D-14 keeps the set minimal and lets it ratchet):
 *
 *   a. nothing outside `apps/desktop` and `apps/web/src/transport` may import tauri;
 *   b. no Rust crate outside `apps/desktop` may declare a tauri dependency;
 *   c. `nysia-core` must not reach tauri through another Nysia crate.
 *
 * Each ships a fixture proving it trips — `pnpm prove:lint-meta`. A check that passes
 * without exercising anything is worse than no check (traps register #13).
 *
 * Usage: `node tools/lint-meta/src/cli.ts [root]`
 */
import { resolve } from 'node:path';
import process from 'node:process';

import { findRepoRoot } from './repoRoot.ts';
import { runRules } from './rules.ts';

const argument = process.argv[2];
const root = argument === undefined ? findRepoRoot() : resolve(argument);
const includeFixtures = process.argv.includes('--include-fixtures');

const violations = runRules(root, includeFixtures);

if (violations.length === 0) {
  process.stdout.write('lint-meta: 3 rules, 0 violations\n');
  process.exit(0);
}

for (const violation of violations) {
  const where = violation.line > 0 ? `${violation.file}:${violation.line}` : violation.file;
  process.stderr.write(`${where}  ${violation.rule}  ${violation.message}\n`);
}
process.stderr.write(`lint-meta: ${violations.length} violation(s)\n`);
process.exit(1);
