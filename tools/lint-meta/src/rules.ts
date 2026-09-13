import { readFileSync, readdirSync } from 'node:fs';
import { join, posix, relative, sep } from 'node:path';

import {
  isTauriEdge,
  isTauriPackage,
  loadCargoWorkspace,
  type CargoDependencyEdge,
  type CargoWorkspace,
} from './cargoGraph.ts';

/**
 * A broken architecture rule, located precisely enough to click on.
 */
export interface Violation {
  readonly rule: string;
  /** Repo-relative, POSIX separators, so the output is identical on both runners. */
  readonly file: string;
  /** 1-indexed; 0 when the rule is about a file as a whole. */
  readonly line: number;
  readonly message: string;
}

/** Directories that never contain source we own. */
const SKIP_DIRS = new Set([
  '.git',
  'node_modules',
  'target',
  'dist',
  'bindings',
  'gen',
  '.pnpm-store',
]);

/**
 * lint-meta's own fixtures deliberately break both rules, so the repo-wide scan skips
 * them. `scripts/prove-lint-meta.ts` points the rules straight at that directory, which is
 * what keeps the exclusion honest: if a rule stopped tripping, the proof would fail.
 */
const FIXTURE_ROOT = 'tools/lint-meta/fixtures';

/**
 * Where Tauri may be imported.
 *
 * `apps/desktop` is the shell itself. `apps/web/src/transport` is the one module in the
 * webview allowed to touch the Tauri `Channel`; every other component reaches the daemon
 * through it. The ESLint `no-restricted-imports` ban set uses exactly this allowlist —
 * if the two disagree, a later wave fails a gate it cannot fix without editing shared
 * config.
 */
export const TAURI_ALLOWLIST: readonly string[] = [
  'apps/desktop/',
  'apps/web/src/transport/',
];

/**
 * Every extension that can carry an `import` or a `require` into a bundle, enumerated
 * deliberately rather than grown one at a time.
 *
 * In:  `.ts` `.tsx` `.mts` `.cts` `.js` `.jsx` `.mjs` `.cjs`, plus `.rs` for the Rust scan.
 * Out: `.json`, `.css`, `.html`, `.svg` — none of them can import anything.
 *
 * `.mts` and `.cts` matter specifically because Vite 8's default `resolve.extensions`
 * includes `.mts`, so such a file bundles. The same list is repeated verbatim in
 * `eslint.config.js`, which cannot import from here; if the two drift, a file is covered by
 * neither.
 */
const SOURCE_EXTENSIONS = [
  '.ts',
  '.tsx',
  '.mts',
  '.cts',
  '.js',
  '.jsx',
  '.mjs',
  '.cjs',
  '.rs',
];

/**
 * The import forms that actually pull Tauri into a module.
 *
 * Anchored at the start of the line on purpose: an unanchored `from '@tauri-apps/...'`
 * also matches the sample code inside `scripts/prove-eslint-bans.ts`, where those strings
 * are test input rather than imports. A rule that fires on its own proof is a rule people
 * switch off.
 */
