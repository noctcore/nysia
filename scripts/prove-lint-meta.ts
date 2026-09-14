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

// An astral char literal is two UTF-16 code units, so a pattern matching one leaves its
// closing quote to pair with the comma and the orphan opens a string that swallows the
// import below. Each spelling sits alone in its file: a later quote anywhere would close the
// runaway string early and rescue the import, which is how a first attempt at this fixture
// passed while the defect was still there.
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/astral_char.rs', 10);
expectLine(trips, 'no-tauri-outside-desktop', 'crates/nysia/src/astral_matches.rs', 7);

// A multi-line grouped import in a .js file, and a `require()` in a .cjs one. ESLint's ban
// blocks were {ts,tsx} only, and `no-restricted-imports` never covered `require` at all.
expectFile(trips, 'no-tauri-outside-desktop', 'apps/web/src/legacy.js');
expectFile(trips, 'no-tauri-outside-desktop', 'apps/web/src/legacy.cjs');

// ---------------------------------------------------------------------------------------
// Rule (d) — reaching StoreContext through a call rather than an import statement.
//
// ESLint owns `import` and `export … from`; it cannot see `require()` or dynamic
// `import()`, which `eslint.config.js` says in its own header are lint-meta's half. Until
// rule (d) existed that half was missing, so a top-level
// `await import('../store/StoreContext')` reached the raw provider while passing typecheck,
// eslint, lint-meta and the Vite build.
// ---------------------------------------------------------------------------------------
expectRule(trips, 'no-store-context-outside-store');

// Both spellings, at their exact lines. The second puts a segment between `store` and the
// filename, which the ESLint patterns deliberately tolerate — a rule here that required the
// two to be adjacent would report the first and miss the second.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/dynstore.ts', 10); // await import(...)
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/dynstore.ts', 11); // require('../store/./StoreContext')

// #20. The two allowlists used to be independent copies, and they already disagreed:
// lint-meta allowlisted by the prefix `apps/web/src/main.` and ESLint carved out
// `main.{ts,tsx,…}`, so a file named `main.helper.tsx` was inside one and outside the other
// and a dynamic import of the provider from it tripped nothing. Both layers now read the
// same boundary list, where the carve-out is the entry-point file in whichever extension it
// carries — not everything whose name begins with it.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/main.helper.tsx', 12);

// #19. Four more spellings of the same call, each of which passed lint-meta, ESLint, tsc
// *and* the Vite build while the rule claimed to match any path ending in `StoreContext`.
// Exact lines, because the locator is half of what makes the rule usable.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/dyn-template.ts', 5);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/dyn-multiline.ts', 6);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/dyn-blockcomment.ts', 5);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/dyn-glob.ts', 5);

// Every spelling that defeated the hand-written scanner this rule used to be, kept as
// regression cases. A string holding a comment opener, a regex holding a backtick, a
// substitution holding one: each blanked the call below it out of existence while the gate
// reported success, and each took a round of review to find. None of them is a case for a
// parser — a comment is trivia and a string is a string — which is the argument for the
// parser, so they stay as the record of what a scanner costs.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/str-block-open.ts', 9);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/str-line-open.ts', 4);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/str-template-open.ts', 4);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/str-escaped-quote.ts', 5);

// The array form of a glob pattern, which Vite documents as first-class. The first version
// of the glob check read one literal out of the call and decided the whole call on it, so a
// stylesheet — or a negation naming one — in front of the store exempted a glob that returns
// the provider. A rule written to close "narrower than its words" was narrower than its
// words, and its proof never noticed because it only ever passed a single-literal call. Both
// cases below put the innocent pattern FIRST, which is the input that got through.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-array-first-innocent.ts', 10);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-array-leading-negation.ts', 4);

// Two more ways to leak a `/*` into the scan, both of which end in the same runaway comment
// blanking every line below. A backtick is the third quoting character and a template may
// span lines, so neither is covered by the same-line rule that defuses `'` and `"`.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/regex-backtick.ts', 13);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/template-nested.ts', 9);

