import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * The repo root, found by walking up for `pnpm-workspace.yaml`.
 *
 * Never `CARGO_MANIFEST_DIR` and never a path baked at build time (traps register #8): a
 * release once shipped the CI runner's directory because a path was frozen into the
 * artifact.
 */
export function findRepoRoot(from = dirname(fileURLToPath(import.meta.url))): string {
  let current = from;
  for (;;) {
    if (existsSync(join(current, 'pnpm-workspace.yaml'))) return current;
    const parent = dirname(current);
    if (parent === current) {
      throw new Error(`no pnpm-workspace.yaml above ${from}`);
    }
    current = parent;
  }
}
