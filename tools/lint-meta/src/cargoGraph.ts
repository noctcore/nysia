import { spawnSync } from 'node:child_process';
import { posix, relative, sep } from 'node:path';
import process from 'node:process';

/**
 * The workspace as **cargo** resolves it.
 *
 * lint-meta used to read `Cargo.toml` with a hand-written line parser, and every spelling
 * cargo accepts that the parser did not was a silent hole: a quoted key
 * (`"tauri" = { … }`), a quoted table header (`[dependencies."tauri"]`), a trailing comment
 * after `[dependencies]`, whitespace inside `[ dependencies ]`, and — worst — a trailing
 * comment after `[package]`, which made the crate's name unreadable and dropped the whole
 * manifest from the scan. Five spellings, five ways to link the UI toolkit into the runtime
 * with the gate reporting success.
 *
 * `cargo metadata` is the authority cargo itself uses. It resolves renames, quoting,
 * workspace inheritance and the full dependency graph for free, and it cannot disagree with
 * what actually gets compiled. There is no parser here to get wrong.
 */

/** A package as cargo resolved it. */
export interface CargoPackage {
  /** Cargo's unique package id. Two crates may share a name; ids never collide. */
  readonly id: string;
  readonly name: string;
  /** Repo-relative, POSIX-separated, so rules read the same on both runners. */
  readonly manifestPath: string;
  /** Direct dependencies, by the crate actually pulled rather than the key written. */
  readonly dependencies: readonly CargoDependencyEdge[];
}

/** One declared dependency. */
export interface CargoDependencyEdge {
  /** The real package name — `ui = { package = "tauri" }` reports `tauri`. */
  readonly name: string;
  /** The key it was declared under, when that differs from the package name. */
  readonly rename: string | null;
}

/** The resolved workspace: its members, and the graph reachable from them. */
export interface CargoWorkspace {
  readonly members: readonly CargoPackage[];
  /** Every package in the graph, members and transitive dependencies alike. */
  readonly packages: ReadonlyMap<string, CargoPackage>;
  /** Resolved edges, package id → dependency package ids. */
  readonly edges: ReadonlyMap<string, readonly string[]>;
}

/** `cargo metadata` could not describe the workspace. Never swallowed. */
export class CargoMetadataError extends Error {}

interface RawDependency {
  name: string;
  rename: string | null;
}
interface RawPackage {
  id: string;
  name: string;
  manifest_path: string;
  dependencies: RawDependency[];
}
interface RawNode {
  id: string;
  deps: { pkg: string }[];
}
interface RawMetadata {
  packages: RawPackage[];
  workspace_members: string[];
  workspace_root: string;
  resolve: { nodes: RawNode[] } | null;
}

function toPosix(path: string): string {
  return path.split(sep).join(posix.sep);
}

/**
 * Ask cargo to describe the workspace rooted at `root`.
 *
 * The full graph, not `--no-deps`: the rule has to hold for a crate that reaches tauri
 * through a third-party dependency, not only for one that names it directly.
 *
 * @throws {CargoMetadataError} if cargo is missing, fails, or returns something
 * unreadable. A dependency rule that cannot run must say so — returning "no violations"
 * because the check never happened is the failure this whole file exists to remove.
 */
export function loadCargoWorkspace(root: string): CargoWorkspace {
  // No `--locked`: a developer who edits a manifest and runs `pnpm lint` before `cargo
  // build` should get the rule's answer, not a lockfile complaint about something else.
  const result = spawnSync('cargo', ['metadata', '--format-version', '1'], {
    cwd: root,
    encoding: 'utf8',
    shell: false,
    maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, CARGO_TERM_COLOR: 'never' },
  });

  if (result.error !== undefined) {
    throw new CargoMetadataError(`could not run cargo in ${root}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new CargoMetadataError(
      `cargo metadata failed in ${root} (exit ${result.status}):\n${result.stderr ?? ''}`,
    );
  }

  let raw: RawMetadata;
  try {
    raw = JSON.parse(result.stdout) as RawMetadata;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new CargoMetadataError(`cargo metadata emitted unreadable JSON in ${root}: ${detail}`);
  }
  if (raw.resolve === null) {
    throw new CargoMetadataError(
      `cargo metadata returned no dependency graph for ${root}; the transitive rule cannot run`,
    );
  }

  const packages = new Map<string, CargoPackage>();
  for (const pkg of raw.packages) {
    packages.set(pkg.id, {
      id: pkg.id,
      name: pkg.name,
      manifestPath: toPosix(relative(raw.workspace_root, pkg.manifest_path)),
      dependencies: pkg.dependencies.map((d) => ({ name: d.name, rename: d.rename })),
    });
  }

  const edges = new Map<string, readonly string[]>();
  for (const node of raw.resolve.nodes) {
    edges.set(
      node.id,
      node.deps.map((d) => d.pkg),
    );
  }

  const members: CargoPackage[] = [];
  for (const id of raw.workspace_members) {
    const pkg = packages.get(id);
    if (pkg === undefined) {
      throw new CargoMetadataError(`cargo metadata listed member ${id} with no package entry`);
    }
    members.push(pkg);
  }

  return { members, packages, edges };
}

/** `tauri` itself, and the `tauri-*` family (`tauri-build`, `tauri-plugin-*`). */
export function isTauriPackage(name: string): boolean {
  return name === 'tauri' || name.startsWith('tauri-');
}
