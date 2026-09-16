import { describe, expect, it } from 'vitest';

import { readIssues, readStarted } from './tasks';

/*
 * The parse that stands in for a ts-rs type until wave C1 lands one.
 *
 * Worth a suite of its own precisely because it is temporary. A hand-written `as Issue[]`
 * would turn a disagreement between the two halves into `undefined` in a table cell — a row
 * with no title, a status pill reading `● undefined`, an Updated column saying `unknown` —
 * which looks like a repository with strange issues in it rather than like a protocol
 * mistake. Everything below is a shape a daemon could plausibly send if C1 and this file
 * disagreed about a field name.
 */

/** One well-formed row, as `gh issue list --json` shapes it. */
const ROW = {
  number: 200,
  title: 'Add the Tasks screen',
  state: 'OPEN',
  updatedAt: '2026-09-09T12:00:00Z',
  url: 'https://github.com/noctcore/nysia/issues/200',
  author: 'Shironex',
  labels: [{ name: 'area:web', color: 'bfd4f2', description: 'The window' }],
};

describe('reading the issue list', () => {
  it('reads a well-formed row into what the table draws', () => {
    expect(readIssues([ROW])).toEqual([
      {
        number: 200,
        title: 'Add the Tasks screen',
        state: 'open',
        updatedAt: '2026-09-09T12:00:00Z',
        url: 'https://github.com/noctcore/nysia/issues/200',
        author: 'Shironex',
        labels: ['area:web'],
      },
    ]);
  });

  it('lower-cases the state gh actually sends', () => {
    // `gh` answers `OPEN` and `CLOSED`. A component comparing against `'open'` would paint
    // every issue closed and nothing would fail anywhere else.
    expect(readIssues([ROW])[0]?.state).toBe('open');
    expect(readIssues([{ ...ROW, state: 'CLOSED' }])[0]?.state).toBe('closed');
  });

  it('drops the colour gh attaches to every label', () => {
    // Keeping it would put a hex on screen that the accent picker cannot reach, which is a
    // bug by this repository's own rule rather than a preference.
    const labels = readIssues([ROW])[0]?.labels;
    expect(labels).toEqual(['area:web']);
    expect(JSON.stringify(labels)).not.toContain('bfd4f2');
  });

  it('reads labels whether the daemon flattens them or not', () => {
    // Which of the two C1 sends is C1's to decide, so both are read and neither is guessed.
    expect(readIssues([{ ...ROW, labels: ['bug', 'P1-high'] }])[0]?.labels).toEqual([
      'bug',
      'P1-high',
    ]);
  });

  it('skips a label it cannot read rather than refusing the whole list', () => {
    // A list is perfectly readable without one pill. Refusing a hundred issues over a
    // malformed label would be the tail wagging the dog.
    const labels = readIssues([{ ...ROW, labels: ['bug', 42, null, { color: 'red' }] }]);
    expect(labels[0]?.labels).toEqual(['bug']);
  });

  it('accepts an issue with no author, because GitHub really sends one', () => {
    // A deleted account leaves an issue with no author. A row that renders without a name is
    // better than a list that will not render at all.
    expect(readIssues([{ ...ROW, author: null }])[0]?.author).toBeNull();
    expect(readIssues([{ ...ROW, author: '' }])[0]?.author).toBeNull();
  });

  it('refuses an answer that is not a list at all', () => {
    // What every daemon sends today: `tasks_list` is unsupported, so the refusal arrives as
    // an error envelope rather than here — but a daemon answering with the wrong payload is
    // exactly the protocol disagreement this is for.
    expect(() => readIssues(undefined)).toThrow(/tasks_list/);
    expect(() => readIssues({ issues: [] })).toThrow(/not a list/);
  });

  it('refuses a row missing a field the table renders, and says which row', () => {
    // The position or the number, so the message points somewhere. `#0` in a stack trace is
    // the difference between a five-minute fix and an afternoon.
    expect(() => readIssues([{ ...ROW, number: undefined }])).toThrow(/position 0/);
    expect(() => readIssues([ROW, { ...ROW, number: 201, title: undefined }])).toThrow(/201/);
    expect(() => readIssues([{ ...ROW, url: undefined }])).toThrow(/title, date or URL/);
    expect(() => readIssues([null])).toThrow(/position 0/);
  });

  it('refuses a number that is not one, rather than printing NaN as an id', () => {
    expect(() => readIssues([{ ...ROW, number: '200' }])).toThrow();
    expect(() => readIssues([{ ...ROW, number: 1.5 }])).toThrow();
  });

  it('reads an empty list as an empty list, which is a real answer', () => {
    // The whole point of the screen's fourth ending: a repository with no open issues is a
    // fact, and it must reach the store as `loaded` rather than as anything else.
    expect(readIssues([])).toEqual([]);
  });
});

describe('reading what Start answered', () => {
  const STARTED = {
    branch: 'issue/200-add-the-tasks-screen',
    handle: 'sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60',
    paneKey: 'tab_9:leaf_1',
    adopted: true,
  };

  it('reads the branch, the session and which of the two happened', () => {
    expect(readStarted(STARTED)).toEqual(STARTED);
  });

  it('refuses a missing adopted flag rather than defaulting it', () => {
    // A default would make a daemon that forgot the field say "started in a new worktree"
    // about one it had adopted — which is exactly the sentence the flag exists to get right,
    // and the user has no way to tell it is wrong.
    expect(() => readStarted({ ...STARTED, adopted: undefined })).toThrow(/adopted/);
    expect(() => readStarted({ ...STARTED, adopted: 'true' })).toThrow(/adopted/);
  });

  it('refuses an answer with no worktree or no session to open a tab for', () => {
    expect(() => readStarted({ ...STARTED, branch: '' })).toThrow(/branch/);
    expect(() => readStarted({ ...STARTED, paneKey: undefined })).toThrow(/session/);
    expect(() => readStarted(undefined)).toThrow(/task_start/);
    expect(() => readStarted([STARTED])).toThrow(/task_start/);
  });
});
