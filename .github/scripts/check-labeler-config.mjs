// Four invariants over .github/labeler.yml.
//
// (a) A negated glob may not sit under an `any-glob-*` key. Those keys OR their globs,
//     so a negation there either does nothing or matches everything.
// (b) A negation in a separate `changed-files` entry from the positive globs must be
//     wrapped in `- all:`, or the two entries are OR-ed instead of AND-ed.
// (c) The labeler may never write a TYPE label — only areas and extras.
// (d) Every label it writes must exist in .github/labels.yml.
//
// WHY (a) EXISTS, AND WHY IT IS NOT THE SAME RULE AS (b).
//
// (b) came first, from `area:web` being written as two entries without the wrapper. The
// fix for that shape is the `- all:` wrapper, and there it works. But the rule as first
// written looked for the literal text `all-globs-to-all-files`, or a block-style `- '!…'`
// item, so a negation written in flow style —
//
//     - any-glob-to-any-file: ['crates/agent/**', '!crates/agent/fixtures/**']
//
// — was never examined at all, and the file passed with its success line while the real
// labeler applied that label to a pty-only pull request, a docs-only one, and an empty one.
//
// Worse, the remediation message told the author to wrap it in `- all:`, and doing that
// changes nothing: `checkIfAnyGlobMatchesAnyFile` ORs the globs inside a single key
// regardless of any wrapper, and `!crates/agent/fixtures/**` matches every file that is
// not a fixture — which is almost every file. A check that hands out advice which does not
// work is worse than one that says nothing, so (a) is its own rule with its own message,
// naming the key that actually expresses an exclusion.
//
// WHERE A NEGATION BELONGS. `all-globs-to-any-file` ANDs its globs against each file in
// turn, which is a real per-file exclusion:
//
//     - all-globs-to-any-file: ['apps/web/**', '!apps/web/src/generated/**']
//
// reads "some changed file is under apps/web and is not generated". The `- all:` plus
// `all-globs-to-all-files` form also excludes correctly, but asks the question of the
// whole pull request at once, so one generated file suppresses the label for every
// hand-written file beside it. Both pass; the message recommends the first.
//
// Not a YAML parser. It splits on label keys, which start at column zero here, and reads
// the four glob keys with their lists — block style, flow style and bare scalar — because
// these rules are about which globs sit under which key, and nothing coarser can see that.
// It is wrong only in the direction of a false positive a human fixes in one line.
//
// Run with `--self-test` for the fixtures, with no arguments to check the real file.

import { readFileSync } from 'node:fs';
import process from 'node:process';

import { TYPE_LABELS } from './facets.mjs';

const CONFIG_PATH = '.github/labeler.yml';
const LABELS_PATH = '.github/labels.yml';

/**
 * The four glob keys, and whether the key ORs its globs.
 *
 * The `any-glob-*` pair returns true as soon as one glob matches, so a negation beside a
 * positive glob is never an exclusion there. The `all-globs-*` pair requires every glob,
 * which is what makes an exclusion mean what it says.
 */
const GLOB_KEYS = {
  'any-glob-to-any-file': { ors: true },
  'any-glob-to-all-files': { ors: true },
  'all-globs-to-any-file': { ors: false },
  'all-globs-to-all-files': { ors: false },
};

/**
 * Label names declared in labels.yml.
 *
 * @param {string} text
 * @returns {Set<string>}
 */
export function declaredLabels(text) {
  const names = new Set();
  for (const m of text.matchAll(/^- name: "((?:[^"\\]|\\.)*)"$/gm)) names.add(JSON.parse(`"${m[1]}"`));
  return names;
}

/**
 * Split a YAML flow sequence — `['a', "b", c]` — into its items.
 *
 * @param {string} inline
 * @returns {string[]}
 */
function flowItems(inline) {
  const body = inline.replace(/^\[/, '').replace(/\]$/, '');
  const items = [];
  for (const m of body.matchAll(/'([^']*)'|"([^"]*)"|([^,\s][^,]*)/g)) {
    const value = m[1] ?? m[2] ?? (m[3] ?? '').trim();
    if (value !== '') items.push(value);
  }
  return items;
}

