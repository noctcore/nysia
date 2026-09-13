import { readFileSync, readdirSync } from 'node:fs';
import { join, posix, relative, sep } from 'node:path';

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

const SOURCE_EXTENSIONS = ['.ts', '.tsx', '.js', '.jsx', '.mjs', '.rs'];

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
 * String literals are therefore stepped over as units — normal, byte (`b"…"`), raw
 * (`r#"…"#`) and char literals alike, the last so that `'"'` is a character and not the
 * start of a string. Their contents are blanked as well, since a `tauri::` inside a string
 * is not an import.
 */
export function blankRustComments(source: string): string {
  const out = [...source];
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

  /** A char literal `'x'` / `'\n'` / `'"'`, as opposed to a lifetime `'a`. */
  const CHAR_LITERAL = /^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[^'\\])'/;

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

    // A raw string, optionally a byte string: `r"…"`, `r#"…"#`, `br##"…"##`.
    if (!isWord(i - 1)) {
      let j = i;
      if (source[j] === 'b') j += 1;
      if (source[j] === 'r') {
        let k = j + 1;
        let hashes = 0;
        while (source[k] === '#') {
          hashes += 1;
          k += 1;
        }
        if (source[k] === '"') {
          const terminator = `"${'#'.repeat(hashes)}`;
          const end = source.indexOf(terminator, k + 1);
          blankTo(end === -1 ? source.length : end + terminator.length);
          continue;
        }
      }

      // A normal or byte string, with backslash escapes.
      let s = i;
      if (source[s] === 'b') s += 1;
      if (source[s] === '"') {
        let k = s + 1;
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

  return out.join('');
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

/** One entry of a Cargo dependency table. */
export interface CargoDependency {
  /** The key as written, which is the name the crate is `use`d by. */
  readonly key: string;
  /**
   * The crate actually pulled from the registry.
   *
   * Differs from `key` when the entry is renamed — `ui = { package = "tauri" }` links
   * tauri under the name `ui`, and a scan that only read keys would never see it.
   */
  readonly package: string;
}

/**
 * Every dependency a Cargo manifest declares, across every dependency table.
 *
 * `[workspace.dependencies]` is deliberately excluded: it is a table of *available*
 * versions for members to opt into, not a dependency of anything. The root manifest lists
 * tauri there on purpose, and a scan that counted it would fire on the workspace itself and
 * teach everyone to ignore the rule.
 */
export function cargoDependencies(manifest: string): CargoDependency[] {
  const found = new Map<string, string>();
  let inDependencyTable = false;
  /** Set while inside `[dependencies.<key>]`, so a later bare `package = "x"` binds to it. */
  let nestedKey: string | undefined;

  const record = (key: string, pkg?: string): void => {
    found.set(key, pkg ?? found.get(key) ?? key);
  };

  for (const raw of manifest.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('#')) continue;

    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header?.[1] !== undefined) {
      const section = header[1];
      const isWorkspaceTable = section.startsWith('workspace.');
      // `[dependencies]`, `[dev-dependencies]`, `[target.'cfg(windows)'.dependencies]`.
      inDependencyTable =
        !isWorkspaceTable && /(^|\.)(dev-|build-)?dependencies$/.test(section);
      // `[dependencies.tauri]` names the crate in the header itself.
      const nested = /(^|\.)(dev-|build-)?dependencies\.([A-Za-z0-9_-]+)$/.exec(section);
      nestedKey = isWorkspaceTable ? undefined : nested?.[3];
      if (nestedKey !== undefined) record(nestedKey);
      continue;
    }

    // Inside `[dependencies.ui]` every line describes that one entry, so the only thing
    // worth reading is a `package = "tauri"` rename. Treating these as new keys would
    // invent dependencies called `version` and `features`.
    if (nestedKey !== undefined) {
      const rename = /^package\s*=\s*['"]([A-Za-z0-9_-]+)['"]/.exec(line);
      if (rename?.[1] !== undefined) record(nestedKey, rename[1]);
      continue;
    }
    if (!inDependencyTable) continue;

    const key = /^([A-Za-z0-9_-]+)\s*(?:\.[A-Za-z0-9_-]+\s*)?=/.exec(line);
    if (key?.[1] === undefined) continue;
    // `ui = { package = "tauri", version = "2" }` on one line.
    const inlineRename = /\bpackage\s*=\s*['"]([A-Za-z0-9_-]+)['"]/.exec(line);
    record(key[1], inlineRename?.[1]);
  }

  return [...found].map(([key, pkg]) => ({ key, package: pkg }));
}

function isTauri(name: string): boolean {
  return name === 'tauri' || name.startsWith('tauri-');
}

/** How a dependency is named in a message — `ui (package = "tauri")` when renamed. */
function describe(dependency: CargoDependency): string {
  return dependency.key === dependency.package
    ? `\`${dependency.key}\``
    : `\`${dependency.key}\` (package = "${dependency.package}")`;
}

/**
 * The `name` of a manifest's `[package]` table, if it has one.
 *
 * Scoped to that table on purpose: `[[bin]]` and `[lib]` also carry a `name`, and matching
 * the first one in the file would mis-key a crate whose sections are ordered unusually.
 */
function packageName(manifest: string): string | undefined {
  let inPackage = false;
  for (const raw of manifest.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('#')) continue;
    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header !== null) {
      inPackage = header[1] === 'package';
      continue;
    }
    if (!inPackage) continue;
    const name = /^name\s*=\s*['"]([A-Za-z0-9_-]+)['"]/.exec(line);
    if (name?.[1] !== undefined) return name[1];
  }
  return undefined;
}

/** Package name → manifest path, for every Cargo manifest in the tree. */
function manifestsByPackage(
  root: string,
  files: readonly string[],
): Map<string, { path: string; text: string }> {
  const found = new Map<string, { path: string; text: string }>();
  for (const file of files) {
    if (file !== 'Cargo.toml' && !file.endsWith('/Cargo.toml')) continue;
    const text = readFileSync(join(root, file), 'utf8');
    const name = packageName(text);
    // A virtual manifest — the workspace root — has no [package] and owns no code, so
    // there is nothing to link tauri into. Its [workspace.dependencies] table is excluded
    // by `cargoDependencies` for the same reason.
    if (name !== undefined) found.set(name, { path: file, text });
  }
  return found;
}

/**
 * Rule (b): no Rust crate outside `apps/desktop` may declare a tauri dependency.
 *
 * This used to walk only the dependency chain rooted at `nysia-core`, which meant
 * `crates/nysia/Cargo.toml` was never inspected at all. `tauri` is already in
 * `[workspace.dependencies]`, so the worker who owns `crates/nysia/**` could add
 * `tauri = { workspace = true }` without touching a single shared file, and the daemon
 * binary would link the UI toolkit with nothing tripping.
 *
 * The runtime outliving the UI is the founding constraint (D-1). A process that links the
 * UI toolkit cannot honour it.
 */
export function noTauriInRustCrates(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];

  for (const [, manifest] of manifestsByPackage(root, files)) {
    if (TAURI_ALLOWLIST.some((prefix) => manifest.path.startsWith(prefix))) continue;
    for (const dependency of cargoDependencies(manifest.text)) {
      if (!isTauri(dependency.package)) continue;
      violations.push({
        rule: 'no-tauri-in-rust-crates',
        file: manifest.path,
        line: 0,
        message:
          `declares ${describe(dependency)}; only apps/desktop may link the UI toolkit (D-1)`,
      });
    }
  }

  return violations;
}

/**
 * Rule (c): `nysia-core` must not reach tauri *through* another Nysia crate.
 *
 * Rule (b) catches the crate that declares the dependency. This one says what it costs:
 * it names the chain, so the reviewer of a `nysia-proto` change can see that it has just
 * put the UI toolkit inside the runtime. `tools/lint-meta/fixtures/trips-transitive`
 * exercises this branch specifically — without a committed fixture it is dead code that
 * nobody has watched fail.
 */
export function noTauriReachingCore(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];
  const manifests = manifestsByPackage(root, files);

  const seen = new Set<string>();
  const inspect = (crate: string, via: readonly string[]): void => {
    if (seen.has(crate)) return;
    seen.add(crate);
    const manifest = manifests.get(crate);
    if (manifest === undefined) return;

    for (const dependency of cargoDependencies(manifest.text)) {
      const chain = [...via, crate];
      if (isTauri(dependency.package)) {
        // Only the indirect case: rule (b) already reports the crate that declares it.
        if (chain.length > 1) {
          violations.push({
            rule: 'no-tauri-reaching-core',
            file: manifest.path,
            line: 0,
            message:
              `${chain.join(' -> ')} depends on ${describe(dependency)}; the runtime must ` +
              'not link the UI toolkit (D-1)',
          });
        }
      } else if (manifests.has(dependency.package)) {
        inspect(dependency.package, chain);
      }
    }
  };

  inspect('nysia-core', []);
  return violations;
}

/** Run every architecture rule over the tree at `root`. */
export function runRules(root: string, includeFixtures = false): Violation[] {
  const files = walk(root, includeFixtures);
  return [
    ...noTauriOutsideDesktop(root, files),
    ...noTauriInRustCrates(root, files),
    ...noTauriReachingCore(root, files),
  ];
}
