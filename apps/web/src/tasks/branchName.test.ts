import { describe, expect, it } from 'vitest';

import { branchForIssue, slugify } from './branchName';

/*
 * What a branch name has to be, tested as properties rather than as a table of examples.
 *
 * An example-per-title suite would pass with a slugifier that special-cased exactly those
 * titles, and the thing that matters here is the claim the module makes about *every* title:
 * that nothing a person can type into a GitHub issue can come out the other side as an
 * illegal ref. So the awkward titles are run through the same two assertions the ordinary
 * ones are.
 */

/**
 * The sequences `git check-ref-format` refuses, as a list this suite can sweep for.
 *
 * Stated here rather than trusted to the whitelist, which is the point: the module's claim
 * is that a whitelist satisfies these without enumerating them, and a claim like that is
 * worth checking from the other side. If the whitelist is ever widened — to keep dots, say,
 * because somebody wants `v1.2` in a branch — this is what notices.
 */
const FORBIDDEN = [
  '..',
  '@{',
  '\\',
  '~',
  '^',
  ':',
  '?',
  '*',
  '[',
  ' ',
  '\t',
  '\n',
  '//',
];

/** Titles chosen because each one aims at a different rule. */
const HOSTILE_TITLES: readonly string[] = [
  'fix: the thing..that broke',
  'refs/heads/@{upstream} is not a title',
  'C:\\Users\\kacpe\\Projekty — a path in a title',
  'tilde~caret^colon:question?star*bracket[',
  '  leading and trailing whitespace  ',
  '---all separators---',
  '.hidden',
  'ends with a dot.',
  'my-work.lock',
  '@',
  '--upload-pack=calc',
  'ゲームの起動に失敗する',
  'emoji 🎉 in the title',
  '',
  '////',
  'a'.repeat(400),
];

describe('a branch derived from an issue', () => {
  it('is issue/<number>-<slug>, so the number is what keeps it unique', () => {
    expect(branchForIssue({ number: 200, title: 'Add the Tasks screen' })).toBe(
      'issue/200-add-the-tasks-screen',
    );
  });

  it('is the same branch every time, which is what lets a second Start adopt', () => {
    // The adoption path in the wave-C contract rests entirely on this: `Start →` on an
    // untouched issue has to ask for the branch the first one created, or the daemon makes a
    // second worktree instead of adopting the first.
    const issue = { number: 200, title: 'Add the Tasks screen' };
    expect(branchForIssue(issue)).toBe(branchForIssue({ ...issue }));
  });

  it('never collides between two issues, however alike their titles', () => {
    // The property, not an example. Two issues cannot share a number within a repository, so
    // the number being in the name is the whole of the uniqueness argument — including for
    // the case a counter would otherwise be invented for, which is identical titles.
    const titles = ['Add the Tasks screen', 'Add the Tasks screen', '', '...', 'x'];
    const branches = titles.map((title, index) => branchForIssue({ number: index + 1, title }));
    expect(new Set(branches).size).toBe(branches.length);
  });

  it('keeps the number readable rather than truncating it away', () => {
    // The cap applies to the slug alone. A title long enough to reach it must not be able to
    // push the number out of the name, because the number is the part that has to stay
    // unique.
    const branch = branchForIssue({ number: 4021, title: 'a'.repeat(400) });
    expect(branch.startsWith('issue/4021-')).toBe(true);
    expect(branch.length).toBeLessThan(80);
  });

  it('falls back to the number alone when a title slugs to nothing', () => {
    // Not a placeholder and not a failure: `issue/200` is unique, legal, and says which
    // issue it came from, which is everything the name is for.
    expect(branchForIssue({ number: 200, title: 'ゲームの起動に失敗する' })).toBe('issue/200');
    expect(branchForIssue({ number: 7, title: '' })).toBe('issue/7');
    expect(branchForIssue({ number: 7, title: '////' })).toBe('issue/7');
  });

  it('carries no sequence git check-ref-format refuses, for any title', () => {
    for (const title of HOSTILE_TITLES) {
      const branch = branchForIssue({ number: 1, title });
      for (const sequence of FORBIDDEN) {
        expect(branch, `${JSON.stringify(title)} produced ${branch}`).not.toContain(sequence);
      }
      // The rules that are about position rather than content.
      expect(branch.endsWith('/'), branch).toBe(false);
      expect(branch.endsWith('.'), branch).toBe(false);
      expect(branch.endsWith('.lock'), branch).toBe(false);
      expect(branch, branch).not.toMatch(/\/\./);
      // A ref may legally begin with `-`; a branch that does is one every command-line tool
      // reads as an option.
      expect(branch.startsWith('-'), branch).toBe(false);
    }
  });

  it('is one path component deep, so no branch becomes another branch directory', () => {
    // `issue/200` and `issue/200/x` cannot both exist — git stores a branch as a file, so a
    // ref that is a prefix directory of another is a D/F conflict and the second one fails to
    // create. A slug that kept `/` would make that reachable from an issue title.
    for (const title of HOSTILE_TITLES) {
      expect(branchForIssue({ number: 1, title }).match(/\//g)).toHaveLength(1);
    }
  });
});

describe('the slug', () => {
  it('keeps only lowercase ascii and digits, joined by single separators', () => {
    expect(slugify('Fix THE Thing (again)')).toBe('fix-the-thing-again');
    expect(slugify('v1.2 — release notes')).toBe('v1-2-release-notes');
  });

  it('is empty rather than a placeholder when nothing survives', () => {
    expect(slugify('—')).toBe('');
    expect(slugify('🎉')).toBe('');
    expect(slugify('   ')).toBe('');
  });

  it('trims the separator after truncating, not only before', () => {
    // The ordering bug this catches ships silently: collapse, cut at 48, and a title whose
    // 49th character was the first of a word leaves a trailing `-`. The branch is still
    // legal, so nothing else here would have failed — it just looks like a mistake.
    const cut = slugify(`${'a'.repeat(47)} word`);
    expect(cut.endsWith('-')).toBe(false);
    expect(cut).toBe('a'.repeat(47));
  });
});
