// Two supply-chain invariants over .github/workflows, and the proof that each trips.
//
// (a) Every third-party action is pinned to a full 40-character commit SHA. A tag is a
//     mutable pointer: whoever can move `v5` can run their code inside a job holding a
//     token that writes to this repository. A SHA cannot be moved.
//
// (b) No `pull_request_target` workflow checks out the pull request's head. That trigger
//     exists to give fork PRs a writable token, and checking out the head under it runs
//     a stranger's code with that token. See the invariant at the top of pr-triage.yml.
//
// Run with `--self-test` to exercise the rules against fixtures; run with no arguments
// to check this repository. CI does both, in that order — a rule nobody has watched
// fail is not a rule (trap 12).

import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';

const WORKFLOW_DIR = '.github/workflows';

/** `owner/repo@<sha>` or `owner/repo/path@<sha>`, forty lower-case hex digits. */
const PINNED = /^[\w.-]+\/[\w.-]+(?:\/[\w./-]+)?@[0-9a-f]{40}$/;

/** Every `uses:` value in a workflow, whatever its indentation. */
const USES = /^[ \t]*(?:-[ \t]*)?uses:[ \t]*(\S+)/gm;

/** Ways a workflow can name the head of the pull request it was triggered by. */
const HEAD_REFS = ['pull_request.head', 'head.sha', 'head.ref', 'github.head_ref'];

/**
 * Drop whole-line comments.
 *
 * Not cosmetic: these rules are *about* dangerous spellings, so the workflows that
 * document them quote those spellings in their header comments. Scanning the comments
 * would fail a workflow for explaining why it is safe.
 *
 * @param {string} text
 * @returns {string}
 */
function stripComments(text) {
  return text
    .split('\n')
    .map((line) => (/^[ \t]*#/.test(line) ? '' : line))
    .join('\n');
}

/**
 * @param {string} name file name, for the message
 * @param {string} text the workflow source
 * @returns {string[]} one message per violation
 */
export function checkWorkflowSource(name, text) {
  const source = stripComments(text);
  const problems = [];

  for (const match of source.matchAll(USES)) {
    const uses = match[1].replace(/^['"]|['"]$/g, '');
    // A local action is this repository's own code, already pinned by the commit the
    // workflow runs from. A docker digest is pinned by the digest.
    if (uses.startsWith('./') || uses.startsWith('docker://')) continue;
    if (!PINNED.test(uses)) {
      problems.push(
        `${name}: uses ${uses}, which is not pinned to a commit SHA. A tag can be moved under you; resolve it with "gh api repos/<owner>/<repo>/commits/<tag> --jq .sha" and keep the tag as a trailing comment.`,
      );
    }
  }

  if (source.includes('pull_request_target')) {
    for (const ref of HEAD_REFS) {
      if (source.includes(ref)) {
        problems.push(
          `${name}: runs on pull_request_target and references ${ref}. That trigger carries a writable token even for fork pull requests, so checking out the head runs untrusted code with it. Read the base, never the head.`,
        );
      }
    }
  }

  return problems;
}

/** Fixtures, each one a defect this file exists to catch. */
function selfTest() {
  const failures = [];
  /**
   * @param {string} what
   * @param {boolean} held
   */
  const check = (what, held) => {
    if (!held) failures.push(what);
  };

  const sha = 'a'.repeat(40);
  const steps = (step) => `jobs:\n  a:\n    steps:\n      - ${step}\n`;

  check('a SHA-pinned action passes', checkWorkflowSource('f.yml', steps(`uses: actions/checkout@${sha} # v7.0.0`)).length === 0);

  const taggedProblems = checkWorkflowSource('f.yml', steps('uses: actions/labeler@v5'));
  check('a tag-pinned action fails', taggedProblems.length === 1);
  // `?? ''` so that a rule which has stopped reporting anything fails by name here
  // rather than crashing on an undefined index — a gate's own diagnostics matter most
  // exactly when the gate is broken.
  check('the message names the offending uses', (taggedProblems[0] ?? '').includes('actions/labeler@v5'));

  check('a branch-pinned action fails', checkWorkflowSource('f.yml', steps('uses: some/action@main')).length === 1);

  // Long enough to look like a SHA, short enough not to be one.
  check('a 39-character hash fails', checkWorkflowSource('f.yml', steps(`uses: some/action@${'b'.repeat(39)}`)).length === 1);

  check('an upper-case hash fails', checkWorkflowSource('f.yml', steps(`uses: some/action@${'A'.repeat(40)}`)).length === 1);

  check('a local action is exempt', checkWorkflowSource('f.yml', steps('uses: ./.github/actions/setup')).length === 0);

  check('quoting does not hide a tag', checkWorkflowSource('f.yml', steps('uses: "actions/checkout@v7"')).length === 1);

  const nested = `jobs:\n  a:\n    steps:\n      - name: x\n        uses: github/codeql-action/init@${sha}\n`;
  check('a subdirectory action pins the same way', checkWorkflowSource('f.yml', nested).length === 0);

  const headRef = 'ref: ${{ github.event.pull_request.head.sha }}';
  const headCheckout = `on:\n  pull_request_target:\njobs:\n  a:\n    steps:\n      - uses: actions/checkout@${sha}\n        with:\n          ${headRef}\n`;
  const headProblems = checkWorkflowSource('f.yml', headCheckout);
  check('a head checkout under pull_request_target fails', headProblems.length > 0);
  check(
    'the message explains why',
    headProblems.some((p) => p.includes('pull_request_target') && p.includes('untrusted')),
  );

  const safeHead = `on:\n  pull_request:\njobs:\n  a:\n    steps:\n      - uses: actions/checkout@${sha}\n        with:\n          ${headRef}\n`;
  check('the same checkout under pull_request is fine', checkWorkflowSource('f.yml', safeHead).length === 0);

  const targetNoCheckout = `on:\n  pull_request_target:\njobs:\n  a:\n    steps:\n      - uses: actions/labeler@${sha}\n`;
  check(
    'pull_request_target without a head reference passes',
    checkWorkflowSource('f.yml', targetNoCheckout).length === 0,
  );

  // The case that made stripComments necessary: pr-triage.yml documents the dangerous
  // spelling in its own header. Explaining a rule must not violate it.
  const documented = `# Never write github.event.pull_request.head.sha here.\n# And never uses: actions/labeler@v5.\n${targetNoCheckout}`;
  check(
    'a dangerous spelling quoted in a comment is not a violation',
    checkWorkflowSource('f.yml', documented).length === 0,
  );

  if (failures.length > 0) {
    process.stderr.write(`check-workflow-pins self-test failed:\n  ${failures.join('\n  ')}\n`);
    process.exit(1);
  }
  process.stdout.write('check-workflow-pins self-test: all cases held\n');
}

function checkRepository() {
  const files = readdirSync(WORKFLOW_DIR).filter((f) => f.endsWith('.yml') || f.endsWith('.yaml'));
  if (files.length === 0) {
    process.stderr.write(`no workflows found in ${WORKFLOW_DIR}; the check cannot have run\n`);
    process.exit(2);
  }

  const problems = files.flatMap((file) =>
    checkWorkflowSource(`${WORKFLOW_DIR}/${file}`, readFileSync(join(WORKFLOW_DIR, file), 'utf8')),
  );

  if (problems.length > 0) {
    for (const problem of problems) process.stderr.write(`::error::${problem}\n`);
    process.exit(1);
  }
  process.stdout.write(`${files.length} workflows checked: every action pinned to a commit SHA\n`);
}

if (process.argv.includes('--self-test')) {
  selfTest();
} else {
  checkRepository();
}
