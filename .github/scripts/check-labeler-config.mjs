// One invariant over .github/labeler.yml: a label that combines a positive glob with a
// negated one must wrap them in `all:`.
//
// This check exists because the configuration it guards was written wrong the first time.
// actions/labeler treats a match object with no top-level key as `any`, so
//
//     'area:web':
//       - changed-files:
//           - any-glob-to-any-file: ['apps/web/**']
//           - all-globs-to-all-files: ['!apps/web/src/generated/**']
//
// reads as "in apps/web OR not in apps/web/src/generated" — and the second half is true
// of nearly every pull request in the repository, so the label lands on all of them. The
// intended meaning needs `- all:` above `changed-files`. The two spellings differ by one
// line, both are valid YAML, and the wrong one fails by over-labelling silently rather
// than by erroring, which is the kind of defect nobody notices for months.
//
// Not a YAML parser: it splits on label keys, which start at column zero in this file,
// and asks a single structural question of each block. That is all the rule needs, and a
// parser this file does not have cannot misread it.
//
// Run with `--self-test` for the fixtures, with no arguments to check the real file.

import { readFileSync } from 'node:fs';
import process from 'node:process';

const CONFIG_PATH = '.github/labeler.yml';

/** A negated glob — the half that makes `any` dangerous. */
const NEGATED = /^\s*-?\s*['"]?!/m;

/**
 * @param {string} text the contents of a labeler config
 * @returns {string[]} one message per label that needs `all:` and lacks it
 */
export function checkLabelerConfig(text) {
  const lines = text.split('\n');
  const problems = [];

  /** @type {{ label: string, body: string[] } | null} */
  let current = null;
  const blocks = [];
  for (const line of lines) {
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
    const text_ = code.join('\n');
    const hasPositive = /any-glob-to-any-file|all-globs-to-any-file/.test(text_);
    const hasNegation = NEGATED.test(text_) || /all-globs-to-all-files/.test(text_);
    const wrapped = /^\s*-\s*all:\s*$/m.test(text_);

    if (hasPositive && hasNegation && !wrapped) {
      problems.push(
        `${label}: combines a positive glob with a negated one but does not wrap them in "- all:". Without it the conditions are OR-ed (a match object with no top-level key defaults to "any"), and a negated glob is true of almost every pull request, so this label would be applied to nearly all of them.`,
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

  // The exact shape this file exists to catch — the one that shipped first.
  const bad = `'area:web':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'apps/web/**'\n      - all-globs-to-all-files:\n          - '!apps/web/src/generated/**'\n`;
  const badProblems = checkLabelerConfig(bad);
  check('an unwrapped exclusion fails', badProblems.length === 1);
  check('the message names the label', (badProblems[0] ?? '').includes('area:web'));
  check('the message explains the OR', (badProblems[0] ?? '').includes('OR-ed'));

  const good = `'area:web':\n  - all:\n      - changed-files:\n          - any-glob-to-any-file:\n              - 'apps/web/**'\n          - all-globs-to-all-files:\n              - '!apps/web/src/generated/**'\n`;
  check('the same exclusion wrapped in all: passes', checkLabelerConfig(good).length === 0);

  // A comment quoting the wrong spelling must not fail the file that explains it — the
  // same lesson comment stripping taught check-workflow-pins.mjs.
  const documented = `# Never write all-globs-to-all-files without '- all:' above it.\n${plain}`;
  check('the dangerous spelling in a comment is not a violation', checkLabelerConfig(documented).length === 0);

  const both = `${bad}\n${plain}`;
  check('a good label beside a bad one is not blamed', checkLabelerConfig(both).length === 1);

  check('an empty config fails rather than passing vacuously', checkLabelerConfig('# nothing\n').length === 1);

  if (failures.length > 0) {
    process.stderr.write(`check-labeler-config self-test failed:\n  ${failures.join('\n  ')}\n`);
    process.exit(1);
  }
  process.stdout.write('check-labeler-config self-test: all cases held\n');
}

if (process.argv.includes('--self-test')) {
  selfTest();
} else {
  const problems = checkLabelerConfig(readFileSync(CONFIG_PATH, 'utf8'));
  if (problems.length > 0) {
    for (const problem of problems) process.stderr.write(`::error::${problem}\n`);
    process.exit(1);
  }
  process.stdout.write(`${CONFIG_PATH}: every exclusion is wrapped in "all:"\n`);
}
