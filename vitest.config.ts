import { defineConfig } from 'vitest/config';

/*
 * D-18: vitest, node-only. No browser mode and no jsdom in v1 — browser-mode is the
 * slowest CI job for a UI that is mostly a terminal, and the terminal's behaviour is
 * tested in Rust, where the state actually lives (D-7).
 *
 * Anything that genuinely needs a DOM is tested end to end in wave 3 instead.
 */
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'lint-meta',
          root: 'tools/lint-meta',
          environment: 'node',
          include: ['src/**/*.test.ts'],
        },
      },
      {
        test: {
          name: 'scripts',
          root: 'scripts',
          environment: 'node',
          include: ['**/*.test.ts'],
        },
      },
      {
        test: {
          name: 'web',
          root: 'apps/web',
          environment: 'node',
          include: ['src/**/*.test.ts'],
        },
      },
    ],
  },
});
