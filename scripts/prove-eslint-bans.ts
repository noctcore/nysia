/**
 * Proof that the ESLint import bans trip, and that trap 10 has been handled.
 *
 * Flat-config `no-restricted-imports` does not merge across blocks: when a later block also
 * sets the rule, it *replaces* the earlier options instead of adding to them. The
 * `apps/web/src/transport/**` block carves Tauri out of the webview ban set, and the trap
 * is that it silently drops every other ban unless it repeats them.
 *
 * Each case below is linted as a virtual file through the real `eslint.config.js` — no
 * fixtures on disk, so nothing here can drift out of the config it is testing.
 */
import process from 'node:process';

import { ESLint } from 'eslint';

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
    what: 'apps/desktop importing Tauri (its whole job)',
    filePath: 'apps/desktop/src/bridge.ts',
    code: "import { Channel } from '@tauri-apps/api/core';\nexport const c = Channel;\n",
    expect: 'clean',
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

if (failures.length > 0) {
  for (const failure of failures) process.stderr.write(`prove:eslint-bans FAILED — ${failure}\n`);
  process.exit(1);
}

process.stdout.write('prove:eslint-bans OK — every ban trips and every carve-out holds\n');