/** @param {string} value */
const unquote = (value) => value.trim().replace(/^['"]|['"]$/g, '');

/**
 * Every glob key in a label's block, with the globs under it.
 *
 * Handles the three spellings the labeler accepts:
 *
 *     - key: ['a', '!b']      flow
 *     - key: 'a'              scalar
 *     - key:                  block
 *         - 'a'
 *         - '!b'
 *
 * @param {string[]} lines the block's lines, comments already removed
 * @returns {{ key: string, globs: string[] }[]}
 */
export function parseGlobKeys(lines) {
  const found = [];

  for (let i = 0; i < lines.length; i += 1) {
    const m = /^(\s*)-?\s*(["']?)([a-z-]+)\2\s*:\s*(.*)$/.exec(lines[i]);
    if (m === null) continue;
    const [, indent, , key, inline] = m;
    if (!Object.hasOwn(GLOB_KEYS, key)) continue;

    const rest = inline.trim();
    if (rest.startsWith('[')) {
      found.push({ key, globs: flowItems(rest) });
      continue;
    }
    if (rest !== '') {
      found.push({ key, globs: [unquote(rest)] });
      continue;
    }

    // Block style: the more-indented `- …` items that follow.
    const globs = [];
    for (let j = i + 1; j < lines.length; j += 1) {
      const item = /^(\s*)-\s*(.+)$/.exec(lines[j]);
      if (item === null) {
        if (lines[j].trim() === '') continue;
        break;
      }
      if (item[1].length <= indent.length) break;
      globs.push(unquote(item[2]));
    }
    found.push({ key, globs });
  }

  return found;
}

/**
 * @param {string} text the contents of a labeler config
 * @param {Set<string> | null} known label names from labels.yml, or null to skip (d)
 * @returns {string[]} one message per violation
 */
export function checkLabelerConfig(text, known = null) {
  const problems = [];

  /** @type {{ label: string, body: string[] } | null} */
  let current = null;
  const blocks = [];
  for (const line of text.split('\n')) {
    if (/^[^\s#]/.test(line)) {
      if (current !== null) blocks.push(current);
      current = { label: line.replace(/:\s*$/, '').replace(/^['"]|['"]$/g, ''), body: [] };
    } else if (current !== null) {
      current.body.push(line);
    }
  }
  if (current !== null) blocks.push(current);

  for (const { label, body } of blocks) {
    // Comments inside a block explain these spellings; they are not configuration.
    const code = body.filter((l) => !/^\s*#/.test(l));
    const keys = parseGlobKeys(code);
    const wrapped = code.some((l) => /^\s*-\s*all:\s*$/.test(l));

    for (const { key, globs } of keys) {
      const negated = globs.filter((g) => g.startsWith('!'));
      if (negated.length > 0 && GLOB_KEYS[key].ors) {
        problems.push(
          `${label}: has the negated glob ${negated[0]} under ${key}, which ORs its globs — the negation then matches every file it does not exclude, so the label lands on nearly every pull request. Wrapping this in "- all:" does NOT fix it. Put the positive and negated globs together under all-globs-to-any-file, which ANDs them against each file: all-globs-to-any-file: ['<positive>', '${negated[0]}'].`,
        );
      }
    }

    const negationOnly = keys.filter((k) => k.globs.length > 0 && k.globs.every((g) => g.startsWith('!')));
    const positiveKeys = keys.filter((k) => k.globs.some((g) => !g.startsWith('!')));
    if (negationOnly.length > 0 && positiveKeys.length > 0 && !wrapped) {
      problems.push(
        `${label}: puts its negated globs in a separate entry (${negationOnly[0].key}) from its positive ones (${positiveKeys[0].key}) without a "- all:" wrapper. Entries are OR-ed by default, so the exclusion becomes an independent branch that matches nearly every pull request. Either wrap both entries in "- all:", or better, merge them into one all-globs-to-any-file list, which excludes per file rather than across the whole pull request.`,
      );
    }

    if (TYPE_LABELS.includes(label)) {
      problems.push(
        `${label}: is a TYPE label, and the labeler may never write one. A path can tell you which area a change is in, but only the author knows whether it is a bug or a chore — and because pr-facets.yml requires exactly one type, a type written here collides with the author's own and fails the pull request intermittently. Move it out of this file; areas and extras only.`,
      );
    }

    if (known !== null && !known.has(label)) {
      problems.push(
        `${label}: is not declared in ${LABELS_PATH}. The labeler would try to apply a label that may not exist in the repository. Add it there first, or fix the spelling.`,
      );
    }
  }

  if (blocks.length === 0) {
    problems.push('no labels found; the check cannot have run against a real config');
  }

  return problems;
}

function selfTest() {
  const failures = [];
  /**
   * @param {string} what
   * @param {boolean} held
   */
  const check = (what, held) => {
    if (!held) failures.push(what);
  };

  const plain = `'area:pty':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'crates/nysia-core/src/pty/**'\n`;
  check('a single positive glob passes', checkLabelerConfig(plain).length === 0);

  // The parser first, since every rule below rests on it.
  check(
    'block style is read',
    JSON.stringify(parseGlobKeys(['      - any-glob-to-any-file:', "          - 'a/**'", "          - '!b/**'"])) ===
      JSON.stringify([{ key: 'any-glob-to-any-file', globs: ['a/**', '!b/**'] }]),
  );
  check(
    'flow style is read',
    JSON.stringify(parseGlobKeys(["      - any-glob-to-any-file: ['a/**', '!b/**']"])) ===
      JSON.stringify([{ key: 'any-glob-to-any-file', globs: ['a/**', '!b/**'] }]),
  );
  check(
    'a bare scalar is read',
    JSON.stringify(parseGlobKeys(['      - any-glob-to-any-file: docs/**'])) ===
      JSON.stringify([{ key: 'any-glob-to-any-file', globs: ['docs/**'] }]),
  );
  check(
    'a double-quoted flow item is read',
    parseGlobKeys(['      - all-globs-to-any-file: ["a/**", "!b/**"]'])[0].globs[1] === '!b/**',
  );
  check(
    'a following key does not swallow the previous block',
    parseGlobKeys([
      '      - any-glob-to-any-file:',
      "          - 'a/**'",
      '      - all-globs-to-all-files:',
      "          - '!b/**'",
    ]).length === 2,
  );

  // (a) A negation under an OR-ing key. This is the exact block that passed the previous
  // version with its success line while the real labeler applied the label to a pty-only
  // pull request, a docs-only one, and an empty one.
  const flowNegation = `'area:agent':\n  - changed-files:\n      - any-glob-to-any-file: ['crates/agent/**', '!crates/agent/fixtures/**']\n`;
  const flowProblems = checkLabelerConfig(flowNegation);
  check('a flow-style negation under an any-glob key fails', flowProblems.length === 1);
  check('the message names the offending glob', (flowProblems[0] ?? '').includes('!crates/agent/fixtures/**'));
  check('the message names the OR-ing key', (flowProblems[0] ?? '').includes('any-glob-to-any-file'));
  // The advice the old message gave, which did not work.
  check('the message says the all: wrapper will not fix it', (flowProblems[0] ?? '').includes('does NOT fix it'));
  check('the message names the key that does', (flowProblems[0] ?? '').includes('all-globs-to-any-file'));

  const blockNegation = `'area:agent':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'crates/agent/**'\n          - '!crates/agent/fixtures/**'\n`;
  check('the same mistake in block style fails', checkLabelerConfig(blockNegation).length === 1);

  // Wrapping the OR-ing key in `all:` must NOT silence the rule — that was the whole bug.
  const wrappedButWrong = `'area:agent':\n  - all:\n      - changed-files:\n          - any-glob-to-any-file: ['crates/agent/**', '!crates/agent/fixtures/**']\n`;
  check('an all: wrapper does not excuse a negation under an OR-ing key', checkLabelerConfig(wrappedButWrong).length === 1);

  // The same gap one key over: any-glob-to-all-files ORs its globs as well.
  const anyToAll = `'area:agent':\n  - changed-files:\n      - any-glob-to-all-files: ['crates/agent/**', '!crates/agent/fixtures/**']\n`;
  check('a negation under any-glob-to-all-files fails too', checkLabelerConfig(anyToAll).length === 1);

  // The shape the message recommends has to be one this check accepts, or the advice
  // sends the author into a second failure.
  const advised = `'area:agent':\n  - changed-files:\n      - all-globs-to-any-file: ['crates/agent/**', '!crates/agent/fixtures/**']\n`;
  check('the shape the message recommends passes', checkLabelerConfig(advised).length === 0);

  // (b) Split entries without the wrapper.
  const split = `'area:web':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'apps/web/**'\n      - all-globs-to-all-files:\n          - '!apps/web/src/generated/**'\n`;
  const splitProblems = checkLabelerConfig(split);
  check('split entries without all: fail', splitProblems.length === 1);
  check('the message explains the OR', (splitProblems[0] ?? '').includes('OR-ed'));

  const splitWrapped = `'area:web':\n  - all:\n      - changed-files:\n          - any-glob-to-any-file:\n              - 'apps/web/**'\n          - all-globs-to-all-files:\n              - '!apps/web/src/generated/**'\n`;
  check('split entries wrapped in all: pass', checkLabelerConfig(splitWrapped).length === 0);

  // A comment quoting a wrong spelling must not fail the file that explains it.
  const documented = `# Never write any-glob-to-any-file: ['a/**', '!b/**'].\n${plain}`;
  check('a dangerous spelling in a comment is not a violation', checkLabelerConfig(documented).length === 0);

  const both = `${flowNegation}\n${plain}`;
  check('a good label beside a bad one is not blamed', checkLabelerConfig(both).length === 1);

  check('an empty config fails rather than passing vacuously', checkLabelerConfig('# nothing\n').length === 1);

  // (c) Types.
  const docs = `'documentation':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'docs/**'\n`;
  const docsProblems = checkLabelerConfig(docs);
  check('a type label written by the labeler fails', docsProblems.length === 1);
  check('the message says it is a type', (docsProblems[0] ?? '').includes('is a TYPE label'));
  for (const type of TYPE_LABELS) {
    const block = `'${type}':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'docs/**'\n`;
    check(`the type ${type} is rejected as a labeler key`, checkLabelerConfig(block).length === 1);
  }
  check(
    'an extra is not mistaken for a type',
    checkLabelerConfig(`'dependencies':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'Cargo.lock'\n`).length === 0,
  );

  // (d) Cross-check against labels.yml.
  const known = new Set(['area:pty', 'dependencies']);
  check('a declared label passes the cross-check', checkLabelerConfig(plain, known).length === 0);
  const unknown = `'area:nope':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'x/**'\n`;
  const unknownProblems = checkLabelerConfig(unknown, known);
  check('an undeclared label fails the cross-check', unknownProblems.length === 1);
  check('the message names labels.yml', (unknownProblems[0] ?? '').includes('labels.yml'));
  check('the cross-check is skipped when no label set is given', checkLabelerConfig(unknown).length === 0);

  check('declaredLabels reads a name', declaredLabels('- name: "area:pty"\n  color: "0052cc"\n').has('area:pty'));
  check('declaredLabels ignores a commented name', !declaredLabels('# - name: "ghost"\n').has('ghost'));

  if (failures.length > 0) {
    process.stderr.write(`check-labeler-config self-test failed:\n  ${failures.join('\n  ')}\n`);
    process.exit(1);
  }
  process.stdout.write('check-labeler-config self-test: all cases held\n');
}

if (process.argv.includes('--self-test')) {
  selfTest();
} else {
  const known = declaredLabels(readFileSync(LABELS_PATH, 'utf8'));
  const problems = checkLabelerConfig(readFileSync(CONFIG_PATH, 'utf8'), known);
  if (problems.length > 0) {
    for (const problem of problems) process.stderr.write(`::error::${problem}\n`);
    process.exit(1);
  }
  process.stdout.write(
    `${CONFIG_PATH}: every negation excludes, no type labels written, all names declared in ${LABELS_PATH}\n`,
  );
}
