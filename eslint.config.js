import js from '@eslint/js';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';
import globals from 'globals';
import tseslint from 'typescript-eslint';

/*
 * Trap 10, which has already cost someone a day: ESLint flat-config
 * `no-restricted-imports` does **not** merge across blocks. When two blocks both match a
 * file, the later one silently *replaces* the earlier rule's options rather than adding to
 * them — so a block that carves out one import re-opens every other import it forgot to
 * repeat.
 *
 * The defence is mechanical: each ban lives in a named constant, and every block that sets
 * `no-restricted-imports` lists the full set it means to enforce, including the ones it
 * inherited in spirit. `pnpm prove:eslint-bans` lints virtual files through this config and
 * fails if a carve-out block has dropped a ban.
 *
 * Every block matches the same deliberate extension set, repeated verbatim in
 * `tools/lint-meta/src/rules.ts` (`SOURCE_EXTENSIONS`), which cannot import from here:
 *
 *   In:  ts tsx mts cts js jsx mjs cjs
 *   Out: json, css, html, svg — none of them can import anything.
 *
 * `.mts` and `.cts` are in the list because Vite 8's default `resolve.extensions` includes
 * `.mts`, so such a file bundles; leaving them out left a file covered by neither layer.
 *
 * One thing this rule cannot see: `no-restricted-imports` does not cover `require()`, and
 * `no-restricted-modules` was removed in ESLint 9. The split is deliberate — ESLint owns
 * `import` and `export … from`, lint-meta owns `require()` and dynamic `import()`.
 */

/**
 * The Tauri boundary. This allowlist is duplicated in `tools/lint-meta/src/rules.ts`
 * (`TAURI_ALLOWLIST`) and the two must agree: lint-meta covers Rust and the files ESLint
 * does not lint, ESLint gives the error at the import site.
 */
const BAN_TAURI = {
  group: ['@tauri-apps', '@tauri-apps/**'],
  message:
    'Only apps/desktop/** and apps/web/src/transport/** may import Tauri (D-1, D-2). ' +
    'Everything else reaches the daemon through the transport module.',
};

/** apps/web is bundled for a webview; node builtins do not exist there. */
const BAN_NODE_BUILTINS = {
  group: ['node:*', 'fs', 'fs/*', 'path', 'os', 'child_process', 'crypto'],
  message: 'apps/web is a browser bundle — node builtins are not available at runtime.',
};

/**
 * The store's command surface, made unreachable rather than merely relocated.
 *
 * `useStore()` is gone and `useCommands()` hands components verbs that return `void`, so
 * there is no promise left for a call site to drop. But a component could still reach past
 * that with `useContext(StoreContext)` and get the raw provider back, whose commands return
 * promises — and `void store.closeTab(key)` compiles, lints clean and produces an unhandled
 * rejection with nothing on screen. That is the exact failure `useCommands` exists to
 * remove, walking back in through a side door on the tenth call site.
 *
 * `store/commands.ts` says the door "cannot be locked from here" because the lint config
 * lives outside that package. This is that lock, from here.
 *
 * Two carve-outs, and only two: `store/**` is the module itself, and `main.tsx` composes
 * the provider — the one line wave 2 changes when the mock store becomes the daemon-backed
 * one. Everything else goes through `useCommands()`.
 */
const BAN_STORE_CONTEXT = {
  group: ['**/store/StoreContext', '**/store/StoreContext.*'],
  message:
    'Components reach the store through useCommands() / useSnapshot() from store/hooks, ' +
    'never through StoreContext. The context hands back the raw provider, whose commands ' +
    'return promises that a call site can drop silently; the routed verbs return void.',
};

/**
 * Wire types come from `nysia-proto` via ts-rs. A hand-written copy is a second authority
 * on the wire, which is exactly what D-13 removes.
 */
const BAN_GENERATED_COPIES = {
  group: ['**/generated/**/*.js', '**/generated/index*'],
  message:
    'Import the generated type directly (e.g. ./generated/SessionKind). There is no barrel: ' +
    'a hand-written one would be an uncommitted file the ts-rs drift guard rejects.',
};

