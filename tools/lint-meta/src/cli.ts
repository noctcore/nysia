/**
 * The architecture-rule runner, wired into `pnpm lint`.
 *
 * Three rules today (D-14 keeps the set minimal and lets it ratchet):
 *
 *   a. nothing outside `apps/desktop` and `apps/web/src/transport` may import tauri;
 *   b. no Rust crate outside `apps/desktop` may depend on tauri;
 *   c. `nysia-core` must not reach tauri through anything.
 *
 * (b) is the load-bearing one: if no crate outside `apps/desktop` depends on tauri then
 * `use tauri::…` there cannot compile, which makes (a) belt and braces. (a) is kept anyway
 * because it reports a file and a line a developer can act on in ten seconds, where (b)
 * reports a manifest.
 *
 * Each ships a fixture proving it trips — `pnpm prove:lint-meta`. A check that passes
 * without exercising anything is worse than no check (traps register #13).
 *
 * Exit codes: 0 clean, 1 violations, **2 the rules could not run**. Rules (b) and (c) shell
 * out to `cargo metadata`; if that fails this exits 2 rather than reporting zero.
 *
 * # Where the boundary actually is
 *
 * Rules (b) and (c) read `cargo metadata`, so they see exactly what cargo compiles: quoting,
 * comments, whitespace, renames and workspace inheritance are cargo's problem, not ours.
 * Rule (a) is a text scan, and the honest thing is to say what it cannot see:
 *
 * - **A path that only exists after macro expansion is invisible**, `include!` included.
 *   Rust is scanned with comments and string literals blanked — which is why a `tauri::`
 *   mentioned in a string is correctly ignored — but nothing here expands macros. Rule (b)
 *   still catches the dependency that would make such a path compile.
 * - **`no-restricted-imports` does not cover `require()`.** ESLint owns `import` and
 *   `export … from`; lint-meta owns `require()`, dynamic `import()` and `import.meta.glob`.
 *   Neither layer is complete alone, and that split is deliberate rather than an oversight.
 * - **Rule (d) reads the specifier, so a specifier that is not a literal is invisible.**
 *   `import(name)`, `import('../store/' + name)` and a template with a `${…}` in it are all
 *   beyond it, and ESLint is equally blind to the static equivalents. This used to be
 *   written as "lint-meta owns dynamic `import()`" with no qualification while the rule
 *   matched only a single-line quoted literal, which over-claimed four spellings that
 *   passed every gate (#19); the rule now reads whole files with comments blanked, and
 *   `import.meta.glob` — which need not name the file at all — is reported unless the call
 *   shows it cannot return a module.
 * - **A file ESLint's `ignores` excludes is covered only by rule (a)'s line scan**, which is
 *   weaker than ESLint's AST.
 *
 * The allowlists themselves live in `tools/lint-meta/src/boundaries.ts`, which
 * `eslint.config.js` reads too. They used to be two hand-written copies described as
 * mirrored, and they had already drifted apart (#20): a file was allowlisted by one layer
 * and banned by the other, and nothing said so. `pnpm prove:eslint-bans` now lints a set of
 * edge paths through both layers and fails if they disagree.
 *
 * Usage: `node tools/lint-meta/src/cli.ts [root]`
 */
import { resolve } from 'node:path';
import process from 'node:process';

import { findRepoRoot } from './repoRoot.ts';
import { CargoMetadataError } from './cargoGraph.ts';
import { runAllRules, runSourceRules } from './rules.ts';

const argument = process.argv[2];
const root = argument === undefined ? findRepoRoot() : resolve(argument);
const includeFixtures = process.argv.includes('--include-fixtures');
/** The source rules alone — (a) and (d) — for a tree that is not a cargo workspace. */
const sourceOnly = process.argv.includes('--source-only');

let violations;
try {
  violations = sourceOnly
    ? runSourceRules(root, includeFixtures)
    : runAllRules(root, includeFixtures);
} catch (error) {
  // Exit 2, never 0. Rules (b) and (c) read cargo's resolved graph, and a rule that could
  // not run reporting "0 violations" is the exact failure this tool exists to catch.
  if (error instanceof CargoMetadataError) {
    process.stderr.write(`lint-meta: the dependency rules could not run\n${error.message}\n`);
    process.exit(2);
  }
  throw error;
}

if (violations.length === 0) {
  // Two source rules (tauri imports, store-context calls); the cargo rules add two more.
  process.stdout.write(`lint-meta: ${sourceOnly ? 2 : 4} rules, 0 violations\n`);
  process.exit(0);
}

for (const violation of violations) {
  const where = violation.line > 0 ? `${violation.file}:${violation.line}` : violation.file;
  process.stderr.write(`${where}  ${violation.rule}  ${violation.message}\n`);
}
process.stderr.write(`lint-meta: ${violations.length} violation(s)\n`);
process.exit(1);
