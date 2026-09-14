/**
 * The architecture-rule runner, wired into `pnpm lint`.
 *
 * Five rules today (D-14 keeps the set minimal and lets it ratchet):
 *
 *   a. nothing outside `apps/desktop` and `apps/web/src/transport` may import tauri;
 *   b. no Rust crate outside `apps/desktop` may depend on tauri;
 *   c. `nysia-core` must not reach tauri through anything;
 *   d. nothing outside the store may reach `StoreContext` through a call;
 *   e. a module that builds a terminal must mute the replies it would otherwise send.
 *
 * (b) is the load-bearing one: if no crate outside `apps/desktop` depends on tauri then
 * `use tauri::…` there cannot compile, which makes (a) belt and braces. (a) is kept anyway
 * because it reports a file and a line a developer can act on in ten seconds, where (b)
 * reports a manifest.
 *
 * Each ships a fixture proving it trips — `pnpm prove:lint-meta`. A check that passes
 * without exercising anything is worse than no check (traps register #13). Rule (e) is here
 * for a reason worth naming: the module that builds the real terminal needs a DOM and a
 * canvas, v0.1's tests are node-only (D-18), and so the line that mutes it was the one line
 * in the repository nothing executed — deleting it left every gate green.
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
 * - **The TypeScript and JavaScript rules parse; they no longer scan.** Three rounds of
 *   review found a spelling the hand-written scanner mishandled, and each fix introduced the
 *   next — the last was an ordinary component line whose self-closing slash opened a regex
 *   scan and blanked a store import out of existence. `moduleReferences.ts` walks a syntax
 *   tree instead, where a comment is trivia and a string is not an import.
 * - **Rule (d) reads the specifier, so a specifier that is not a literal is invisible.**
 *   `import(name)`, `import('../store/' + name)` and a template with a `${…}` in it are all
 *   beyond it, and ESLint is equally blind to the static equivalents. This used to be
 *   written as "lint-meta owns dynamic `import()`" with no qualification while the rule
 *   matched only a single-line quoted literal, which over-claimed four spellings that
 *   passed every gate (#19); the rule reads a syntax tree now, and
 *   `import.meta.glob` — which need not name the file at all — is reported unless **every**
 *   pattern in the call reaches no module, asked of a glob matcher over the files that
 *   exist. Both halves of that were learned the hard way: reading one pattern out of an
 *   array let a stylesheet in front of the store exempt a glob that returned the provider,
 *   and reading the extension out of a pattern by hand split `*.t?x` on the question mark as
 *   though a glob carried a URL query, where it is the single-character wildcard.
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
  // Three source rules (tauri imports, store-context calls, an unmuted renderer); the cargo
  // rules add two more.
  process.stdout.write(`lint-meta: ${sourceOnly ? 3 : 5} rules, 0 violations\n`);
  process.exit(0);
}

for (const violation of violations) {
  const where = violation.line > 0 ? `${violation.file}:${violation.line}` : violation.file;
  process.stderr.write(`${where}  ${violation.rule}  ${violation.message}\n`);
}
process.stderr.write(`lint-meta: ${violations.length} violation(s)\n`);
process.exit(1);
