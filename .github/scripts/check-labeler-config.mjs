// Three invariants over .github/labeler.yml.
//
// (a) A label that combines a positive glob with a negated one must wrap them in `all:`.
// (b) The labeler may never write a TYPE label — only areas and extras.
// (c) Every label it writes must exist in .github/labels.yml.
//
// (b) is the one that bites hardest. `documentation` was applied here from `docs/**`
// while pr-facets.yml counts it as a type, so any enhancement, bug or chore PR that also
// touched docs ended up with two types and failed — intermittently, because labels
// written with GITHUB_TOKEN do not trigger other workflows and the two jobs race. Types
// are editorial and belong to the author; paths can only decide areas and extras.
//
// (c) catches a label written here that nobody ever created, which the labeler reports
// as a plain API failure long after the config looked fine in review.
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

import { TYPE_LABELS } from './facets.mjs';

const CONFIG_PATH = '.github/labeler.yml';
const LABELS_PATH = '.github/labels.yml';

/**
 * Label names declared in labels.yml.
 *
 * One regex over `- name: "…"` rather than a parse, for the same reason as below: the
 * question is "which names exist", and a name that this misses shows up as a false
 * positive someone fixes in a line, never as a silent pass.
 *
 * @param {string} text
 * @returns {Set<string>}
 */
export function declaredLabels(text) {
  const names = new Set();
  for (const m of text.matchAll(/^- name: "((?:[^"\\]|\\.)*)"$/gm)) names.add(JSON.parse(`"${m[1]}"`));
  return names;
}

/** A negated glob — the half that makes `any` dangerous. */
const NEGATED = /^\s*-?\s*['"]?!/m;

/**
 * @param {string} text the contents of a labeler config
 * @param {Set<string> | null} known label names from labels.yml, or null to skip (c)
 * @returns {string[]} one message per violation
 */
export function checkLabelerConfig(text, known = null) {
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

  // (b) The deadlock. This is the exact block that shipped, and every other type in the
  // table would behave the same way, so the rule is checked against all of them.
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

  // (c) Cross-check against labels.yml.
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
    `${CONFIG_PATH}: exclusions wrapped in "all:", no type labels written, all names declared in ${LABELS_PATH}\n`,
  );
}