const TS_TAURI_IMPORTS = [
  /^\s*import\s[^'"]*from\s*['"]@tauri-apps[^'"]*['"]/,
  /^\s*import\s*['"]@tauri-apps[^'"]*['"]/,
  /^\s*export\s[^'"]*from\s*['"]@tauri-apps[^'"]*['"]/,
  // The closing line of a multi-line named import: `}` then `from '@tauri-apps/...'`.
  /^\s*\}\s*from\s*['"]@tauri-apps[^'"]*['"]/,
  /(?:^|[^\w.$])import\s*\(\s*['"]@tauri-apps[^'"]*['"]/,
  /(?:^|[^\w.$])require\s*\(\s*['"]@tauri-apps[^'"]*['"]/,
];

/**
 * The crate root, in any of the spellings a `use` path can start with.
 *
 * `tauri`, and also `tauri_plugin_opener` and friends — a leading `::` is optional because
 * `use ::tauri::X;` is the same import written defensively. A lookbehind rather than a
 * consumed delimiter, for two reasons: the match index then points at the statement itself
 * so the line number is right, and refusing a preceding word character or colon keeps
 * `crate::tauri_helpers::X` a local module rather than a false positive.
 */
const RUST_TAURI_ROOT = String.raw`(?<![\w:])(?:::\s*)?tauri(?:_[A-Za-z0-9_]+)?`;

/** A whole `use` statement, however many lines it spans. */
const RUST_USE_STATEMENT = /(?<![\w:])(?:pub(?:\s*\([^)]*\))?\s+)?use\s[^;]*;/g;
/** The crate root appearing anywhere inside such a statement. */
const RUST_USE_NAMES_TAURI = new RegExp(
  `${RUST_TAURI_ROOT}\\s*(?:::|,|;|\\}|\\s+as\\b|$)`,
);
/** `extern crate tauri;` — 2015-edition spelling, still legal. */
const RUST_EXTERN_CRATE = /(?<![\w:])extern\s+crate\s+tauri(?:_[A-Za-z0-9_]+)?\b/g;
/** A fully-qualified path used without a `use`: `tauri::Builder::default()`. */
const RUST_TAURI_PATH = new RegExp(`${RUST_TAURI_ROOT}::`, 'g');

/**
 * Blank out Rust comments and string literals, preserving every byte position.
 *
 * Bodies become spaces and newlines are kept, so a match index still maps to the right
 * line. Three things depend on this:
 *
 * - the module docs in `nysia-core` discuss tauri constantly and must not trip the rule;
 * - a `;` inside a comment would otherwise truncate the `use` statement after it;
 * - **a string literal must not be able to open a comment.** `const OPEN: &str = "/*";`
 *   would otherwise start a block comment that never closes, and every `use tauri::…`
 *   below it in the file would be silently invisible. That is a false *negative*, and the
 *   worst kind: the gate keeps reporting success.
 *
 * String literals are therefore stepped over as units, in every spelling Rust has — see
 * `STRING_OPENER` for the enumeration — plus char literals, so that `'"'` is a character
 * and not the start of a string. Their contents are blanked as well, since a `tauri::`
 * inside a string is not an import.
 */
export function blankRustComments(source: string): string {
  // `split('')`, deliberately NOT `[...source]` or `Array.from(source)`.
  //
  // Those iterate by code point, giving one slot per character, while every index this
  // function computes — `i`, `source[at]`, `startsWith`, `indexOf`, `slice` — is a UTF-16
  // code unit. One astral character (any emoji) earlier in the file would shift every
  // later blank one slot right, cumulatively, and because `blank` skips newlines the drift
  // lands on real code: the first characters after each later comment get erased instead.
  // A single emoji in a doc header would quietly disarm this rule for the whole file.
  const out = source.split('');
  let i = 0;
  let blockDepth = 0;

  const blank = (at: number): void => {
    if (out[at] !== '\n') out[at] = ' ';
  };
  const blankTo = (end: number): void => {
    for (let k = i; k < end && k < source.length; k += 1) blank(k);
    i = Math.min(end, source.length);
  };
  const isWord = (at: number): boolean => {
    const c = source[at];
    return c !== undefined && /[A-Za-z0-9_]/.test(c);
  };

  /**
   * A char literal `'x'` / `'\n'` / `'"'` / `'🔥'`, as opposed to a lifetime `'a`.
   *
   * The surrogate-pair alternative is load-bearing and comes first. `[^'\\]` matches a
   * single UTF-16 code unit, so an astral char literal was not recognised: the closing
   * quote paired with whatever followed, the orphaned quote opened a string that ran to the
   * next one in the file, and every import between them was blanked. `['🔥','"']` and
   * `matches!(c, '🔥'|'"')` both erased every tauri import below them.
   *
   * The `u` flag would express this more neatly but cannot be used here: in unicode mode
   * the `\{` of the `\u{...}` escape alternative is an invalid identity escape.
   */
  const CHAR_LITERAL =
    /^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[\uD800-\uDBFF][\uDC00-\uDFFF]|[^'\\])'/;

  /**
   * Every string-literal opener Rust has, as one table rather than a chain of `if`s — the
   * `c` prefix was missing from the chain, and its absence meant `c"/*"` opened a block
   * comment that never closed and made the rest of the file invisible.
   *
   * In:  `"…"`  `b"…"`  `c"…"`  `r"…"`  `br"…"`  `cr"…"`  `r#"…"#`  `br#"…"#`  `cr#"…"#`
   * Out: nothing else. Rust has no `rb"` or `rc"`; the prefix always precedes the `r`.
   *
   * Sticky, so it can be anchored at the current index without slicing.
   */
  const STRING_OPENER = /(?:[bc]?r(#*)|[bc]?)"/y;

  while (i < source.length) {
    if (blockDepth > 0) {
      if (source.startsWith('/*', i)) {
        blockDepth += 1;
        blankTo(i + 2);
        continue;
      }
      if (source.startsWith('*/', i)) {
        blockDepth -= 1;
        blankTo(i + 2);
        continue;
      }
      blankTo(i + 1);
      continue;
    }

    if (source.startsWith('/*', i)) {
      blockDepth = 1;
      blankTo(i + 2);
      continue;
    }
    // Covers `//`, `///` and `//!` alike.
    if (source.startsWith('//', i)) {
      const newline = source.indexOf('\n', i);
      blankTo(newline === -1 ? source.length : newline);
      continue;
    }

    // A string literal in any of its nine spellings (see STRING_OPENER).
    if (!isWord(i - 1)) {
      STRING_OPENER.lastIndex = i;
      const opener = STRING_OPENER.exec(source);
      if (opener !== null) {
        const hashes = opener[1] ?? '';
        const bodyStart = i + opener[0].length;

        if (opener[0].includes('r')) {
          // Raw: no escapes, so the terminator is the quote plus the same hash count.
          const terminator = `"${hashes}`;
          const end = source.indexOf(terminator, bodyStart);
          blankTo(end === -1 ? source.length : end + terminator.length);
          continue;
        }

        // Non-raw: a backslash escapes the next character, including a quote.
        let k = bodyStart;
        while (k < source.length) {
          if (source[k] === '\\') {
            k += 2;
            continue;
          }
          if (source[k] === '"') {
            k += 1;
            break;
          }
          k += 1;
        }
        blankTo(k);
        continue;
      }
    }

    if (source[i] === "'") {
      const char = CHAR_LITERAL.exec(source.slice(i, i + 12));
      if (char !== null) {
        blankTo(i + char[0].length);
        continue;
      }
      // A lifetime: nothing to blank.
      i += 1;
      continue;
    }

    i += 1;
  }

  const blanked = out.join('');
  // The whole scanner is offset arithmetic, so this invariant is the thing that must never
  // quietly break. A rule that silently stops scanning is worse than no rule: it keeps
  // reporting success. If a future edit reintroduces code-point iteration, crash here
  // rather than start erasing code.
  if (blanked.length !== source.length) {
    throw new Error(
      `blankRustComments changed the length of its input (${source.length} -> ` +
        `${blanked.length}); every index in this function must be a UTF-16 code unit`,
    );
  }
  return blanked;
}

/** 1-indexed line of a byte offset. */
function lineAt(source: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index; i += 1) {
    if (source[i] === '\n') line += 1;
  }
  return line;
}

/**
 * Every line in a Rust file that pulls tauri in.
 *
 * Scanned per *statement* rather than per line, because a `use` can span lines and the
 * previous line-anchored regex walked straight past three legal spellings:
 * `use ::tauri::Builder;`, `use {tauri, serde};`, and the multi-line form of the second.
 * Any of them would have put the UI toolkit in the daemon with no gate tripping.
 */
function rustTauriLines(source: string): number[] {
  const code = blankRustComments(source);
  const lines = new Set<number>();

  for (const match of code.matchAll(RUST_USE_STATEMENT)) {
    if (match.index === undefined) continue;
    if (RUST_USE_NAMES_TAURI.test(match[0])) lines.add(lineAt(code, match.index));
  }
  for (const pattern of [RUST_EXTERN_CRATE, RUST_TAURI_PATH]) {
    for (const match of code.matchAll(pattern)) {
      if (match.index !== undefined) lines.add(lineAt(code, match.index));
    }
  }

  return [...lines].sort((a, b) => a - b);
}

function toPosix(path: string): string {
  return path.split(sep).join(posix.sep);
}

/** Every file under `root`, repo-relative and POSIX-separated, fixtures excluded. */
export function walk(root: string, includeFixtures = false): string[] {
  const found: string[] = [];
  const visit = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const absolute = join(dir, entry.name);
      const rel = toPosix(relative(root, absolute));
      if (entry.isDirectory()) {
        if (SKIP_DIRS.has(entry.name)) continue;
        if (!includeFixtures && rel === FIXTURE_ROOT) continue;
        visit(absolute);
      } else if (entry.isFile()) {
        found.push(rel);
      }
    }
  };
  visit(root);
  return found.sort();
}

function read(root: string, file: string): string {
  return readFileSync(join(root, file), 'utf8');
}

/**
 * Rule (a): nothing outside the Tauri allowlist may import Tauri.
 *
 * The window is a client with no privileged path (D-1, D-2). A component that reaches for
 * `@tauri-apps/api` directly, or a crate that links `tauri`, has quietly made the runtime
 * depend on the UI toolkit — which is the one thing the architecture is built to prevent.
 */
export function noTauriOutsideDesktop(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];
  const allowed = (file: string): boolean =>
    TAURI_ALLOWLIST.some((prefix) => file.startsWith(prefix));

  const report = (file: string, line: number): void => {
    violations.push({
      rule: 'no-tauri-outside-desktop',
      file,
      line,
      message: `imports tauri; only ${TAURI_ALLOWLIST.join(' and ')} may do that (D-1, D-2)`,
    });
  };

  for (const file of files) {
    if (allowed(file)) continue;
    const extension = file.slice(file.lastIndexOf('.'));
    if (!SOURCE_EXTENSIONS.includes(extension)) continue;

    const source = read(root, file);

    if (extension === '.rs') {
      // Rust is scanned whole: comments blanked, then statement by statement, so a `use`
      // that spans lines is one unit rather than three lines none of which match.
      for (const line of rustTauriLines(source)) report(file, line);
      continue;
    }

    // TypeScript and JavaScript stay line-based. ESLint owns this boundary properly with
    // an AST; lint-meta is the backstop for the files ESLint's ignores exclude, and a
    // statement-level scan here would fire on the sample code inside
    // `scripts/prove-eslint-bans.ts`, where those imports are test input.
    source.split(/\r?\n/).forEach((text, index) => {
      const trimmed = text.trim();
      // Comments talk about the rule constantly; only code breaks it.
      if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) {
        return;
      }
      if (TS_TAURI_IMPORTS.some((pattern) => pattern.test(text))) report(file, index + 1);
    });
  }
  return violations;
}

