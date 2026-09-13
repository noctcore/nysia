/**
 * Proof that every lint-meta architecture rule trips (traps register #13).
 *
 * `pnpm lint` runs the rules against the real repository, where they are expected to find
 * nothing — which on its own proves nothing at all. This points the same runner at the
 * fixture trees: `trips`, where each rule is broken directly; `trips-transitive`, which
 * exercises the indirect branch of the crate rule; and `clean`, which exercises every
 * carve-out the rules allow.
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

process.stdout.write('prove:lint-meta\n');

const trips = runRules(fixture('trips'));
expectRule(trips, 'no-tauri-outside-desktop');
expectRule(trips, 'no-tauri-in-rust-crates');

// crates/nysia can reach tauri straight out of [workspace.dependencies] without editing a
// single shared file, so the rule has to inspect every crate and not only the dependency
// chain rooted at nysia-core.
expectFile(trips, 'no-tauri-in-rust-crates', 'crates/nysia/Cargo.toml');

// The three Rust spellings a line-anchored `use tauri::` regex walked straight past. Exact
// lines, so the locator is proven and not just the boolean.
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/leak.rs', 4); // use ::tauri::Builder;
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/leak.rs', 7); // use {tauri, serde};
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/leak.rs', 10); // the multi-line form

// A multi-line grouped import in a .js file. ESLint's ban blocks were {ts,tsx} only and
// the regex was line-anchored, so this spelling was covered by neither layer.
expectFile(trips, 'no-tauri-outside-desktop', 'apps/web/src/legacy.js');

// A renamed dependency pulls the same crate under another key; reading keys alone missed it.
expectMessage(trips, 'no-tauri-in-rust-crates', 'package = "tauri"');

// The transitive branch has its own fixture: nysia-core is clean and the violation arrives
// through nysia-proto. Without it, that branch is code nobody has watched fail.
const transitive = runRules(fixture('trips-transitive'));
expectRule(transitive, 'no-tauri-reaching-core');
expectFile(transitive, 'no-tauri-in-rust-crates', 'crates/nysia-proto/Cargo.toml');
expectMessage(transitive, 'no-tauri-reaching-core', 'nysia-core -> nysia-proto');

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

process.stdout.write('prove:lint-meta OK — every rule trips, every carve-out holds\n');
