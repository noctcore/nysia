/**
 * Proof that the ESLint import bans trip, and that trap 10 has been handled.
 *
 * Flat-config `no-restricted-imports` does not merge across blocks: when a later block also
 * sets the rule, it *replaces* the earlier options instead of adding to them. The
 * `apps/web/src/transport/**` block carves Tauri out of the webview ban set, and the trap
 * is that it silently drops every other ban unless it repeats them.
 *
 * Each case below is linted as a virtual file through the real `eslint.config.js` — no
 * fixtures on disk, so nothing here can drift out of the config it is testing. The
 * cross-layer section at the end needs a real file for lint-meta to walk, and writes one
 * into a throwaway directory outside the repository, removed in a `finally` — CI checks the
 * working tree is clean after every proof, and that check has to stay honest.
 */
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import process from 'node:process';

import { ESLint } from 'eslint';

import { runSourceRules } from '../tools/lint-meta/src/rules.ts';

interface Case {
  readonly what: string;
  readonly filePath: string;
  readonly code: string;
  readonly expect: 'error' | 'clean';
}

const CASES: readonly Case[] = [
  {
    what: 'a webview component importing Tauri directly',
    filePath: 'apps/web/src/Sidebar.ts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'error',
  },
  {
    what: 'a webview component importing a node builtin',
    filePath: 'apps/web/src/Sidebar.ts',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'error',
  },
  {
    what: 'the transport module importing Tauri (the carve-out)',
    filePath: 'apps/web/src/transport/channel.ts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'clean',
  },
  {
    // The trap-10 proof. If the transport block forgot to repeat BAN_NODE_BUILTINS, it
    // would replace the webview block's options and this would lint clean.
    what: 'the transport module importing a node builtin (trap 10)',
    filePath: 'apps/web/src/transport/channel.ts',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'error',
  },
  {
    what: 'tooling importing a node builtin (allowed there)',
    filePath: 'tools/lint-meta/src/probe.ts',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'clean',
  },
  {
    // The other half of trap 10: the tooling block drops the node ban, so it has to repeat
    // the Tauri one.
    what: 'tooling importing Tauri (trap 10)',
    filePath: 'tools/lint-meta/src/probe.ts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'error',
  },
  {
    // Every ban block used to be `{ts,tsx}` only, so a plain .js file under apps/web was
    // linted by neither this rule nor lint-meta's line-anchored regex.
    what: 'a .js file in the webview importing Tauri',
    filePath: 'apps/web/src/legacy.js',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'error',
  },
  {
    // The spelling that slipped past both layers: the specifier is not on the `import` line,
    // so a line-anchored regex sees nothing and the ban block did not cover .jsx at all.
    what: 'a .jsx file importing Tauri across several lines',
    filePath: 'apps/web/src/Legacy.jsx',
    code: "import {\n  Channel,\n} from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'error',
  },
  {
    what: 'a .js file in the webview importing a node builtin',
    filePath: 'apps/web/src/legacy.js',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'error',
  },
  {
    // Vite 8 resolves .mts by default, so such a file bundles. It was in neither layer.
    what: 'an .mts file importing Tauri',
    filePath: 'apps/web/src/legacy.mts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'error',
  },
  {
    what: 'a .cts file importing a node builtin',
    filePath: 'apps/web/src/legacy.cts',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'error',
  },
  {
    what: 'apps/desktop importing Tauri (its whole job)',
    filePath: 'apps/desktop/src/bridge.ts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'clean',
  },

  // ---------------------------------------------------------------------------------
  // The raw store command shape. `useCommands()` hands out verbs returning `void`, but
  // that is only a real boundary if the provider itself is out of reach — otherwise
  // `useContext(StoreContext)` gets the promise-returning commands back and
  // `void store.closeTab(key)` is available again, unhandled rejection and all.
  // ---------------------------------------------------------------------------------
  {
    // The reviewer's exact reproduction: reaches the provider directly and drops the
    // promise. Before the ban this passed tsc, eslint at --max-warnings 0, and lint-meta.
    what: 'a component reaching the provider and dropping a command promise',
    filePath: 'apps/web/src/chrome/TabStrip.tsx',
    code:
      "import { useContext } from 'react';\n" +
      "import { StoreContext } from '../store/StoreContext';\n" +
      'export function close(key: string): void {\n' +
      '  const store = useContext(StoreContext);\n' +
      '  void store?.closeTab(key);\n' +
      '}\n',
    expect: 'error',
  },
  {
    // One directory up, because the specifier is what is matched and the depth changes it.
    what: 'App.tsx importing StoreContext one level up',
    filePath: 'apps/web/src/App.tsx',
    code: "import { StoreContext } from './store/StoreContext';\nexport const c = StoreContext;\n",
    expect: 'error',
  },
  {
    what: 'a deeply nested component importing StoreContext',
    filePath: 'apps/web/src/chrome/panes/Inner.tsx',
    code: "import { StoreContext } from '../../store/StoreContext';\nexport const c = StoreContext;\n",
    expect: 'error',
  },
  {
    // `store` has to sit next to the filename for `**/store/StoreContext` to match, and
    // these two spellings put something between them while resolving to the same file on
    // disk. Both passed eslint, tsc, lint-meta and the Vite build. No auto-import writes
    // either, so this is about making a deliberate act deliberate rather than closing a
    // hole a tool could fall into — but the same patterns are what make the carve-out
    // cases above able to fail.
    what: 'a component reaching StoreContext through a single-dot segment',
    filePath: 'apps/web/src/chrome/TabStrip.tsx',
    code: "import { StoreContext } from '../store/./StoreContext';\nexport const c = StoreContext;\n",
    expect: 'error',
  },
  {
    what: 'a component reaching StoreContext through a doubled slash',
    filePath: 'apps/web/src/chrome/TabStrip.tsx',
    code: "import { StoreContext } from '../store//StoreContext';\nexport const c = StoreContext;\n",
    expect: 'error',
  },
  {
    what: 'the routed hook, which is how a component is meant to get commands',
    filePath: 'apps/web/src/chrome/TabStrip.tsx',
    code: "import { useCommands } from '../store/hooks';\nexport const u = useCommands;\n",
    expect: 'clean',
  },
  {
    // `../store/StoreContext`, deliberately not `./StoreContext`, which resolves to the
    // same file and is what a sibling module would actually write. `./StoreContext`
    // matches none of the ban patterns from any path, so the case was clean everywhere
    // and could not tell whether the carve-out existed: deleting the carve-out left this
    // passing. A carve-out case has to use a specifier the ban would otherwise catch, or
    // it is a green light wired to nothing (trap 12).
    what: 'the store module itself importing StoreContext (carve-out)',
    filePath: 'apps/web/src/store/hooks.ts',
    code: "import { StoreContext } from '../store/StoreContext';\nexport const c = StoreContext;\n",
    expect: 'clean',
  },
  {
    what: 'main.tsx importing StoreContext to compose the provider (carve-out)',
    filePath: 'apps/web/src/main.tsx',
    code: "import { StoreContext } from './store/StoreContext';\nexport const c = StoreContext;\n",
    expect: 'clean',
  },
  {
    // Trap 10 on the new carve-out block. It drops BAN_STORE_CONTEXT, so it has to repeat
    // the other three — written as a one-off carve-out it would re-open all of them here.
    what: 'the store module importing Tauri (trap 10)',
    filePath: 'apps/web/src/store/hooks.ts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'error',
  },
  {
    what: 'the store module importing a node builtin (trap 10)',
    filePath: 'apps/web/src/store/hooks.ts',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'error',
  },
  {
    what: 'main.tsx importing a node builtin (trap 10)',
    filePath: 'apps/web/src/main.tsx',
    code: "import { readFileSync } from 'node:fs';\nexport const r = readFileSync;\n",
    expect: 'error',
  },
  {
    // Trap 10 on the transport block, which is not a store carve-out and had to add the
    // new ban to keep it. Without that it would be the one module still able to reach the
    // raw provider.
    what: 'the transport module importing StoreContext (trap 10)',
    filePath: 'apps/web/src/transport/channel.ts',
    code: "import { StoreContext } from '../store/StoreContext';\nexport const c = StoreContext;\n",
    expect: 'error',
  },
];

