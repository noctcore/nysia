/**
 * Proof that every lint-meta architecture rule trips (traps register #13).
 *
 * `pnpm lint` runs the rules against the real repository, where they are expected to find
 * nothing — which on its own proves nothing at all. This points the same rules at fixtures
 * built to break them:
 *
 * - `fixtures/trips` and `fixtures/clean` — source trees for rule (a). Every literal form
 *   and the astral characters sit **above** the asserted imports, so removing the literal
 *   handling or the code-unit indexing makes this proof red rather than only the unit tests.
 * - `fixtures/cargo/violating` and `fixtures/cargo/clean` — real cargo workspaces, resolved
 *   offline through path dependencies on a stub crate named `tauri`, for rules (b) and (c).
 * - `fixtures/cargo/unresolvable` — a workspace cargo cannot read, to prove the rules
 *   report that they could not run instead of reporting zero violations.
 */
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import process from 'node:process';

import { runCargoRules, runSourceRules, type Violation } from '../tools/lint-meta/src/rules.ts';
import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const repoRoot = findRepoRoot();
const fixture = (...parts: string[]): string =>
  join(repoRoot, 'tools', 'lint-meta', 'fixtures', ...parts);

const failures: string[] = [];

function expectRule(violations: readonly Violation[], rule: string): void {
  const hit = violations.filter((v) => v.rule === rule);
  if (hit.length === 0) {
    failures.push(`rule \`${rule}\` did not trip on the violating fixture`);
    return;
  }
  const where = [...new Set(hit.map((v) => v.file))].join(', ');
  process.stdout.write(`  trips: ${rule} -> ${where} (${hit.length})\n`);
}

/** The rule tripped, and it tripped on this exact file. */
function expectFile(violations: readonly Violation[], rule: string, file: string): void {
  if (!violations.some((v) => v.rule === rule && v.file === file)) {
    failures.push(`rule \`${rule}\` did not trip on ${file}`);
  }
}

/** The rule tripped on this exact file *and* line, so the locator is real. */
function expectLine(
  violations: readonly Violation[],
  rule: string,
  file: string,
  line: number,
): void {
  if (!violations.some((v) => v.rule === rule && v.file === file && v.line === line)) {
    const seen = violations
      .filter((v) => v.rule === rule && v.file === file)
      .map((v) => v.line)
      .join(', ');
    failures.push(`rule \`${rule}\` missed ${file}:${line} (reported lines: ${seen || 'none'})`);
  }
}

/** The rule tripped and its message carries the detail that makes it actionable. */
function expectMessage(violations: readonly Violation[], rule: string, fragment: string): void {
  if (!violations.some((v) => v.rule === rule && v.message.includes(fragment))) {
    failures.push(`rule \`${rule}\` never reported ${JSON.stringify(fragment)}`);
  }
}

function expectClean(violations: readonly Violation[], what: string): void {
  if (violations.length > 0) {
    failures.push(
      `${what} reported ${violations.length} violation(s): ` +
        violations.map((v) => `${v.file} ${v.rule}`).join(', '),
    );
  }
}

process.stdout.write('prove:lint-meta\n');

// ---------------------------------------------------------------------------------------
// Rule (a) — the Rust and TypeScript source scan.
// ---------------------------------------------------------------------------------------
const trips = runSourceRules(fixture('trips'));
expectRule(trips, 'no-tauri-outside-desktop');

// The three Rust spellings a line-anchored `use tauri::` regex walked straight past. Exact
// lines, so the locator is proven and not just the boolean — and every literal form and the
// astral characters that must not hide them sit above these lines in the fixture.
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/leak.rs', 22); // use ::tauri::Builder;
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/leak.rs', 25); // use {tauri, serde};
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/leak.rs', 28); // the multi-line form

// A multi-line grouped import in a .js file, and a `require()` in a .cjs one. ESLint's ban
// blocks were {ts,tsx} only, and `no-restricted-imports` never covered `require` at all.
expectFile(trips, 'no-tauri-outside-desktop', 'apps/web/src/legacy.js');
expectFile(trips, 'no-tauri-outside-desktop', 'apps/web/src/legacy.cjs');

expectClean(runSourceRules(fixture('clean')), 'the clean source fixture');
process.stdout.write('  clean: apps/desktop and apps/web/src/transport carve-outs hold\n');

// ---------------------------------------------------------------------------------------
// Rules (b) and (c) — cargo's own resolution of a real workspace.
// ---------------------------------------------------------------------------------------
const cargoTrips = runCargoRules(fixture('cargo', 'violating'));
expectRule(cargoTrips, 'no-tauri-in-rust-crates');
expectRule(cargoTrips, 'no-tauri-reaching-core');

// crates/nysia can reach tauri straight out of [workspace.dependencies] without editing a
// shared file, so every crate is inspected and not only the chain rooted at nysia-core.
// Here it is declared through a rename under a quoted table header.
expectFile(cargoTrips, 'no-tauri-in-rust-crates', 'crates/nysia/Cargo.toml');
expectMessage(cargoTrips, 'no-tauri-in-rust-crates', 'package = "tauri"');

// Declared under a quoted key, in a `[ dependencies ]` header carrying whitespace and a
// trailing comment. The hand-written parser reported 0 violations for every one of these.
expectFile(cargoTrips, 'no-tauri-in-rust-crates', 'crates/nysia-proto/Cargo.toml');

// nysia-core's manifest carries a trailing comment after `[package]`. The old parser could
// not read its name and dropped that crate from the scan entirely, so rules (b) and (c) went
// blind on it; cargo has no such trouble and the chain through it is visible.
expectMessage(cargoTrips, 'no-tauri-reaching-core', 'nysia-core -> nysia-proto -> tauri');

// Reading the graph rather than every Cargo.toml on disk narrows the rule in one way: a
// crate that is not a workspace member never appears in it. Reported rather than skipped,
// because "the rule stopped looking and said nothing" is the whole failure class here.
expectFile(cargoTrips, 'no-tauri-in-rust-crates', 'crates/orphan/Cargo.toml');
expectMessage(cargoTrips, 'no-tauri-in-rust-crates', 'is not a workspace member');

expectClean(runCargoRules(fixture('cargo', 'clean')), 'the clean cargo workspace');
process.stdout.write('  clean: apps/desktop may link tauri in its own manifest\n');

// A rule that cannot run must say so. This is the failure mode the whole round is about:
// reporting "0 violations" because the check never happened.
const unreadable = spawnSync(
  process.execPath,
  [join(repoRoot, 'tools', 'lint-meta', 'src', 'cli.ts'), fixture('cargo', 'unresolvable')],
  { cwd: repoRoot, encoding: 'utf8', shell: false },
);
if (unreadable.status !== 2) {
  failures.push(
    `an unreadable workspace exited ${unreadable.status}, not 2 — a dependency rule that ` +
      'could not run must never look like a clean one',
  );
} else {
  process.stdout.write('  reports failure: an unreadable workspace exits 2, not 0\n');
}

// ---------------------------------------------------------------------------------------
// The repo-wide scan skips the fixtures. If that exclusion ever widened, the rules would
// stop seeing real code, so assert the real tree is still scanned by both halves.
// ---------------------------------------------------------------------------------------
expectClean(runSourceRules(repoRoot), 'the repository source scan');
expectClean(runCargoRules(repoRoot), 'the repository dependency graph');

if (failures.length > 0) {
  for (const failure of failures) process.stderr.write(`prove:lint-meta FAILED — ${failure}\n`);
  process.exit(1);
}

process.stdout.write('prove:lint-meta OK — every rule trips, every carve-out holds\n');