/** Which crate a dependency is, spelled for a message — `ui (package = "tauri")` renamed. */
function describe(edge: CargoDependencyEdge): string {
  return edge.rename === null
    ? `\`${edge.name}\``
    : `\`${edge.rename}\` (package = "${edge.name}")`;
}

/**
 * Rule (b): no Rust crate outside `apps/desktop` may depend on tauri.
 *
 * This is the load-bearing rule of the three. If no crate outside `apps/desktop` depends on
 * tauri then `use tauri::…` in those crates cannot compile, which makes the Rust text scan
 * belt and braces rather than the guard. It is kept because it reports a file and a line
 * that a developer can act on in ten seconds, where this rule reports a manifest.
 *
 * Every dependency kind counts — normal, dev and build alike. A runtime crate has no reason
 * to link the UI toolkit even in a test. So does either half of a rename: see `isTauriEdge`.
 */
export function noTauriInRustCrates(workspace: CargoWorkspace): Violation[] {
  const violations: Violation[] = [];

  for (const member of workspace.members) {
    if (TAURI_ALLOWLIST.some((prefix) => member.manifestPath.startsWith(prefix))) continue;
    for (const edge of member.dependencies) {
      if (!isTauriEdge(edge)) continue;
      violations.push({
        rule: 'no-tauri-in-rust-crates',
        file: member.manifestPath,
        line: 0,
        message: `declares ${describe(edge)}; only apps/desktop may link the UI toolkit (D-1)`,
      });
    }
  }

  return violations;
}

