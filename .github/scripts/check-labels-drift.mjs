// Fails when .github/labels.yml and the repository's real labels have drifted apart.
//
// Takes two JSON files, both arrays of {name, color, description}: the config, and what
// `gh label list --json name,description,color` reports. The workflow converts the YAML
// with `yq`, so the parse is a real one — an earlier version of labels.yml claimed a
// drift check was impossible "because there is no YAML parser in the dependency set",
// which was not true: yq is on the runner, and this file is the correction.
//
// It reports in both directions. A label in the file but not in the repository has never
// been created; a label in the repository but not in the file was made in the UI and the
// file no longer describes reality. Colour and description differences are named field
// by field, because "labels differ" is not something anyone can act on.
//
// Run with `--self-test` for the fixtures. Run with two paths to compare for real.

import { readFileSync } from 'node:fs';
import process from 'node:process';

/**
 * @typedef {{ name: string, color: string, description: string | null }} Label
 */

/**
 * @param {readonly Label[]} declared from .github/labels.yml
 * @param {readonly Label[]} live from the GitHub API
 * @returns {string[]} one message per difference
 */
export function diffLabels(declared, live) {
  const problems = [];
  const byName = (list) => new Map(list.map((l) => [l.name, l]));
  const a = byName(declared);
  const b = byName(live);

  // `?? ''` on description: the API returns null for a label with none, while yq gives
  // an empty string. Treating those as different would fail on a difference nobody made.
  const text = (value) => value ?? '';

  for (const [name, want] of a) {
    const got = b.get(name);
    if (got === undefined) {
      problems.push(
        `${name}: declared in .github/labels.yml but does not exist in the repository. Create it: gh label create "${name}" --color ${want.color} --description "${text(want.description)}" --force`,
      );
      continue;
    }
    if (want.color.toLowerCase() !== got.color.toLowerCase()) {
      problems.push(`${name}: colour is ${got.color} in the repository, ${want.color} in .github/labels.yml.`);
    }
    if (text(want.description) !== text(got.description)) {
      problems.push(
        `${name}: description is "${text(got.description)}" in the repository, "${text(want.description)}" in .github/labels.yml.`,
      );
    }
  }

  for (const name of b.keys()) {
    if (!a.has(name)) {
      problems.push(
        `${name}: exists in the repository but is not declared in .github/labels.yml. Re-capture the file: gh label list --repo <owner/repo> --limit 200 --json name,description,color`,
      );
    }
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

  const base = [
    { name: 'bug', color: 'd73a4a', description: "Something isn't working" },
    { name: 'gate', color: 'fef2c0', description: 'CI gates' },
  ];
  const copy = () => JSON.parse(JSON.stringify(base));

  check('an identical set passes', diffLabels(base, copy()).length === 0);

  const missing = copy().slice(0, 1);
  const missingProblems = diffLabels(base, missing);
  check('a label that does not exist in the repository fails', missingProblems.length === 1);
  check('the message says how to create it', (missingProblems[0] ?? '').includes('gh label create'));

  const extra = [...copy(), { name: 'ghost', color: 'ffffff', description: 'made in the UI' }];
  const extraProblems = diffLabels(base, extra);
  check('a label only in the repository fails', extraProblems.length === 1);
  check('the message says to re-capture', (extraProblems[0] ?? '').includes('Re-capture'));

  const recoloured = copy();
  recoloured[0].color = '000000';
  const colourProblems = diffLabels(base, recoloured);
  check('a changed colour fails', colourProblems.length === 1);
  check('the message names both colours', (colourProblems[0] ?? '').includes('000000') && (colourProblems[0] ?? '').includes('d73a4a'));

  const upper = copy();
  upper[0].color = 'D73A4A';
  check('colour comparison ignores case', diffLabels(base, upper).length === 0);

  const redescribed = copy();
  redescribed[1].description = 'something else';
  check('a changed description fails', diffLabels(base, redescribed).length === 1);

  // yq gives "" for an empty description, the API gives null. That is not drift.
  const nulls = [{ name: 'x', color: 'ffffff', description: null }];
  const empties = [{ name: 'x', color: 'ffffff', description: '' }];
  check('a null description equals an empty one', diffLabels(empties, nulls).length === 0);

  check('several differences are all reported', diffLabels(base, [{ name: 'ghost', color: 'ffffff', description: '' }]).length === 3);

  if (failures.length > 0) {
    process.stderr.write(`check-labels-drift self-test failed:\n  ${failures.join('\n  ')}\n`);
    process.exit(1);
  }
  process.stdout.write('check-labels-drift self-test: all cases held\n');
}

if (process.argv.includes('--self-test')) {
  selfTest();
} else {
  const [declaredPath, livePath] = process.argv.slice(2);
  if (declaredPath === undefined || livePath === undefined) {
    process.stderr.write('usage: node .github/scripts/check-labels-drift.mjs <declared.json> <live.json>\n');
    process.exit(2);
  }

  const declared = JSON.parse(readFileSync(declaredPath, 'utf8'));
  const live = JSON.parse(readFileSync(livePath, 'utf8'));

  // Both sides non-empty, or the comparison did not happen: an empty file or a failed
  // API call would otherwise read as "no drift".
  if (!Array.isArray(declared) || declared.length === 0) {
    process.stderr.write(`::error::${declaredPath} parsed to no labels; the drift check cannot have run.\n`);
    process.exit(2);
  }
  if (!Array.isArray(live) || live.length === 0) {
    process.stderr.write(`::error::${livePath} parsed to no labels; the drift check cannot have run.\n`);
    process.exit(2);
  }

  const problems = diffLabels(declared, live);
  if (problems.length > 0) {
    for (const problem of problems) process.stderr.write(`::error::${problem}\n`);
    process.stderr.write(
      'Either re-apply .github/labels.yml to the repository, or re-capture it — its header documents both.\n',
    );
    process.exit(1);
  }
  process.stdout.write(`${declared.length} labels: .github/labels.yml matches the repository\n`);
}