// The two that ended the hand-written scanner.
//
// A `?` in a glob is the single-character wildcard, not a URL query separator, so reading an
// extension out of the pattern by hand exempted `*.t?x` while the matcher returns
// `StoreProvider.tsx`. The glob question is put to a matcher over the real tree now, and
// reverting that turns this case red on its own.
//
// The second is an ordinary component line — two sibling elements, the second with a
// template prop holding a Tailwind fraction. The self-closing slash opened a regex scan that
// swallowed the template's opening backtick, and everything after it was read one quote out
// of step until the import vanished. There is nothing left to revert for that one: the
// scanner is deleted rather than patched, which is the point.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-wildcard-question.ts', 7);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/jsx-sibling-template.tsx', 10);

// The fail-closed inversion, which is what these four are for.
//
// The rule used to enumerate the ways a call might be safe and exempt them, so every option
// and spelling nobody had thought of failed OPEN — and Vite's option surface is Vite's to
// change. Each of these was a clean report from a rule that had guessed wrong about it:
// `{ query: { raw: true } }` becomes `?raw=true`, which is not Vite's raw flag; `base` and
// `caseSensitive` both change which files a pattern reaches; and three ordinary expression
// wrappers left a specifier looking computed. A glob is reported now unless the call is
// provably incapable, where "provably" is a list verified by executing the pinned Vite.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-raw-object-true.ts', 10);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-case-insensitive.ts', 3);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/panes/glob-base-option.tsx', 7);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/wrapped-specifiers.ts', 4);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/wrapped-specifiers.ts', 5);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/wrapped-specifiers.ts', 6);

// The other half of the inversion, and the half that was missed: the OPTIONS were inverted
// and the PATTERN was left hand-modelled. Every pattern was joined onto the importing file's
// directory, which anchors it; Vite hands a pattern beginning with a double star to the
// globber untouched and walks it from the filesystem root. A pattern the rule cannot anchor
// is now a pattern it refuses to judge.
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-unanchored-pattern.ts', 13);
expectLine(trips, 'no-store-context-outside-store', 'apps/web/src/chrome/glob-unanchored-array.ts', 3);

// ---------------------------------------------------------------------------------------
// Rule (e) — a module that builds a terminal without muting the replies it would send.
//
// This one exists because the line it guards is the line nothing executes. `surface/xterm.ts`
// needs a DOM and a canvas to construct, v0.1's tests are node-only (D-18), and so deleting
// `muteTerminalReplies(terminal.parser)` from it left `pnpm test` green at 613/613 — and
// deleting the orphaned import with it left every gate green. The table the call applies is
// covered by unit tests; the call was covered by nothing.
// ---------------------------------------------------------------------------------------
expectRule(trips, 'renderer-must-mute-replies');

// The exact line of the import that obliges the module, because the locator is half of what
// makes a rule usable — and because the fixture names `muteTerminalReplies` in a comment
// ABOVE it. The real file discusses the mute in prose too, so a rule that searched the text
// rather than the tree would count that sentence and pass the module that never calls it.
expectLine(
  trips,
  'renderer-must-mute-replies',
  'apps/web/src/transport/surface/xterm.ts',
  8,
);
expectMessage(trips, 'renderer-must-mute-replies', 'never calls muteTerminalReplies()');

// And the module that constructs nothing and is reported anyway: a bare re-export hands the
// constructor on under this module's name, so whoever builds a terminal from it names
// `./reexport` and a rule that matches on specifiers stops seeing the library at all.
expectLine(
  trips,
  'renderer-must-mute-replies',
  'apps/web/src/transport/surface/reexport.ts',
  7,
);
expectMessage(trips, 'renderer-must-mute-replies', 're-exports Terminal');

// And the renderer that never writes the name down: a dynamic import hands over the whole
// module, so scoping the obligation to a bound `Terminal` would have gone quiet on the
// ordinary way to split a heavy renderer out of the main bundle. Obliged on the reach alone.
expectLine(trips, 'renderer-must-mute-replies', 'apps/web/src/transport/surface/lazy.ts', 6);
expectMessage(trips, 'renderer-must-mute-replies', 'loads @xterm/xterm at runtime');