/**
 * Rule (c): no crate outside `apps/desktop` may *reach* tauri through anything.
 *
 * Rule (b) names the crate that declares the dependency. This one says what it costs, by
 * walking the resolved graph and reporting the shortest chain — so the reviewer of a
 * `nysia-proto` change can see that it has just put the UI toolkit inside the runtime.
 *
 * Every non-allowlisted member is a seed, not `nysia-core` alone. Seeding from core only
 * left the daemon binary able to reach tauri through `nysia-desktop`, or through any
 * non-member crate, with nothing tripping: rule (b) sees only direct declarations, and this
 * rule was not looking. That is the same hole round one found, one hop further out.
 *
 * A chain through an allowlisted crate still counts. `apps/desktop` may link the UI
 * toolkit; a crate that depends on `apps/desktop` has linked it too.
 */
export function noTauriReachingRustCrates(workspace: CargoWorkspace): Violation[] {
  const violations: Violation[] = [];

  for (const seed of workspace.members) {
    if (TAURI_ALLOWLIST.some((prefix) => seed.manifestPath.startsWith(prefix))) continue;

    const chain = shortestChainToTauri(workspace, seed.id);
    // `[seed, tauri]` is a direct declaration, which is rule (b)'s to report. This rule
    // exists for everything longer.
    if (chain === undefined || chain.length < 3) continue;

    const names = chain.map((id) => workspace.packages.get(id)?.name ?? id).join(' -> ');
    violations.push({
      rule: 'no-tauri-reaching-rust-crates',
      file: seed.manifestPath,
      line: 0,
      message: `${names}; a crate outside apps/desktop must not link the UI toolkit (D-1)`,
    });
  }

  return violations;
}

