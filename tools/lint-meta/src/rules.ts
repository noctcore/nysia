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
  /(?:^|[^\w.$])import\s*\(\s*['"]@tauri-apps[^'"]*['"]/,
  /(?:^|[^\w.$])require\s*\(\s*['"]@tauri-apps[^'"]*['"]/,
];
/** `use tauri::…`, `use tauri as …`, `extern crate tauri`, or a bare `tauri::` path. */
const RUST_TAURI_USE = /(?:^|[^\w:])(?:use\s+tauri\b|extern\s+crate\s+tauri\b|tauri::)/;

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

function read(root: string, file: string): string[] {
  return readFileSync(join(root, file), 'utf8').split(/\r?\n/);
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

  for (const file of files) {
    if (allowed(file)) continue;
    const extension = file.slice(file.lastIndexOf('.'));
    if (!SOURCE_EXTENSIONS.includes(extension)) continue;

    const patterns = extension === '.rs' ? [RUST_TAURI_USE] : TS_TAURI_IMPORTS;
    read(root, file).forEach((text, index) => {
      const trimmed = text.trim();
      // Comments talk about the rule constantly; only code breaks it.
      if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) {
        return;
      }
      if (patterns.some((pattern) => pattern.test(text))) {
        violations.push({
          rule: 'no-tauri-outside-desktop',
          file,
          line: index + 1,
          message: `imports tauri; only ${TAURI_ALLOWLIST.join(' and ')} may do that (D-1, D-2)`,
        });
      }
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
