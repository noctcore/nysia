// Fails a pull request that is missing a type or a priority label.
//
// CLAUDE.md section 1 asks every PR for exactly one type, exactly one priority and at
// least one area. Areas are applied automatically from the paths the PR touches
// (.github/labeler.yml), but type and priority are judgement calls no path can make,
// so they are the two this check enforces.
//
// Label names arrive on stdin, one per line — the workflow pipes `gh pr view --json
// labels` in rather than reading the event payload, which is a snapshot taken when the
// run was queued and is stale the moment someone adds the missing label.
//
// Run `node .github/scripts/require-pr-facets.mjs --self-test` to exercise the rule
// itself. CI runs that too: a check nobody has watched fail is not a check (trap 12).

import { readFileSync } from 'node:fs';
import process from 'node:process';

/** Exactly one of these. The kind of change, which a diff cannot infer. */
export const TYPE_LABELS = [
  'bug',
  'enhancement',
  'chore',
  'refactor',
  'performance',
  'security',
  'dx',
  'documentation',
  'test',
  'dependencies',
];

/** Exactly one of these. How soon it matters, which is a call, not a fact. */
export const PRIORITY_LABELS = ['P0-critical', 'P1-high', 'P2-medium', 'P3-low'];

/**
 * Check one label set.
 *
 * @param {readonly string[]} labels label names as GitHub reports them
 * @returns {{ ok: boolean, problems: string[] }} every problem, not just the first —
 * being told about the missing priority only after fixing the missing type wastes a
 * round trip through CI.
 */
export function checkFacets(labels) {
  const present = new Set(labels);
  const problems = [];

  for (const [facet, valid] of [
    ['type', TYPE_LABELS],
    ['priority', PRIORITY_LABELS],
  ]) {
    const found = valid.filter((name) => present.has(name));
    if (found.length === 0) {
      problems.push(`This pull request carries no ${facet} label. Add exactly one of: ${valid.join(', ')}.`);
    } else if (found.length > 1) {
      problems.push(
        `This pull request carries ${found.length} ${facet} labels (${found.join(', ')}). Keep exactly one of: ${valid.join(', ')}.`,
      );
    }
  }

  return { ok: problems.length === 0, problems };
}

/** The self-test. Each case names the defect it would catch. */
function selfTest() {
  const failures = [];
  /**
   * @param {string} what
   * @param {boolean} held
   */
  const check = (what, held) => {
    if (!held) failures.push(what);
  };

  const ok = checkFacets(['chore', 'P2-medium', 'area:build']);
  check('a complete label set passes', ok.ok && ok.problems.length === 0);

  const noType = checkFacets(['P1-high', 'area:pty']);
  check('a missing type fails', !noType.ok);
  check('the missing-type message says which facet', noType.problems.some((p) => p.includes('no type label')));
  check(
    'the missing-type message names every valid type',
    TYPE_LABELS.every((name) => noType.problems.some((p) => p.includes(name))),
  );
  check(
    'a missing type is not reported as a missing priority',
    !noType.problems.some((p) => p.includes('no priority label')),
  );

  const noPriority = checkFacets(['bug', 'area:vt']);
  check('a missing priority fails', !noPriority.ok);
  check(
    'the missing-priority message names every valid priority',
    PRIORITY_LABELS.every((name) => noPriority.problems.some((p) => p.includes(name))),
  );

  const neither = checkFacets(['area:web']);
  check('a PR with only area labels fails', !neither.ok);
  check('both facets are reported at once', neither.problems.length === 2);

  const none = checkFacets([]);
  check('an unlabelled PR fails', !none.ok && none.problems.length === 2);

  // CLAUDE.md says "exactly one", so two is as wrong as none — and it is the shape a
  // human produces by adding the right label and forgetting to remove the wrong one.
  const twoTypes = checkFacets(['bug', 'chore', 'P3-low']);
  check('two type labels fail', !twoTypes.ok);
  check('the message names both offenders', twoTypes.problems.some((p) => p.includes('bug, chore')));

  const twoPriorities = checkFacets(['test', 'P0-critical', 'P3-low']);
  check('two priority labels fail', !twoPriorities.ok);

  // Substring matches would make `P1-high` satisfy a search for `P1`, and `dx` appears
  // inside no other name only by luck. Names must match whole.
  const lookalikes = checkFacets(['bugfix', 'P2', 'area:git']);
  check('a label that merely resembles a type does not satisfy it', !lookalikes.ok);
  check('a lookalike leaves both facets unsatisfied', lookalikes.problems.length === 2);

  if (failures.length > 0) {
    process.stderr.write(`require-pr-facets self-test failed:\n  ${failures.join('\n  ')}\n`);
    process.exit(1);
  }
  process.stdout.write('require-pr-facets self-test: all cases held\n');
}

function main() {
  if (process.argv.includes('--self-test')) {
    selfTest();
    return;
  }

  const labels = readFileSync(0, 'utf8')
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);

  const { ok, problems } = checkFacets(labels);
  const seen = labels.length > 0 ? labels.join(', ') : '(none)';

  if (ok) {
    process.stdout.write(`Labels: ${seen}\n`);
    return;
  }

  for (const problem of problems) {
    process.stderr.write(`::error::${problem}\n`);
  }
  process.stderr.write(`Labels currently on this pull request: ${seen}\n`);
  process.stderr.write('Areas are applied automatically; type and priority are yours to choose.\n');
  process.exit(1);
}

main();