/**
 * The shortest path from `seedId` to any tauri package, or `undefined` if there is none.
 *
 * Breadth-first, and with its own `seen` set per seed. Sharing one set across seeds would
 * be faster and wrong: a tauri package first reached by a crate that declares it directly
 * would be marked seen, and the crate that reaches the same package through two hops would
 * then be passed over in silence.
 */
function shortestChainToTauri(
  workspace: CargoWorkspace,
  seedId: string,
): readonly string[] | undefined {
  const queue: string[][] = [[seedId]];
  const seen = new Set<string>([seedId]);

  while (queue.length > 0) {
    const chain = queue.shift();
    if (chain === undefined) break;
    const tail = chain[chain.length - 1];
    if (tail === undefined) continue;

    for (const nextId of workspace.edges.get(tail) ?? []) {
      if (seen.has(nextId)) continue;
      seen.add(nextId);
      const next = workspace.packages.get(nextId);
      if (next === undefined) continue;

      if (isTauriPackage(next.name)) return [...chain, nextId];
      queue.push([...chain, nextId]);
    }
  }

  return undefined;
}

/**
 * The rules that read source text: rule (a), over the tree at `root`.
 *
 * Separate from the cargo rules because it needs no workspace — it runs against the fixture
 * trees, which are deliberately not buildable crates.
 */
export function runSourceRules(root: string, includeFixtures = false): Violation[] {
  return noTauriOutsideDesktop(root, walk(root, includeFixtures));
}

/**
 * Rule (b), continued: every crate on disk must be one cargo knows about.
 *
 * Reading the dependency graph instead of every `Cargo.toml` on disk is what makes rules
 * (b) and (c) trustworthy, but it narrows them in one way: a crate directory that is not a
 * workspace member never appears in `cargo metadata`, so nothing inspects it. Today such a
 * crate compiles into nothing, which is why it is a small risk — but "the rule stopped
 * looking and said nothing" is the failure class this tool exists to remove, so a manifest
 * cargo does not account for is reported rather than skipped.
 */
export function noUnknownCrates(
  files: readonly string[],
  workspace: CargoWorkspace,
): Violation[] {
  const known = new Set(workspace.members.map((m) => m.manifestPath));
  // The workspace root manifest may be virtual — no `[package]`, so never a member.
  known.add('Cargo.toml');

  return files
    .filter((file) => file === 'Cargo.toml' || file.endsWith('/Cargo.toml'))
    .filter((file) => !known.has(file))
    .filter((file) => !TAURI_ALLOWLIST.some((prefix) => file.startsWith(prefix)))
    .map((file) => ({
      rule: 'no-tauri-in-rust-crates',
      file,
      line: 0,
      message:
        'is not a workspace member, so cargo never resolves it and the dependency rules ' +
        'cannot see inside it; add it to [workspace] members or delete it',
    }));
}

/**
 * The rules that read cargo's resolved dependency graph: rules (b) and (c).
 *
 * @throws {CargoMetadataError} if cargo cannot describe the workspace. Deliberately not
 * caught here: a dependency rule that could not run must not report zero violations.
 */
export function runCargoRules(root: string, includeFixtures = false): Violation[] {
  const workspace = loadCargoWorkspace(root);
  return [
    ...noTauriInRustCrates(workspace),
    ...noTauriReachingRustCrates(workspace),
    ...noUnknownCrates(walk(root, includeFixtures), workspace),
  ];
}

/**
 * Every architecture rule, for a tree that is a cargo workspace.
 *
 * @throws {CargoMetadataError} see {@link runCargoRules}.
 */
export function runAllRules(root: string, includeFixtures = false): Violation[] {
  return [
    ...runSourceRules(root, includeFixtures),
    ...runCargoRules(root, includeFixtures),
  ];
}
