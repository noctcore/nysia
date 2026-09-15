/**
 * The dev-run half of `sidecar.ts`: which profile a `tauri dev` invocation runs, and the one
 * property that keeps `pnpm dev` out of the D-1 failure.
 *
 * Both halves are here because both are claims the header makes and neither is visible from
 * a green build. The profile mapping decides *which* `target/<profile>/nysia` the window will
 * look beside, and getting it wrong reproduces #74 under `--release` — in the one place
 * nobody would go looking for it, because `pnpm dev` works. And `ensure` doing **nothing**
 * when the runtime is already there is not an optimisation: it is what stops the dev run ever
 * rewriting the file a surviving daemon is running from, which is the whole reason the
 * condition is existence rather than freshness.
 */
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { ensure, profileFor, runtimeFor } from './sidecar.ts';

const RUNTIME = process.platform === 'win32' ? 'nysia.exe' : 'nysia';

describe('profileFor', () => {
  it('reads --release as the release profile', () => {
    expect(profileFor(['--release'])).toEqual({ directory: 'release', cargoFlags: ['--release'] });
  });

  it('defaults to debug, which is what tauri dev runs with no flag', () => {
    expect(profileFor([])).toEqual({ directory: 'debug', cargoFlags: [] });
  });

  it('still finds --release among other tauri arguments', () => {
    expect(profileFor(['--no-watch', '--release', '-v']).directory).toBe('release');
  });

  // The shape pnpm actually produces. `pnpm dev -- --release` reaches this script as the two
  // words `--` `--release`, so a rule that stopped reading at the separator would build debug
  // for the one invocation a developer would type to get release — #74 again, under the flag
  // nobody would suspect. Measured against pnpm 10.33.2, and the reason `--` is not a stop.
  it('reads --release through the -- that pnpm forwards', () => {
    expect(profileFor(['--', '--release']).directory).toBe('release');
    expect(profileFor(['-v', '--', '--release']).directory).toBe('release');
  });

  // Every built-in, because half of them are not named after their own directory and an
  // earlier revision of this test pinned the pretty version instead of the measured one —
  // `bench` to `target/bench`, which cargo never writes. A wrong directory here is not a
  // cosmetic error: `ensure` would check a path that never appears, rebuild on every run, and
  // so degrade back into the unconditional build the absent-only rule exists to avoid.
  //
  // The table is from cargo 1.97.1, one `--profile` per build against a throwaway crate.
  it.each([
    ['dev', 'debug'],
    ['test', 'debug'],
    ['release', 'release'],
    ['bench', 'release'],
  ])('maps the built-in %s profile to target/%s', (profile, directory) => {
    expect(profileFor(['--profile', profile])).toEqual({
      directory,
      cargoFlags: ['--profile', profile],
    });
  });

  it('gives a profile cargo does not know a directory of its own name', () => {
    // What a manifest-declared profile gets. This workspace declares none, so nothing here
    // can reach it today — it is the fallback that makes the table above the exception list
    // rather than the whole world.
    expect(profileFor(['--', '--profile', 'bespoke']).directory).toBe('bespoke');
  });

  it('falls back to debug for an argument that selects no profile', () => {
    expect(profileFor(['--wat']).directory).toBe('debug');
    // `--profile` with nothing after it selects nothing; cargo would reject it anyway.
    expect(profileFor(['--profile']).directory).toBe('debug');
  });
});

describe('ensure', () => {
  let root: string;

  beforeEach(() => {
    root = mkdtempSync(join(tmpdir(), 'nysia-ensure-'));
  });

  afterEach(() => {
    rmSync(root, { recursive: true, force: true });
  });

  it('does nothing at all when the runtime is already there', () => {
    const profile = profileFor([]);
    mkdirSync(join(root, 'target', profile.directory), { recursive: true });
    writeFileSync(runtimeFor(profile, root), 'a runtime');

    // False is the whole assertion: it did not build, so it did not touch a file a daemon
    // could be running from. This temp root holds no Cargo.toml, so had it reached cargo the
    // call would have thrown — which the next test relies on, and is what keeps this one from
    // passing vacuously.
    expect(ensure(profile, root)).toBe(false);
  });

  it('reaches cargo when the runtime is absent', () => {
    // The mirror image, and the reason the test above means something. No manifest here, so
    // cargo refuses and `ensure` reports it rather than handing a window a missing runtime.
    expect(() => ensure(profileFor([]), root)).toThrow(/cargo build -p nysia/);
  });

  it('looks for the release runtime under --release', () => {
    const profile = profileFor(['--release']);
    mkdirSync(join(root, 'target', 'release'), { recursive: true });
    writeFileSync(join(root, 'target', 'release', RUNTIME), 'a runtime');

    expect(ensure(profile, root)).toBe(false);
    // And a debug runtime does not satisfy a release run, which is the #74 recurrence the
    // mapping exists to prevent.
    expect(() => ensure(profileFor([]), root)).toThrow(/cargo build -p nysia/);
  });
});
