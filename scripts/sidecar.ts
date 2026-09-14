/**
 * The `nysia` runtime that ships inside the app bundle — staged, and checked for.
 *
 * D-11 says one binary is both the daemon and the CLI, selected by argv. D-1 says killing
 * the window must never interrupt a session, which means the window is a *client* of a
 * daemon it has to be able to start. Put together: the app has to carry that binary, or a
 * first launch on a machine with nothing listening is an instruction to go and open a
 * terminal (§12 q6).
 *
 * Tauri carries it as an `externalBin` — a sidecar. The name on disk has to end in the
 * target triple, which is how one `binaries/` directory can hold the runtime for every
 * platform being built; Tauri strips the triple again when it lays the bundle out, so the
 * file the window looks for is plain `nysia` beside it.
 *
 * Two commands:
 *
 * - `stage [--profile debug|release]` copies the freshly built `nysia` into `binaries/`.
 *   **Every** `cargo build` of the desktop crate needs this to have happened, not only a
 *   bundle: `tauri_build::build()` resolves `externalBin` at compile time and fails with
 *   *resource path ... doesn't exist* when it is missing.
 * Re-stage whenever the runtime itself changes: `tauri_build` copies what is in
 * `binaries/` over `target/<profile>/nysia`, so a `cargo build -p nysia` that is not
 * followed by a stage is undone by the next build of the window.
 *
 * - `verify <directory>` asserts the runtime is beside a window that has been built or
 *   bundled — `target/release` on Windows, `Nysia.app/Contents/MacOS` on macOS. That is the
 *   check a bundle step exists to make: a bundle that built cleanly and shipped no runtime
 *   is exactly the defect this closes, and it is invisible until someone launches it.
 */
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

/**
 * The window's own executable, which the runtime must sit beside.
 *
 * Two names because two layouts: `target/<profile>` holds the binary cargo built, and a
 * macOS bundle holds the same file under the product name inside `Contents/MacOS`. Checking
 * for either keeps this one function usable against both without it having to be told which
 * it is looking at.
 */
const WINDOWS = (process.platform === 'win32'
  ? ['nysia-desktop.exe', 'Nysia.exe']
  : ['nysia-desktop', 'Nysia']) as readonly string[];

/** The runtime, as it is named once Tauri has laid the bundle out. */
const RUNTIME = process.platform === 'win32' ? 'nysia.exe' : 'nysia';

/** Where `externalBin` in `tauri.conf.json` points, relative to `src-tauri`. */
const BINARIES = join('apps', 'desktop', 'src-tauri', 'binaries');

/**
 * The triple rustc is building for.
 *
 * Asked of rustc rather than composed from `process.platform`, because the two disagree on
 * exactly the machine where it matters: an Apple Silicon runner is `aarch64-apple-darwin`
 * and an Intel one is `x86_64-apple-darwin`, and a sidecar staged under the wrong name is
 * not found by a build that then says only that a path does not exist.
 */
function hostTriple(): string {
  const rustc = spawnSync('rustc', ['-vV'], { encoding: 'utf8' });
  if (rustc.status !== 0) {
    throw new Error(`rustc -vV failed: ${rustc.stderr || rustc.error?.message || 'no output'}`);
  }
  const host = rustc.stdout.split('\n').find((line) => line.startsWith('host: '));
  if (host === undefined) {
    throw new Error(`rustc -vV printed no host line:\n${rustc.stdout}`);
  }
  return host.slice('host: '.length).trim();
}

/** Copy the built runtime into `binaries/`, named for the triple. */
export function stage(profile: string, repoRoot = findRepoRoot()): string {
  const built = join(repoRoot, 'target', profile, RUNTIME);
  if (!existsSync(built)) {
    throw new Error(
      `${built} does not exist — build it first with \`cargo build -p nysia\`` +
        (profile === 'release' ? ' --release' : ''),
    );
  }
  const suffix = process.platform === 'win32' ? '.exe' : '';
  const staged = join(repoRoot, BINARIES, `nysia-${hostTriple()}${suffix}`);
  mkdirSync(join(repoRoot, BINARIES), { recursive: true });
  copyFileSync(built, staged);
  return staged;
}

/**
 * Assert that a built or bundled window has the runtime beside it.
 *
 * Both files, not only the runtime: a directory holding neither would otherwise pass the
 * half of the check that matters least, and the claim being made is about what ships *with
 * the window*.
 */
export function verify(directory: string): void {
  const window = WINDOWS.map((name) => join(directory, name)).find((path) => existsSync(path));
  const runtime = join(directory, RUNTIME);
  if (window === undefined) {
    throw new Error(
      `${directory} holds none of ${WINDOWS.join(', ')}, so there is no window to check the ` +
        'runtime beside',
    );
  }
  if (!existsSync(runtime)) {
    throw new Error(
      `${window} shipped without ${RUNTIME} beside it: the app cannot start a daemon, so a ` +
        'first launch on a machine with none reaches nothing (§12 q6)',
    );
  }
  if (statSync(runtime).size === 0) {
    throw new Error(`${runtime} is empty, so it is not the runtime the window would start`);
  }
}

function main(argv: readonly string[]): void {
  const [command, ...rest] = argv;
  switch (command) {
    case 'stage': {
      const flag = rest.indexOf('--profile');
      const profile = flag === -1 ? 'debug' : (rest[flag + 1] ?? 'debug');
      const staged = stage(profile);
      process.stdout.write(`staged ${staged}\n`);
      return;
    }
    case 'verify': {
      const directory = rest[0];
      if (directory === undefined) {
        throw new Error('verify needs the directory the window was built or bundled into');
      }
      verify(directory);
      process.stdout.write(`${join(directory, RUNTIME)} ships beside the window\n`);
      return;
    }
    default:
      throw new Error(`usage: sidecar.ts stage [--profile <p>] | verify <directory>`);
  }
}

// Run only when this file is the program, never when `prove-sidecar.ts` imports `verify`
// from it — a suffix test would match that importer's path too, which is exactly what it
// did.
if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
