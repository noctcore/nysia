/**
 * The `nysia` runtime the window starts — ensured for a dev run, staged into a bundle, and
 * checked for once the bundle is laid out.
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
 * ## Why the declaration is in `tauri.bundle.conf.json` and not `tauri.conf.json`
 *
 * Because `tauri_build` acts on `externalBin` at **compile** time, not bundle time: for every
 * `cargo build` of the desktop crate it deletes `target/<profile>/nysia` and copies the
 * staged file over it — and the delete is an `unwrap`.
 *
 * Read that against D-1. The daemon **outlives the window by design**, so "run the app, close
 * it, build again" is the ordinary sequence rather than an unusual one — and the daemon still
 * running is holding `target/<profile>/nysia`, which is the file about to be deleted.
 * **Holding it stops the delete only once that file is Tauri's own single-linked copy.**
 * Windows unlinks a running image while another name for it survives, so a daemon started
 * from the two-name hardlink cargo leaves from `deps/` does not trip it: measured, the
 * unlink succeeds through the other name, the link count goes from two to one, and the
 * build finishes. It is the copy Tauri writes over it — one name, and a daemon running
 * from that name — where the delete fails, so the build panicked with `PermissionDenied`
 * as soon as that copy had replaced the hardlink.
 *
 * Declared in a config the bundler merges with `--config` and `cargo` never reads, **the
 * ordinary loop is clear of it**: `cargo build`, `cargo test`, `cargo clippy` and `tauri dev`
 * neither need the sidecar nor touch it, and a developer's window starts the runtime cargo
 * just built rather than a copy of an older one. The copy happens during the bundle build,
 * which is the only time anybody wants it.
 *
 * **The bundle build itself still deletes, and this file should not say otherwise.** `tauri
 * build --config` passes the merged config on in `TAURI_CONFIG`, so `tauri-build` reads the
 * declaration too and reaches the same `fs::remove_file(target/<profile>/nysia).unwrap()`,
 * which panics when a daemon is running from that exact file. Survivable: it takes a daemon
 * running under the profile being bundled, bundling is deliberate and occasional, and the
 * way out is to stop that daemon. Not fixable by staging elsewhere either — the file has to
 * be called `nysia` and has to land beside the window, which is the file the daemon runs
 * from. §12 q5 of the architecture doc carries that with the line numbers in it.
 *
 * ## What a dev run needs
 *
 * The window resolves its runtime **beside its own executable**, which in development is
 * `target/<profile>/nysia` — the binary cargo built, never the copy in `binaries/`. `tauri
 * dev` merges only `build.devUrl` into `TAURI_CONFIG` and never the `externalBin`
 * declaration, so nothing on the dev path reads `binaries/` at all: staging for a dev run
 * would copy a file no window opens.
 *
 * What a dev run does need is that `target/<profile>/nysia` **exist** — and `tauri dev`
 * builds `nysia-desktop` without ever building `nysia`. That was #74: a fresh clone's first
 * `pnpm dev` opened a window whose runtime was not there, and the notice it raised said to
 * reinstall, which is honest about the missing file and useless advice for a missing build.
 *
 * So `pnpm dev` runs [`ensure`] first, and **`ensure` builds only when the file is absent**.
 * Absent is the whole condition: no mtime, no size, nothing compared against sources. The two
 * richer answers are both worse. Comparing the binary to its sources re-implements cargo's
 * fingerprint and cannot see a feature, a profile or a moved dependency. Running `cargo build
 * -p nysia` unconditionally is worse still, because it **fails** in an ordinary sequence: a
 * daemon from the last run that is still up is running from `target/<profile>/nysia`, and
 * rewriting that file means unlinking a single-linked running image, which Windows refuses
 * (`os error 5`, measured). A daemon outliving its window is **D-1 working as designed**, not
 * an edge case — idle retire needs `sessions.is_empty()`, so one that held a tab is still
 * there — so a dev command that breaks there breaks in the ordinary case. Building only what
 * is missing cannot reach that failure at all: it never rewrites a file that exists.
 *
 * **What that costs, and it is a real cost.** A runtime that is present is never rebuilt, so
 * editing the daemon and running `pnpm dev` starts the *old* one and says nothing. Stopping
 * the daemon does not refresh it either; only `cargo build -p nysia`, or deleting
 * `target/<profile>/nysia`, does. That gap is not #74 and not a regression — today's `pnpm
 * dev` builds nothing at all — but it is the price of never failing in the D-1 path, and
 * somebody reading this later should know it was bought rather than overlooked.
 *
 * **Why one entry point and not a `&&`.** pnpm appends the arguments of `pnpm dev -- --release`
 * to the end of the whole script, so a prepended command never sees the flag: it would build
 * `debug` while the window it then opens is `target/release/nysia-desktop.exe`, which looks
 * beside *itself* — #74 again, verbatim, in the one place nobody would look for it. Reading
 * the arguments and then handing them on is what makes the profile the one actually run.
 *
 * Four commands:
 *
 * - `dev [tauri args…]` is what `pnpm dev` runs: `ensure`, then `tauri dev` with the same
 *   arguments and the same exit code.
 *
 * - `ensure [--release]` builds `nysia` if `target/<profile>/nysia` is not there, and does
 *   nothing whatsoever if it is.
 *
 * - `stage [--profile debug|release]` copies the freshly built `nysia` into `binaries/`.
 *   Needed **before bundling** — `pnpm build:app`, or the bundle job in CI — and never
 *   before an ordinary cargo command. Re-stage whenever the runtime changes and you are
 *   about to bundle. In development there is nothing to re-stage for: the window starts
 *   what cargo built.
 *
 * - `verify <directory>` asserts the runtime is beside a window that has been built or
 *   bundled — `target/release` on Windows, `Nysia.app/Contents/MacOS` on macOS. That is the
 *   check a bundle step exists to make: a bundle that built cleanly and shipped no runtime
 *   is exactly the defect this closes, and it is invisible until someone launches it.
 */
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, pathToFileURL } from 'node:url';

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

