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
 * # Where the boundary actually is
 *
 * These rules are greps over text, not a compiler, and the honest thing is to say what they
 * still cannot see rather than let the next reader assume they are airtight. Each of these
 * is a deliberate limit, not an oversight:
 *
 * - **A path that only exists after macro expansion is invisible**, `include!` included.
 *   Rust is scanned as text with comments and string literals blanked — which is why
 *   `tauri::` mentioned in a string is correctly ignored — but nothing here expands macros.
 * - **A dependency renamed in `Cargo.lock` rather than in a manifest is invisible.** The
 *   manifest scan resolves `ui = { package = "tauri" }` in both spellings, but a path or
 *   git dependency whose own manifest renames itself again would need the lock graph, which
 *   is out of scope for a text rule. `cargo tree -i tauri` is the check that catches it.
 * - **A file ESLint's `ignores` excludes is covered only by rule (a)'s line-based scan**,
 *   which is weaker than ESLint's AST. The two layers are deliberately different: ESLint
 *   owns the TypeScript and JavaScript boundary properly, lint-meta is the backstop for
 *   Rust and for anything ESLint does not reach.
 *
 * The allowlist itself is duplicated in `eslint.config.js`, and the two must stay in
 * agreement — if they disagree, a later wave fails a gate it cannot fix without editing
 * coordinator-owned config.
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
