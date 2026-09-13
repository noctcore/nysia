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

/** Names of the dependencies declared by a Cargo manifest, across every dependency table. */
export function cargoDependencies(manifest: string): string[] {
  const names = new Set<string>();
  let inDependencyTable = false;

  for (const raw of manifest.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('#')) continue;

    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header?.[1] !== undefined) {
      const section = header[1];
      // `[dependencies]`, `[dev-dependencies]`, `[target.'cfg(windows)'.dependencies]`.
      inDependencyTable = /(^|\.)(dev-|build-)?dependencies$/.test(section);
      // `[dependencies.tauri]` names the crate in the header itself.
      const nested = /(^|\.)(dev-|build-)?dependencies\.([A-Za-z0-9_-]+)$/.exec(section);
      if (nested?.[3] !== undefined) names.add(nested[3]);
      continue;
    }
    if (!inDependencyTable) continue;

    const key = /^([A-Za-z0-9_-]+)\s*(?:\.[A-Za-z0-9_-]+\s*)?=/.exec(line);
    if (key?.[1] !== undefined) names.add(key[1]);
  }
  return [...names];
}

const NYSIA_CRATES = ['nysia', 'nysia-core', 'nysia-proto'];

function isTauri(name: string): boolean {
  return name === 'tauri' || name.startsWith('tauri-');
}

/**
 * Rule (b): `nysia-core` must not depend on `tauri`, directly or through another Nysia
 * crate.
 *
 * The runtime outliving the UI is the founding constraint (D-1). A runtime that links the
 * UI toolkit cannot honour it, and the dependency would arrive by accident — someone adds
 * `tauri` to a shared crate for one convenience type and the boundary is gone.
 */
export function coreDeclaresNoTauri(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];
  const manifestOf = (crate: string): string | undefined => {
    const path = `crates/${crate}/Cargo.toml`;
    return files.includes(path) ? path : undefined;
  };

  const seen = new Set<string>();
  const inspect = (crate: string, via: readonly string[]): void => {
    if (seen.has(crate)) return;
    seen.add(crate);
    const path = manifestOf(crate);
    if (path === undefined) return;

    const dependencies = cargoDependencies(readFileSync(join(root, path), 'utf8'));
    for (const dependency of dependencies) {
      if (isTauri(dependency)) {
        const chain = [...via, crate].join(' -> ');
        violations.push({
          rule: 'core-declares-no-tauri',
          file: path,
          line: 0,
          message: `${chain} depends on \`${dependency}\`; the runtime must not link the UI toolkit (D-1)`,
        });
      } else if (NYSIA_CRATES.includes(dependency)) {
        inspect(dependency, [...via, crate]);
      }
    }
  };

  inspect('nysia-core', []);
  return violations;
}

/** Run every architecture rule over the tree at `root`. */
export function runRules(root: string, includeFixtures = false): Violation[] {
  const files = walk(root, includeFixtures);
  return [...noTauriOutsideDesktop(root, files), ...coreDeclaresNoTauri(root, files)];
}