/** Where `externalBin` in `tauri.bundle.conf.json` points, relative to `src-tauri`. */
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

/** A cargo profile, as both the directory it lands in and the flags that select it. */
export interface Profile {
  /** The directory under `target/`. */
  readonly directory: string;
  /** What to pass cargo to build into it — empty for the default. */
  readonly cargoFlags: readonly string[];
}

/**
 * The profile a `tauri dev` invocation will run, read from the arguments it was given.
 *
 * **Every argument is read, including the ones after a `--`, and that is deliberate.** The
 * obvious rule — stop at `--`, because what follows belongs to the runner rather than to
 * tauri — is wrong twice over. pnpm forwards `pnpm dev -- --release` as the two words `--`
 * `--release`, so stopping at the separator throws away the flag in exactly the invocation a
 * developer would type; and tauri hands its runner arguments to `cargo run`, so a `--release`
 * that *is* on the far side of a `--` still selects the release profile. Both were measured.
 *
 * `--profile <name>` is read too, though `tauri dev` has no such switch of its own, because
 * it reaches cargo the same way and is the only other thing that moves the output directory.
 * The `dev` profile lands in `target/debug` — cargo's own spelling quirk, and the one case
 * where the directory is not the profile's name. Anything else is a custom profile in
 * `target/<name>`. `scripts/sidecar.test.ts` pins every direction of this.
 */
export function profileFor(args: readonly string[]): Profile {
  const flag = args.indexOf('--profile');
  const named = flag === -1 ? undefined : args[flag + 1];
  if (named !== undefined) {
    return { directory: named === 'dev' ? 'debug' : named, cargoFlags: ['--profile', named] };
  }
  return args.includes('--release')
    ? { directory: 'release', cargoFlags: ['--release'] }
    : { directory: 'debug', cargoFlags: [] };
}

/** Where the window built under `profile` will look for its runtime. */
export function runtimeFor(profile: Profile, repoRoot = findRepoRoot()): string {
  return join(repoRoot, 'target', profile.directory, RUNTIME);
}

/**
 * Build the runtime for `profile` **if it is not already there**, and otherwise do nothing.
 *
 * Returns whether it built. The "otherwise do nothing" is the load-bearing half: it is what
 * keeps this off the file a surviving daemon is running from, which is the whole reason the
 * condition is existence rather than freshness. See the header.
 */
export function ensure(profile: Profile, repoRoot = findRepoRoot()): boolean {
  const runtime = runtimeFor(profile, repoRoot);
  if (existsSync(runtime)) {
    return false;
  }
  const flags = ['build', '-p', 'nysia', ...profile.cargoFlags];
  process.stdout.write(`${runtime} is absent — building it once: cargo ${flags.join(' ')}\n`);
  const cargo = spawnSync('cargo', flags, { stdio: 'inherit', cwd: repoRoot });
  if (cargo.error !== undefined) {
    throw new Error(`cargo ${flags.join(' ')} could not start: ${cargo.error.message}`);
  }
  if (cargo.status !== 0) {
    throw new Error(
      `cargo ${flags.join(' ')} failed, so the window would have no runtime to start`,
    );
  }
  return true;
}

/**
 * The tauri CLI's own entry point, run with this node rather than through its `.bin` shim.
 *
 * The shim is a `.CMD` on Windows, which node refuses to spawn without a shell, and handing a
 * developer's arguments to a shell is a quoting problem nobody needs. `tauri.js` is what that
 * shim execs anyway, so running it directly is the same program with one less layer.
 *
 * Resolved from `apps/desktop`, because that is the package `@tauri-apps/cli` is a dependency
 * of; pnpm's `node_modules` is strict, so it is not resolvable from the root.
 */
function tauriCli(repoRoot: string): string {
  const from = createRequire(pathToFileURL(join(repoRoot, 'apps', 'desktop', 'package.json')));
  try {
    return from.resolve('@tauri-apps/cli/tauri.js');
  } catch (error) {
    throw new Error(
      '@tauri-apps/cli is not installed, so there is no `tauri dev` to run — `pnpm install` first',
      { cause: error },
    );
  }
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
    case 'dev': {
      const repoRoot = findRepoRoot();
      ensure(profileFor(rest), repoRoot);
      const tauri = spawnSync(process.execPath, [tauriCli(repoRoot), 'dev', ...rest], {
        stdio: 'inherit',
        cwd: join(repoRoot, 'apps', 'desktop'),
      });
      if (tauri.error !== undefined) {
        throw new Error(`tauri dev could not start: ${tauri.error.message}`);
      }
      // A signal leaves `status` null — Ctrl-C on the dev loop is the ordinary way out, and
      // reporting it as success would tell a script the window exited cleanly.
      process.exitCode = tauri.status ?? 1;
      return;
    }
    case 'ensure': {
      const profile = profileFor(rest);
      if (!ensure(profile)) {
        process.stdout.write(`${runtimeFor(profile)} is already there\n`);
      }
      return;
    }
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
      throw new Error(
        'usage: sidecar.ts dev [tauri args…] | ensure [--release] | ' +
          'stage [--profile <p>] | verify <directory>',
      );
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
