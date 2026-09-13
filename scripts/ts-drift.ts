/**
 * The ts-rs drift guard.
 *
 * Regenerates the bindings into a temp directory and diffs them against what is committed
 * under `apps/web/src/generated`. Any difference — changed, missing, or extra file — fails.
 *
 * Two details this script exists to get right:
 *
 *   1. cargo runs with **cwd = crates/nysia-proto**, never `--manifest-path` from the root.
 *      Cargo finds `.cargo/config.toml` by walking up from the working directory; run from
 *      the root, the ts-rs environment is unset, the bindings land in a gitignored
 *      `bindings/`, and this guard passes without having compared anything
 *      (traps register #1).
 *   2. The comparison is bidirectional and newline-normalised, so a stray committed file
 *      fails and a CRLF checkout does not.
 *
 * Exit codes: 0 clean, 1 drift, 2 the generator itself failed.
 */
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, posix, relative, sep } from 'node:path';
import process from 'node:process';

import { findRepoRoot } from '../tools/lint-meta/src/repoRoot.ts';

const EXIT_DRIFT = 1;
const EXIT_GENERATOR_FAILED = 2;

const repoRoot = findRepoRoot();
const protoCrate = join(repoRoot, 'crates', 'nysia-proto');
const committed = join(repoRoot, 'apps', 'web', 'src', 'generated');

/**
 * Every `.ts` file under `dir`, as a POSIX-separated path relative to it.
 *
 * Recursive, and that is load-bearing. `#[ts(export_to = "rpc/")]` puts a binding in a
 * subdirectory, and a top-level-only listing drops it from *both* sides of the diff at
 * once: the guard then reports "N bindings match" and has silently stopped checking the
 * nested ones. `scripts/prove-ts-drift.ts` exports its probe to `drift_probe/` precisely so
 * that this recursion is proven rather than assumed.
 */
function listTypeScript(dir: string): string[] {
  const found: string[] = [];
  const visit = (current: string): void => {
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const absolute = join(current, entry.name);
      if (entry.isDirectory()) {
        visit(absolute);
      } else if (entry.isFile() && entry.name.endsWith('.ts')) {
        found.push(relative(dir, absolute).split(sep).join(posix.sep));
      }
    }
  };
  visit(dir);
  // Sorted so the report reads the same on both runners.
  return found.sort();
}

/** Normalise line endings so a CRLF checkout is not reported as drift. */
function normalise(path: string): string {
  return readFileSync(path, 'utf8').replace(/\r\n/g, '\n');
}

function generateInto(target: string): { ok: true } | { ok: false; output: string } {
  const result = spawnSync('cargo', ['test', '--quiet', 'export_bindings'], {
    // cwd = the crate, so `.cargo/config.toml` is on cargo's lookup path.
    cwd: protoCrate,
    // `force` is absent from that config, so an environment value wins — which is the
    // only reason redirecting the export here works at all.
    env: { ...process.env, TS_RS_EXPORT_DIR: target },
    encoding: 'utf8',
    shell: false,
  });
  if (result.status === 0) return { ok: true };
  return { ok: false, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

function main(): number {
  const scratch = mkdtempSync(join(tmpdir(), 'nysia-ts-drift-'));
  try {
    const generated = generateInto(scratch);
    if (!generated.ok) {
      process.stderr.write('ts-drift: the ts-rs export failed, so nothing was compared.\n');
      process.stderr.write(generated.output);
      return EXIT_GENERATOR_FAILED;
    }

    const fresh = listTypeScript(scratch);
    const onDisk = listTypeScript(committed);
    if (fresh.length === 0) {
      process.stderr.write(
        'ts-drift: the export produced no bindings. Check that cargo ran with ' +
          'cwd = crates/nysia-proto (traps register #1).\n',
      );
      return EXIT_GENERATOR_FAILED;
    }

    const problems: string[] = [];
    for (const name of [...new Set([...fresh, ...onDisk])].sort()) {
      const inFresh = fresh.includes(name);
      const inCommitted = onDisk.includes(name);
      if (!inCommitted) {
        problems.push(`+ ${name} is generated but not committed`);
        continue;
      }
      if (!inFresh) {
        problems.push(`- ${name} is committed but no longer generated`);
        continue;
      }
      const expected = normalise(join(scratch, name));
      const actual = normalise(join(committed, name));
      if (expected !== actual) {
        problems.push(`~ ${name} differs:\n${diff(expected, actual)}`);
      }
    }

    if (problems.length > 0) {
      process.stderr.write('ts-drift: committed bindings do not match nysia-proto.\n');
      for (const problem of problems.sort()) process.stderr.write(`  ${problem}\n`);
      process.stderr.write(
        '\nRegenerate with: cd crates/nysia-proto && cargo test export_bindings\n',
      );
      return EXIT_DRIFT;
    }

    process.stdout.write(`ts-drift: ${fresh.length} binding(s) match nysia-proto\n`);
    return 0;
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/** A minimal line diff — enough to see what moved without pulling in a dependency. */
function diff(expected: string, actual: string): string {
  const want = expected.split('\n');
  const have = actual.split('\n');
  const lines: string[] = [];
  for (let i = 0; i < Math.max(want.length, have.length); i += 1) {
    if (want[i] !== have[i]) {
      if (have[i] !== undefined) lines.push(`      committed: ${have[i]}`);
      if (want[i] !== undefined) lines.push(`      generated: ${want[i]}`);
    }
  }
  return lines.join('\n');
}

process.exit(main());