const RULE = 'no-restricted-imports';

const eslint = new ESLint();
const failures: string[] = [];

process.stdout.write('prove:eslint-bans\n');

for (const testCase of CASES) {
  const results = await eslint.lintText(testCase.code, { filePath: testCase.filePath });
  const hits = results.flatMap((r) => r.messages).filter((m) => m.ruleId === RULE);
  const tripped = hits.length > 0;
  const wanted = testCase.expect === 'error';

  if (tripped !== wanted) {
    failures.push(
      `${testCase.what} (${testCase.filePath}): expected ${testCase.expect}, ` +
        `got ${tripped ? 'error' : 'clean'}`,
    );
  } else {
    process.stdout.write(`  ${testCase.expect === 'error' ? 'trips ' : 'allows'}: ${testCase.what}\n`);
  }
}

// ---------------------------------------------------------------------------------------
// The two layers answer the same question the same way (#20).
//
// `eslint.config.js` and `tools/lint-meta/src/rules.ts` used to hold two hand-written copies
// of the store carve-out, described as mirrored so they could not disagree silently. Nothing
// cross-checked them and they already disagreed: lint-meta allowlisted by the prefix
// `apps/web/src/main.`, ESLint carved out `main.{ts,tsx,…}`. `apps/web/src/main.helper.tsx`
// was therefore allowlisted by one layer and banned by the other, so a dynamic import of the
// provider from it passed both — ESLint cannot see `import()`, and lint-meta thought the file
// was the entry point.
//
// They now read one list, and this is what says so: for each path, ask ESLint whether a
// static import of the provider is banned there, ask lint-meta whether a dynamic one is, and
// require the same answer. A case that only lints through ESLint could not have caught the
// defect — ESLint's half was already right.
// ---------------------------------------------------------------------------------------
const MIRROR_PATHS: readonly string[] = [
  // The carve-out, in the extension it has and one it could grow into.
  'apps/web/src/main.tsx',
  'apps/web/src/main.mts',
  // The store module, at the top and nested.
  'apps/web/src/store/hooks.ts',
  'apps/web/src/store/deep/nested.ts',
  // The disagreement. A file whose name merely begins with the entry point's.
  'apps/web/src/main.helper.tsx',
  'apps/web/src/main.config.ts',
  // A directory that begins with it, which neither layer has ever carved out.
  'apps/web/src/main/index.ts',
  // Ordinary webview files, one of them the transport — which is not a store carve-out.
  'apps/web/src/mainly.ts',
  'apps/web/src/chrome/TabStrip.tsx',
  'apps/web/src/transport/bridge.ts',
  // Outside the webview entirely, where neither layer bans anything.
  'apps/desktop/src/bridge.ts',
];

