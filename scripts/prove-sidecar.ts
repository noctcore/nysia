/**
 * Proof that the sidecar check trips (traps register #12).
 *
 * `pnpm sidecar:verify` runs against a bundle that is expected to have the runtime in it,
 * and a check that only ever passes is worse than no check: the whole defect it guards
 * against — an app that builds cleanly and ships no `nysia` — is invisible until somebody
 * launches it on a machine with no daemon.
 *
 * So the same function is pointed at three directories built to break it: one with a window
 * and no runtime, one with a runtime that is empty, and one with neither. Each must throw,
 * and the message must name what is missing rather than fail generically — a bundle step
 * that says only "verification failed" sends the next person looking in the wrong place.
 */
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';

import { verify } from './sidecar.ts';

const WINDOW = process.platform === 'win32' ? 'nysia-desktop.exe' : 'nysia-desktop';
const RUNTIME = process.platform === 'win32' ? 'nysia.exe' : 'nysia';

const failures: string[] = [];
const root = mkdtempSync(join(tmpdir(), 'nysia-sidecar-'));

/** `verify` must refuse `directory`, and say `expected` while doing it. */
function expectTrips(what: string, directory: string, expected: string): void {
  try {
    verify(directory);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
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
  const bare = join(root, 'bare');
  mkdirSync(bare, { recursive: true });
  expectTrips('a directory with no window at all', bare, 'no window');

  const windowOnly = join(root, 'window-only');
  mkdirSync(windowOnly, { recursive: true });
  writeFileSync(join(windowOnly, WINDOW), 'a window');
  expectTrips('a window with no runtime beside it', windowOnly, RUNTIME);

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