expectClean(runSourceRules(fixture('clean')), 'the clean source fixture');
process.stdout.write('  clean: apps/desktop and apps/web/src/transport carve-outs hold\n');
// The clean fixture also reaches StoreContext by call from store/ and from main.tsx. If
// rule (d) stopped honouring its allowlist those two would report and the line above would
// fail, which is what stops the carve-out silently becoming a ban.
process.stdout.write('  clean: store/** and main.tsx may reach the provider by call\n');
// And the two shapes rule (d) must not report, both of which `apps/web` really contains:
// comments that discuss the ban in the exact words of the ban, and the glob imports that
// read source text or stylesheets rather than modules. Without these the rule could be
// widened until it reported everything, which is the other way to stop being a gate.
process.stdout.write('  clean: comments may discuss the ban, raw and css globs may run\n');
// And rule (e)'s carve-out: the clean tree builds a terminal *and* mutes it, and its
// stylesheet import — `@xterm/xterm/css/xterm.css` — must not read as a second module that
// builds one. A rule matching every `@xterm/xterm` subpath would oblige a stylesheet to call
// a function, and a clean fixture with no stylesheet in it would never have said so.
process.stdout.write('  clean: a muted terminal passes, and a stylesheet builds nothing\n');
// And the three modules rule (e) must not oblige, which are the over-reports it shipped
// with: `import type { Terminal }`, the per-specifier `{ type Terminal }` that
// `verbatimModuleSyntax` keeps as a statement, and a script that reaches past the package
// for `EscapeSequenceParser` alone. None of the three builds a terminal, and lint-meta has
// no suppression mechanism, so a module reported here has no way out but to stop importing
// the thing it needs.
process.stdout.write('  clean: a type-only import and a lone parser oblige nothing\n');

// ---------------------------------------------------------------------------------------
// Rules (b) and (c) — cargo's own resolution of a real workspace.
// ---------------------------------------------------------------------------------------
const cargoTrips = runCargoRules(fixture('cargo', 'violating'));
expectRule(cargoTrips, 'no-tauri-in-rust-crates');
expectRule(cargoTrips, 'no-tauri-reaching-rust-crates');

// A crate can reach tauri straight out of [workspace.dependencies] without editing a shared
// file, so every crate is inspected and not only the chain rooted at nysia-core. Here it is
// declared through a rename under a quoted table header.
expectFile(cargoTrips, 'no-tauri-in-rust-crates', 'crates/nysia-hook/Cargo.toml');
expectMessage(cargoTrips, 'no-tauri-in-rust-crates', 'package = "tauri"');

// The other half of a rename: the key says tauri and the crate behind it does not. That
// makes `use tauri::…` compile here and reads as a tauri dependency to anyone skimming the
// manifest, and only the key gives it away — origin/main's key-based parser caught it and
// the resolved-name check did not.
expectFile(cargoTrips, 'no-tauri-in-rust-crates', 'crates/nysia-shim/Cargo.toml');
expectMessage(cargoTrips, 'no-tauri-in-rust-crates', '`tauri` (package = "third")');

// Declared under a quoted key, in a `[ dependencies ]` header carrying whitespace and a
// trailing comment. The hand-written parser reported 0 violations for every one of these.
expectFile(cargoTrips, 'no-tauri-in-rust-crates', 'crates/nysia-proto/Cargo.toml');

// nysia-core's manifest carries a trailing comment after `[package]`. The old parser could
// not read its name and dropped that crate from the scan entirely, so rules (b) and (c) went
// blind on it; cargo has no such trouble and the chain through it is visible.
expectMessage(cargoTrips, 'no-tauri-reaching-rust-crates', 'nysia-core -> nysia-proto -> tauri');

// The daemon binary reaching tauri through apps/desktop. Nothing in its manifest names
// tauri, so rule (b) cannot see it, and seeding the reach search from nysia-core alone left
// this crate — the runtime D-1 protects — able to link the UI toolkit with nothing tripping.
expectFile(cargoTrips, 'no-tauri-reaching-rust-crates', 'crates/nysia/Cargo.toml');
expectMessage(cargoTrips, 'no-tauri-reaching-rust-crates', 'nysia -> nysia-desktop -> tauri');

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