/** Does ESLint ban a static import of the provider from this path? */
async function eslintBansProvider(filePath: string): Promise<boolean> {
  const results = await eslint.lintText(
    "import { StoreContext } from '../store/StoreContext';\nexport const c = StoreContext;\n",
    { filePath },
  );
  return results.flatMap((r) => r.messages).some((m) => m.ruleId === RULE);
}

/**
 * Does lint-meta ban a dynamic import of the provider from this path?
 *
 * Written into a throwaway tree and run through the real rule rather than asked of the
 * allowlist directly: a proof that consults the same constant the rule consults can only
 * ever agree with itself. CI checks the working tree is clean afterwards, so nothing here
 * may touch the repository.
 */
function lintMetaBansProvider(filePath: string): boolean {
  const root = mkdtempSync(join(tmpdir(), 'nysia-mirror-'));
  try {
    const absolute = join(root, filePath);
    mkdirSync(dirname(absolute), { recursive: true });
    writeFileSync(
      absolute,
      "const store = await import('../store/StoreContext');\nexport const c = store;\n",
    );
    return runSourceRules(root).some((v) => v.rule === 'no-store-context-outside-store');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

for (const filePath of MIRROR_PATHS) {
  const byEslint = await eslintBansProvider(filePath);
  const byLintMeta = lintMetaBansProvider(filePath);

  if (byEslint !== byLintMeta) {
    failures.push(
      `the two layers disagree about ${filePath}: eslint ` +
        `${byEslint ? 'bans' : 'allows'} it, lint-meta ${byLintMeta ? 'bans' : 'allows'} it`,
    );
  } else {
    process.stdout.write(
      `  agree (${byEslint ? 'ban ' : 'allow'}): ${filePath} reaching the store provider\n`,
    );
  }
}

if (failures.length > 0) {
  for (const failure of failures) process.stderr.write(`prove:eslint-bans FAILED — ${failure}\n`);
  process.exit(1);
}

process.stdout.write('prove:eslint-bans OK — every ban trips and every carve-out holds\n');
