import { describe, expect, it } from 'vitest';

import { readIssues, readStarted } from './tasks';

/*
 * The parse that stands between a generated type and the daemon that actually answered.
 *
 * Worth a suite of its own now that the types are generated, not in spite of it. `tsc` checks
 * that this build agrees with the `nysia-proto` it was generated from; it cannot check the
 * daemon on the other end of the socket, which under D-1 outlives the window and can be a
 * version behind it. A cast would turn that disagreement into `undefined` in a table cell — a
 * row with no title, a status pill reading `● undefined`, an Updated column saying `unknown` —
 * which looks like a repository with strange issues in it rather than like a protocol mistake.
 *
 * Everything below is a shape a daemon could plausibly send if its converter and this file
 * disagreed. Several of them are `gh`'s own spellings, and that is the point: those are what
 * `crates/nysia-core/src/rpc/tasks.rs` converts *away* from, so meeting one here means the
 * conversion is not happening.
 */

/** One well-formed row, as the daemon spells it — not as `gh` does. */
const ROW = {
  number: 200,
  title: 'Add the Tasks screen',
  state: 'open',
  updatedAt: '2026-09-09T12:00:00Z',
  url: 'https://github.com/noctcore/nysia/issues/200',
  author: 'Shironex',
  labels: ['area:web'],
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

  it('reads both states, because both are drawn', () => {
    expect(readIssues([ROW])[0]?.state).toBe('open');
    expect(readIssues([{ ...ROW, state: 'closed' }])[0]?.state).toBe('closed');
  });

  it('refuses gh’s own spelling of the state instead of folding the case', () => {
    // The reverse of what this file used to assert, and the reason is that the decision moved.
    // `gh` answers `OPEN`; the daemon turns that into an enum that serialises lowercase, so
    // `OPEN` arriving here is not a spelling to be tolerant of — it is the converter not
    // running. Lowercasing would let that through silently, which is the one outcome every
    // other assertion in this file exists to prevent.
    expect(() => readIssues([{ ...ROW, state: 'OPEN' }])).toThrow(/open nor closed/);
    expect(() => readIssues([{ ...ROW, state: 'CLOSED' }])).toThrow(/open nor closed/);
  });

  it('refuses a label object, because flattening one has exactly one home', () => {
    // `gh` sends `{name, description, color}` per label and the daemon keeps the name. Reading
    // the object here as well would be a second implementation of that conversion — and the
    // colour is the half that matters: a wire hex on screen is a pixel the theme switcher
    // cannot reach, which is a bug by this repository's own rule rather than a preference.
    expect(() =>
      readIssues([{ ...ROW, labels: [{ name: 'area:web', color: 'bfd4f2' }] }]),
    ).toThrow(/label/);
  });

  it('refuses a label it cannot name rather than dropping it', () => {
    // The reverse of what this file used to assert, and the reason is which case actually
    // happens. Skipping is defensible for one bad label among good ones; the shape a daemon
    // really sends is keyed the other way *for every row at once*, and skipping turned that
    // into a table saying the repository does not label its work.
    expect(() => readIssues([{ ...ROW, labels: ['bug', { color: 'red' }] }])).toThrow(/label/);
    expect(() => readIssues([{ ...ROW, labels: [{ title: 'area:web' }] }])).toThrow(/label/);
    expect(() => readIssues([{ ...ROW, labels: [42] }])).toThrow(/label/);
    // An empty name is a pill with nothing in it. The daemon drops these, so one arriving is
    // the same signal as an object arriving.
    expect(() => readIssues([{ ...ROW, labels: [''] }])).toThrow(/label/);
  });

  it('refuses labels that are not a list, rather than reading them as none', () => {
    // Every one of these produced `[]` — the same answer as an issue with nothing on it.
    expect(() => readIssues([{ ...ROW, labels: { nodes: [{ name: 'bug' }] } }])).toThrow(
      /not a list/,
    );
    expect(() => readIssues([{ ...ROW, labels: 'bug,P1-high' }])).toThrow(/not a list/);
    expect(() => readIssues([{ ...ROW, labels: undefined }])).toThrow(/not a list/);
  });

  it('accepts an issue with no author, because GitHub really sends one', () => {
    // A deleted account leaves an issue with no author. A row that renders without a name is
    // better than a list that will not render at all. `null` is how the wire spells it.
    expect(readIssues([{ ...ROW, author: null }])[0]?.author).toBeNull();
  });

  it('refuses the two other ways of having no author, which are not the wire’s way', () => {
    // Both used to read as `null`, and both now say the same thing a label object says. The
    // daemon turns gh's empty login into `null` rather than `""`, and `author` has no
    // `skip_serializing_if`, so the field is always present — an empty string is a converter
    // that stopped converting and a missing field is a daemon that does not send it at all.
    // Neither is an issue nobody owns, and rendering them as one hides which.
    expect(() => readIssues([{ ...ROW, author: '' }])).toThrow(/author/);
    expect(() => readIssues([{ ...ROW, author: undefined }])).toThrow(/author/);
  });

  it('refuses gh’s author object rather than reporting no author', () => {
    // Measured, not hypothetical: `gh issue list --json author` answers with this object, and
    // reading a non-string as "no author" reported every row in the table as authorless while
    // looking perfectly well.
    expect(() =>
      readIssues([{ ...ROW, author: { id: 'U_1', is_bot: false, login: 'Shironex', name: '' } }]),
    ).toThrow(/author/);
    expect(() => readIssues([{ ...ROW, author: 42 }])).toThrow(/author/);
  });

  it('refuses a state it does not recognise instead of calling it closed', () => {
    // The worst of the three. `TaskTable` paints the pill unconditionally, so reading
    // everything-but-open as closed put an accent `Closed` on *every* row, directly under a
    // filter bar reading `is:issue is:open` — and nothing anywhere said so.
    expect(() => readIssues([{ ...ROW, state: 'merged' }])).toThrow(/open nor closed/);
    expect(() => readIssues([{ ...ROW, state: 1 }])).toThrow(/open nor closed/);
    expect(() => readIssues([{ ...ROW, state: { name: 'open' } }])).toThrow(/open nor closed/);
    // Misnamed rather than missing, which is the likeliest disagreement of the lot: the
    // field is there, under a key this module does not read.
    const misnamed: Record<string, unknown> = { ...ROW, status: 'open' };
    delete misnamed.state;
    expect(() => readIssues([misnamed])).toThrow(/open nor closed/);
  });

  it('refuses an answer that is not a list at all', () => {
    // A daemon older than v0.3 wave C1 refuses the verb outright, so its answer arrives as an
    // error envelope rather than here — but one answering with the wrong payload is exactly
    // the protocol disagreement this is for.
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
    // GitHub numbers issues from 1. A negative one reached `tasks/branchName.ts` and came
    // out as `issue/-5-…` — a branch whose name starts with a flag.
    expect(() => readIssues([{ ...ROW, number: -5 }])).toThrow();
    expect(() => readIssues([{ ...ROW, number: 0 }])).toThrow();
  });

  it('refuses two rows sharing one issue number', () => {
    // Load-bearing rather than tidy. The whole argument that a derived branch name is unique
    // is that an issue number is unique within a repository, so two of them would be two
    // `Start →` buttons asking for one worktree — and two table rows under one React key.
    // GitHub cannot produce this, which is exactly why meeting it means the answer is not
    // what this module thinks it is.
    expect(() => readIssues([ROW, { ...ROW, title: 'a different title' }])).toThrow(
      /sharing one number/,
    );
  });

  it('collapses a repeated label instead of refusing the list over one', () => {
    // Unlike a repeated number, a repeated label says nothing about whether the rest of the
    // answer can be trusted — so it costs one pill, not a hundred issues.
    expect(readIssues([{ ...ROW, labels: ['bug', 'bug'] }])[0]?.labels).toEqual(['bug']);
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
    adopted: true,
    handle: 'sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60',
    paneKey: 'tab_9:leaf_1',
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
    expect(() => readStarted(undefined)).toThrow(/project_start/);
    expect(() => readStarted([STARTED])).toThrow(/project_start/);
  });
});
