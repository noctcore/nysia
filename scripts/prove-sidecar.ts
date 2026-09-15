/**
 * Proof that the sidecar mechanism trips (traps register #12).
 *
 * Two halves, because a bundle that ships no runtime is two failures: the copy that was
 * supposed to put the binary next to the window never ran, and the check that was supposed
 * to refuse the result never saw it. A proof that only exercises `verify` stays green if
 * `stage` is hollowed to `return`; a proof that asserts `verify`'s missing-runtime case on
 * a `nysia.exe` substring stays green if the `existsSync` guard is hollowed, because
 * `statSync` then throws `ENOENT` and that error's message already contains the path.
 *
 * So this drives both. `stage` is pointed at a scratch prefix that already holds a built
 * runtime, and the copy has to land under `binaries/` as `nysia-<triple>` with those
 * bytes — an immediate `return` leaves that directory empty. `verify` is pointed at four
 * directories: one with a window and no runtime, one with a runtime that is empty, one
 * with neither, and one with both. The missing-runtime case has to refuse with the guard's
 * own message (`shipped without`), not with a system error that happens to name the file.
 * The complete shape has to be accepted, or a check that refuses everything would satisfy
 * the first three.
 */
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, isAbsolute, join, relative } from 'node:path';
import process from 'node:process';

import { stage, verify } from './sidecar.ts';

const WINDOW = process.platform === 'win32' ? 'nysia-desktop.exe' : 'nysia-desktop';
const RUNTIME = process.platform === 'win32' ? 'nysia.exe' : 'nysia';

const failures: string[] = [];
const root = mkdtempSync(join(tmpdir(), 'nysia-sidecar-'));

/** Node's `ENOENT` (and friends) carry a `code`; the check's own `Error` does not. */
function errnoCode(error: unknown): string | undefined {
  if (typeof error !== 'object' || error === null || !('code' in error)) {
    return undefined;
  }
  return typeof error.code === 'string' ? error.code : undefined;
}

/** `verify` must refuse `directory` with its own message, not a thrown system error. */
function expectTrips(what: string, directory: string, expected: string): void {
  try {
    verify(directory);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const code = errnoCode(error);
    if (code !== undefined) {
      failures.push(
        `${what}: tripped with ${code}, not the check's own refusal: ${message}`,
      );
      return;
    }
    if (!message.includes(expected)) {
      failures.push(`${what}: tripped, but the message did not mention ${expected}: ${message}`);
      return;
    }
    process.stdout.write(`  trips: ${what}\n`);
    return;
  }
  failures.push(`${what}: the check passed, so it would pass on a bundle with no runtime`);
}

try {
  const prefix = join(root, 'stage-prefix');
  const profile = 'debug';
  mkdirSync(join(prefix, 'target', profile), { recursive: true });
  const payload = 'a runtime that stage must copy, not invent';
  writeFileSync(join(prefix, 'target', profile, RUNTIME), payload);
  const binaries = join(prefix, 'apps', 'desktop', 'src-tauri', 'binaries');
  try {
    const staged = stage(profile, prefix);
    const rel = typeof staged === 'string' ? relative(binaries, staged) : '';
    if (
      typeof staged !== 'string' ||
      staged.length === 0 ||
      rel === '' ||
      rel.startsWith('..') ||
      isAbsolute(rel) ||
      !basename(staged).startsWith('nysia-') ||
      !existsSync(staged) ||
      readFileSync(staged, 'utf8') !== payload
    ) {
      failures.push(
        `stage: the runtime did not land under binaries/ (returned ${String(staged)})`,
      );
    } else {
      process.stdout.write('  stages: the runtime into binaries/\n');
    }
  } catch (error) {
    failures.push(
      `stage: threw instead of copying the runtime: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }

  const bare = join(root, 'bare');
  mkdirSync(bare, { recursive: true });
  expectTrips('a directory with no window at all', bare, 'no window');

  const windowOnly = join(root, 'window-only');
  mkdirSync(windowOnly, { recursive: true });
  writeFileSync(join(windowOnly, WINDOW), 'a window');
  expectTrips('a window with no runtime beside it', windowOnly, 'shipped without');

  const empty = join(root, 'empty-runtime');
  mkdirSync(empty, { recursive: true });
  writeFileSync(join(empty, WINDOW), 'a window');
  writeFileSync(join(empty, RUNTIME), '');
  expectTrips('a runtime that is an empty file', empty, 'empty');

  // And the shape it is meant to accept, so the proof is not satisfied by a check that
  // refuses everything.
  const good = join(root, 'complete');
  mkdirSync(good, { recursive: true });
  writeFileSync(join(good, WINDOW), 'a window');
  writeFileSync(join(good, RUNTIME), 'a runtime');
  try {
    verify(good);
    process.stdout.write('  passes: a window with its runtime beside it\n');
  } catch (error) {
    failures.push(
      `a complete bundle was refused: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
} finally {
  rmSync(root, { recursive: true, force: true });
}

if (failures.length > 0) {
  process.stderr.write(`${failures.join('\n')}\n`);
  process.exitCode = 1;
} else {
  process.stdout.write('the sidecar check trips on every bundle missing its runtime\n');
}