export default tseslint.config(
  {
    ignores: [
      '**/dist/**',
      '**/target/**',
      '**/node_modules/**',
      '**/bindings/**',
      'apps/web/src/generated/**',
      // Deliberately broken trees; `pnpm prove:lint-meta` is what reads them.
      'tools/lint-meta/fixtures/**',
    ],
  },

  js.configs.recommended,
  ...tseslint.configs.recommended,

  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-non-null-assertion': 'error',
      '@typescript-eslint/consistent-type-imports': [
        'error',
        { prefer: 'type-imports', fixStyle: 'separate-type-imports' },
      ],
      eqeqeq: ['error', 'always'],
      'no-console': ['error', { allow: ['warn', 'error'] }],
    },
  },

  // ---------------------------------------------------------------------------------
  // apps/web — the browser bundle. Full ban set.
  // ---------------------------------------------------------------------------------
  {
    files: ['apps/web/**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}'],
    languageOptions: {
      globals: globals.browser,
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    plugins: {
      'react-hooks': reactHooks,
      'react-refresh': reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
      'no-restricted-imports': [
        'error',
        { patterns: [BAN_TAURI, BAN_NODE_BUILTINS, BAN_GENERATED_COPIES, BAN_STORE_CONTEXT] },
      ],
    },
  },

  // ---------------------------------------------------------------------------------
  // apps/web/src/transport — the one module allowed to hold a Tauri Channel (wave 2, W5).
  //
  // Trap 10: this block replaces the block above for these files, so it must repeat every
  // ban it still wants. Dropping BAN_NODE_BUILTINS here would silently allow `node:fs`
  // into the bundle, and dropping BAN_STORE_CONTEXT would make the transport the one place
  // that could still reach the raw provider. The transport is not a store carve-out.
  // ---------------------------------------------------------------------------------
  {
    files: ['apps/web/src/transport/**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}'],
    rules: {
      'no-restricted-imports': [
        'error',
        { patterns: [BAN_NODE_BUILTINS, BAN_GENERATED_COPIES, BAN_STORE_CONTEXT] },
      ],
    },
  },

  // ---------------------------------------------------------------------------------
  // The two store carve-outs: the module itself, and the entry point that composes the
  // provider. Both must come after the apps/web block to replace it.
  //
  // Trap 10 again, and this is the block where dropping a ban would be least visible:
  // these files are allowed to name StoreContext, so the temptation is to write the rule
  // as "just that one off". Written that way it would also re-open Tauri, node builtins
  // and the generated barrel for `store/**` — the full set is repeated for that reason.
  // ---------------------------------------------------------------------------------
  {
    files: [
      'apps/web/src/store/**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}',
      'apps/web/src/main.{ts,tsx,mts,cts,js,jsx,mjs,cjs}',
    ],
    rules: {
      'no-restricted-imports': [
        'error',
        { patterns: [BAN_TAURI, BAN_NODE_BUILTINS, BAN_GENERATED_COPIES] },
      ],
    },
  },

  // ---------------------------------------------------------------------------------
  // Node-side tooling. Node builtins are the point here, so that ban is dropped — but the
  // Tauri ban is repeated, because trap 10 means it would otherwise be gone.
  // ---------------------------------------------------------------------------------
  {
    files: [
      'tools/**/*.{ts,mts,cts,js,mjs,cjs}',
      'scripts/**/*.{ts,mts,cts,js,mjs,cjs}',
      '*.config.{ts,mts,cts,js,mjs,cjs}',
      'eslint.config.js',
    ],
    languageOptions: {
      globals: globals.node,
    },
    rules: {
      'no-console': 'off',
      'no-restricted-imports': ['error', { patterns: [BAN_TAURI, BAN_GENERATED_COPIES] }],
    },
  },

  // ---------------------------------------------------------------------------------
  // apps/desktop — the shell itself. Tauri is its whole job, so no import ban applies.
  // ---------------------------------------------------------------------------------
  {
    files: ['apps/desktop/**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}'],
    languageOptions: {
      globals: globals.browser,
    },
  },
);
