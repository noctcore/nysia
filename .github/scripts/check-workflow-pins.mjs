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
// WHAT THIS IS, AND WHAT IT IS NOT.
//
// This is a lint against honest mistakes, not a security boundary, and it is deliberately
// not written as one. It matches text; it does not parse YAML. An author who wants to get
// a tag past it can; `uses` reached through an anchor, an unusual quoting, or a tag
// assembled from an expression will all slip through, and so will a head checkout spelled
// as something this file has not thought of.
//
// That is an acceptable design because of who the adversary is. Workflow files only run
// with a writable token from the BASE branch — a fork's edits to .github/workflows never
// execute against this repository on its own pull request. So an evasion cannot be used
// by an outsider; it can only be merged by a maintainer who reviewed it. Against that
// threat model the useful job is catching the `@v5` somebody typed out of habit, and a
// yq-based parser would buy correctness against an adversary who is not there while
// making the check unrunnable on a developer machine that has no yq.
//
// The evasions below are handled because they are cheap, not because the list is complete:
// a value on the line after `uses:`, a flow mapping, a quoted key, and a folded scalar.
// If you need a boundary rather than a lint, the thing to add is a branch ruleset that
// requires this check, plus review on .github/**, not a better regex here.
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

/** A docker reference pinned by digest: `docker://image@sha256:<64 hex>`. */
const DOCKER_PINNED = /^docker:\/\/\S+@sha256:[0-9a-f]{64}$/;

/**
 * Any `uses` key, in the spellings ordinary YAML allows.
 *
 * Not line-anchored to `- uses:`, because `- { uses: x }`, `"uses": x` and a value on
 * the following line are all valid YAML that the anchored form waved through.
 */
const USES_KEY = /(?:^|[\s{,])["']?uses["']?[ \t]*:[ \t]*(.*)$/gm;

/** Ways a workflow can name the head of the pull request it was triggered by. */
const HEAD_REFS = [
  'pull_request.head',
  "pull_request['head']",
  'pull_request["head"]',
  'head.sha',
  'head.ref',
  'github.head_ref',
  'refs/pull/',
  'gh pr checkout',
];

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
 * Every action reference in a workflow, with the value normalised.
 *
 * Handles a value on the line after the key, a folded or literal block scalar, and the
 * trailing `}` or `,` a flow mapping leaves behind.
 *
 * @param {string} source comment-stripped workflow text
 * @returns {string[]}
 */
function usesValues(source) {
  const lines = source.split('\n');
  const values = [];

  for (const match of source.matchAll(USES_KEY)) {
    let raw = match[1].trim();

    // `uses: >-` / `uses: |` — the value is the indented block that follows. Also the
    // empty case, `uses:` alone on its line.
    if (raw === '' || raw === '>' || raw === '>-' || raw === '|' || raw === '|-') {
      const upTo = source.slice(0, match.index).split('\n').length;
      const next = lines.slice(upTo).find((l) => l.trim() !== '');
      raw = (next ?? '').trim();
    }

    // A trailing comment is where the human-readable version tag lives — `uses: x@<sha>
    // # v7.0.1` — so it has to come off before the value is judged, or every correctly
    // pinned action in the repository reads as unpinned. YAML needs whitespace before
    // the `#` for it to start a comment.
    raw = raw.replace(/\s+#.*$/, '');

    // A flow mapping closes with `}`, and entries are comma-separated.
    raw = raw
      .replace(/[},].*$/, '')
      .trim()
      .replace(/^['"]|['"]$/g, '');

    if (raw !== '') values.push(raw);
  }

  return values;
}

/**
 * @param {string} name file name, for the message
 * @param {string} text the workflow source
 * @returns {string[]} one message per violation
 */
export function checkWorkflowSource(name, text) {
  const source = stripComments(text);
  const problems = [];

  for (const uses of usesValues(source)) {
    // A local action is this repository's own code, already pinned by the commit the
    // workflow runs from.
    if (uses.startsWith('./')) continue;

    if (uses.startsWith('docker://')) {
      // Only a digest actually pins a docker reference. `docker://alpine:3` is as
      // mutable as a git tag, and exempting the whole scheme let it through while the
      // comment above claimed digest pinning.
      if (!DOCKER_PINNED.test(uses)) {
        problems.push(
          `${name}: uses ${uses}, which is a docker reference pinned by tag rather than by digest. Use docker://image@sha256:<digest>.`,
        );
      }
      continue;
    }

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

  check('a SHA-pinned action passes', checkWorkflowSource('f.yml', steps(`uses: actions/checkout@${sha} # v7.0.1`)).length === 0);

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

  // The evasions. Each of these is ordinary YAML that the line-anchored `- uses:` regex
  // waved straight through with a mutable tag.
  check(
    'a value on the line after the key is still read',
    checkWorkflowSource('f.yml', 'jobs:\n  a:\n    steps:\n      - uses:\n          actions/checkout@v7\n').length === 1,
  );
  check(
    'a flow mapping is still read',
    checkWorkflowSource('f.yml', 'jobs:\n  a:\n    steps:\n      - { uses: actions/checkout@v7 }\n').length === 1,
  );
  check(
    'a quoted key is still read',
    checkWorkflowSource('f.yml', 'jobs:\n  a:\n    steps:\n      - "uses": actions/checkout@v7\n').length === 1,
  );
  check(
    'a folded scalar is still read',
    checkWorkflowSource('f.yml', 'jobs:\n  a:\n    steps:\n      - uses: >-\n          actions/checkout@v7\n').length === 1,
  );
  check(
    'a flow mapping with a pin passes',
    checkWorkflowSource('f.yml', `jobs:\n  a:\n    steps:\n      - { uses: actions/checkout@${sha}, with: x }\n`).length === 0,
  );

  // docker:// was exempted wholesale while the comment claimed digest pinning.
  check(
    'a docker reference pinned by tag fails',
    checkWorkflowSource('f.yml', steps('uses: docker://alpine:3')).length === 1,
  );
  check(
    'a docker reference pinned by digest passes',
    checkWorkflowSource('f.yml', steps(`uses: docker://alpine@sha256:${'c'.repeat(64)}`)).length === 0,
  );

  const headCheckout = (ref) =>
    `on:\n  pull_request_target:\njobs:\n  a:\n    steps:\n      - uses: actions/checkout@${sha}\n        with:\n          ref: ${ref}\n`;
  const headProblems = checkWorkflowSource('f.yml', headCheckout('${{ github.event.pull_request.head.sha }}'));
  check('a head checkout under pull_request_target fails', headProblems.length > 0);
  check(
    'the message explains why',
    headProblems.some((p) => p.includes('pull_request_target') && p.includes('untrusted')),
  );
  // Index notation and the merge ref reach the same place by a different spelling.
  check(
    'bracket notation is caught',
    checkWorkflowSource('f.yml', headCheckout("${{ github.event.pull_request['head']['sha'] }}")).length > 0,
  );
  check(
    'the pull merge ref is caught',
    checkWorkflowSource('f.yml', headCheckout('refs/pull/${{ github.event.number }}/merge')).length > 0,
  );
  check(
    'gh pr checkout in a run step is caught',
    checkWorkflowSource(
      'f.yml',
      `on:\n  pull_request_target:\njobs:\n  a:\n    steps:\n      - run: gh pr checkout "$PR"\n`,
    ).length > 0,
  );

  const safeHead = `on:\n  pull_request:\njobs:\n  a:\n    steps:\n      - uses: actions/checkout@${sha}\n        with:\n          ref: \${{ github.event.pull_request.head.sha }}\n`;
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
