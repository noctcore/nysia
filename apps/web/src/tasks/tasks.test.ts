import { describe, expect, it } from 'vitest';

import { repositoryOf, updatedPhrase, type Issue } from './issue';
import {
  isStarting,
  isTasksBusy,
  issuesOf,
  startedPhrase,
  tasksNotice,
  unavailableReason,
  type TaskStartState,
  type TasksNotice,
  type TaskUnavailableReason,
  type TasksState,
} from './tasks';

/*
 * The four ways this screen shows nothing, and the requirement that they are four.
 *
 * `an empty list for any of those is a lie` is the contract's own phrasing, and the test it
 * turns into is the one below: every ending that draws a table with nothing in it produces a
 * heading, and no two of those headings are equal.
 */

/**
 * Every reason, in a shape the type will not let grow past.
 *
 * **A `Record` keyed by the union, not an array annotated with it**, and the difference is
 * the whole of what this holds. `readonly TaskUnavailableReason[]` accepts a list of three
 * when the union has four — a subset is a perfectly good array — so the roster this file
 * used to carry typechecked, iterated three, passed, and said in a comment that it was
 * derived from the type. It was not. The check that really held that property was `tsc` on
 * `Record<TaskUnavailableReason, TasksNotice>` in `tasks.ts`, a different gate in a
 * different file; here a fourth reason went unnoticed, and a fourth reason *repeating* an
 * existing heading went unnoticed everywhere.
 *
 * A `Record` has to name every member, so adding one to the union fails to compile on this
 * object — and because {@link Object.values} then hands the tests a list that includes it,
 * the duplicate-heading assertion below covers it too. Same shape as `UNAVAILABLE_NOTICES`
 * itself, for the same reason.
 */
const REASON_ROSTER: Readonly<Record<TaskUnavailableReason, TaskUnavailableReason>> = {
  gh_missing: 'gh_missing',
  gh_unauthenticated: 'gh_unauthenticated',
  query_failed: 'query_failed',
};

const EVERY_REASON: readonly TaskUnavailableReason[] = Object.values(REASON_ROSTER);

const A_NOTICE: TasksNotice = { heading: 'Something went wrong', tone: 'failed' };

/*
 * The two rosters below **must not compile**, and that is the assertion.
 *
 * `TaskUnavailableReason` is `Extract<ErrorCode, …>` now, which reads as though it ties the
 * three reasons to the wire. It only does so if the `Extract` really narrows — and `ErrorCode`
 * carries an open `(string & {})` tail, so an `Extract` written against the wrong target
 * resolves to the whole union instead, which is `string` for every practical purpose.
 *
 * **A widened union passes every check this file otherwise makes.** `Record<string, TasksNotice>`
 * accepts a roster of two, of three, of four, and of four under names `nysia-proto` has never
 * heard of; `REASON_ROSTER` above would still compile, `Object.values` would still hand back
 * three, and the duplicate-heading test would still pass. Nothing would notice until a rename
 * in proto quietly dropped a heading and the screen started saying "the issue list could not
 * be fetched" about a missing `gh`.
 *
 * `@ts-expect-error` is what turns that into a gate, and it is a gate in both directions: it
 * fails the build when the error it names *stops* happening. So if the union ever widens, both
 * directives below go unused and `pnpm typecheck` fails on the directives themselves.
 */

// @ts-expect-error a roster that forgets a reason must not compile. If this is ever accepted,
// `TaskUnavailableReason` has stopped being three literals and the notices are no longer tied
// to `ErrorCode` at all.
const MISSING_A_REASON: Readonly<Record<TaskUnavailableReason, TasksNotice>> = {
  gh_missing: A_NOTICE,
  gh_unauthenticated: A_NOTICE,
};

const CARRIES_A_STRANGER: Readonly<Record<TaskUnavailableReason, TasksNotice>> = {
  gh_missing: A_NOTICE,
  gh_unauthenticated: A_NOTICE,
  query_failed: A_NOTICE,
  // @ts-expect-error a code `nysia-proto` does not spell is not a reason this screen has a
  // heading for. Narrowing is what reports it; a widened union would take it in silence, and
  // so would the object literal a `Map` was chosen over.
  gh_rate_limited: A_NOTICE,
};

function unavailable(reason: TaskUnavailableReason): TasksState {
  return { phase: 'unavailable', reason, message: 'the daemon said so', nextSteps: ['do this'] };
}

const AN_ISSUE: Issue = {
  number: 200,
  title: 'Add the Tasks screen',
  state: 'open',
  updatedAt: '2026-09-09T12:00:00Z',
  url: 'https://github.com/noctcore/nysia/issues/200',
  author: 'Shironex',
  labels: ['area:web'],
};

describe('the empty screens', () => {
  it('gives every ending that shows no table a heading of its own', () => {
    const headings = [
      tasksNotice({ phase: 'loaded', issues: [] })?.heading,
      ...EVERY_REASON.map((reason) => tasksNotice(unavailable(reason))?.heading),
    ];
    for (const heading of headings) {
      expect(heading, headings.join(' | ')).toBeTruthy();
    }
    expect(new Set(headings).size, headings.join(' | ')).toBe(headings.length);
  });

  it('does not tell a user with no issues that something went wrong', () => {
    // The one the contract names explicitly. A repository with nothing open is a fact, and a
    // tone of `failed` on it is how a working screen reads as a broken one.
    expect(tasksNotice({ phase: 'loaded', issues: [] })).toEqual({
      heading: 'No open issues',
      tone: 'empty',
    });
  });

  it('calls both gh states setup rather than failure', () => {
    // Neither is anything anybody did wrong — they are the two steps between a fresh machine
    // and a working screen.
    expect(tasksNotice(unavailable('gh_missing'))?.tone).toBe('setup');
    expect(tasksNotice(unavailable('gh_unauthenticated'))?.tone).toBe('setup');
    expect(tasksNotice(unavailable('query_failed'))?.tone).toBe('failed');
  });

  it('shows no notice when there is a table to show', () => {
    expect(tasksNotice({ phase: 'loaded', issues: [AN_ISSUE] })).toBeNull();
  });

  it('says a query is running rather than leaving the screen blank', () => {
    // Not one of the four, and not a lie either — but returning `null` here left a bare
    // header over empty space for the length of a round trip, which is the one state on this
    // screen where a user genuinely cannot tell whether anything is happening.
    expect(tasksNotice({ phase: 'loading' })?.heading).toBe('Asking GitHub…');
    expect(tasksNotice({ phase: 'idle' })?.heading).toBe('Asking GitHub…');
    // Toned `empty`: a query in flight is not an event and not a fault.
    expect(tasksNotice({ phase: 'loading' })?.tone).toBe('empty');
  });
});

describe('reading a refusal', () => {
  it('recognises each of the three codes the contract names', () => {
    expect(unavailableReason('gh_missing')).toBe('gh_missing');
    expect(unavailableReason('gh_unauthenticated')).toBe('gh_unauthenticated');
    expect(unavailableReason('query_failed')).toBe('query_failed');
  });

  it('falls back to query_failed rather than inventing a state', () => {
    // `ErrorCode` is open on purpose — an error that cannot be parsed is the worst possible
    // place to be strict — so a code from a newer daemon is a real possibility. It degrades to
    // a heading one notch less specific, carrying the daemon's own sentence, never to a blank
    // table. `unsupported` is what a daemon older than wave C1 answers, having no such verb.
    expect(unavailableReason('unsupported')).toBe('query_failed');
    expect(unavailableReason('something_a_newer_daemon_added')).toBe('query_failed');
    expect(unavailableReason(null)).toBe('query_failed');
  });

  it('will not let the roster drift from the wire without failing a build', () => {
    // The assertion is `tsc`, not these two expectations: the rosters above carry
    // `@ts-expect-error` directives that fail the typecheck if the errors they name stop
    // happening. These lines exist so the declarations are used — `noUnusedLocals` is on — and
    // so the shapes being described are visible from the test that names them.
    expect(Object.keys(MISSING_A_REASON)).toHaveLength(2);
    expect(Object.keys(CARRIES_A_STRANGER)).toHaveLength(4);
  });

  it('answers a miss for a name that lives on Object.prototype', () => {
    // A `Map` and not an object literal. With a literal, `constructor` and `toString` are
    // members and a daemon sending either would have been read as a reason.
    expect(unavailableReason('constructor')).toBe('query_failed');
    expect(unavailableReason('toString')).toBe('query_failed');
    expect(unavailableReason('__proto__')).toBe('query_failed');
  });
});

describe('the state', () => {
  it('holds the refresh shut only while one is in flight', () => {
    expect(isTasksBusy({ phase: 'loading' })).toBe(true);
    expect(isTasksBusy({ phase: 'idle' })).toBe(false);
    expect(isTasksBusy({ phase: 'loaded', issues: [] })).toBe(false);
    expect(isTasksBusy(unavailable('query_failed'))).toBe(false);
  });

  it('offers rows only from a loaded list', () => {
    expect(issuesOf({ phase: 'loaded', issues: [AN_ISSUE] })).toEqual([AN_ISSUE]);
    expect(issuesOf({ phase: 'loading' })).toEqual([]);
    expect(issuesOf(unavailable('gh_missing'))).toEqual([]);
  });
});

describe('what Start → says it did', () => {
  const started = (adopted: boolean): TaskStartState => ({
    phase: 'started',
    issue: 200,
    branch: 'issue/200-add-the-tasks-screen',
    paneKey: 'tab_9:leaf_1',
    adopted,
  });

  it('says adopting a worktree and making one differently', () => {
    // `adopted` is not decoration, and this is the whole of what it buys. `ProjectStarted`'s
    // own comment asks the window to say *"opened the worktree you already had"* rather than
    // implying it made one — because "created a worktree" and "moved into the one that was
    // there, with whatever is in it" are different enough to act on. A flag carried across the
    // wire, parsed, refused when missing, and then rendered into one sentence either way would
    // be four steps in aid of nothing.
    const adopted = startedPhrase(started(true));
    const created = startedPhrase(started(false));
    expect(adopted).toBeTruthy();
    expect(created).toBeTruthy();
    expect(adopted).not.toBe(created);
    expect(adopted).toContain('already');
    expect(created).toContain('new worktree');
  });

  it('names the branch, because that is what the worktree is keyed by', () => {
    // D-6. The issue number is in the line as a label for the row somebody pressed; the branch
    // is the thing that outlives the query.
    expect(startedPhrase(started(false))).toContain('issue/200-add-the-tasks-screen');
  });

  it('has nothing to confirm until something has started', () => {
    expect(startedPhrase({ phase: 'idle' })).toBeNull();
    expect(startedPhrase({ phase: 'starting', issue: 200 })).toBeNull();
  });

  it('busies only the row that was pressed', () => {
    expect(isStarting({ phase: 'starting', issue: 200 }, 200)).toBe(true);
    expect(isStarting({ phase: 'starting', issue: 200 }, 199)).toBe(false);
    expect(isStarting({ phase: 'idle' }, 200)).toBe(false);
    expect(isStarting(started(true), 200)).toBe(false);
  });
});

describe('the owner and repository, which gh does not send', () => {
  it('comes off the issue URL', () => {
    expect(repositoryOf('https://github.com/noctcore/nysia/issues/200')).toEqual({
      owner: 'noctcore',
      name: 'nysia',
    });
  });

  it('works on an enterprise host, because the path shape is the same', () => {
    expect(repositoryOf('https://ghe.example.com/acme/tools/issues/7')).toEqual({
      owner: 'acme',
      name: 'tools',
    });
  });

  it('is null rather than a guess when the URL is not an issue URL', () => {
    // A wrong owner is worse than a missing one: it puts a person's name on somebody else's
    // issue. The row renders without the sub-line instead.
    expect(repositoryOf('https://github.com/noctcore/nysia/pull/91')).toBeNull();
    expect(repositoryOf('https://github.com/noctcore')).toBeNull();
    expect(repositoryOf('not a url at all')).toBeNull();
    expect(repositoryOf('')).toBeNull();
  });
});

describe('the Updated column', () => {
  const NOW = Date.parse('2026-09-16T12:00:00Z');

  it('writes the age out in words, not in the sidebar’s one-letter form', () => {
    expect(updatedPhrase(NOW, '2026-09-09T12:00:00Z')).toBe('7 days ago');
    expect(updatedPhrase(NOW, '2026-09-16T09:00:00Z')).toBe('3 hours ago');
    expect(updatedPhrase(NOW, '2026-09-16T11:40:00Z')).toBe('20 minutes ago');
    expect(updatedPhrase(NOW, '2026-09-16T11:59:30Z')).toBe('just now');
  });

  it('says one day, not 1 days', () => {
    expect(updatedPhrase(NOW, '2026-09-15T12:00:00Z')).toBe('1 day ago');
    expect(updatedPhrase(NOW, '2026-09-16T11:00:00Z')).toBe('1 hour ago');
  });

  it('reads a clock behind the server as just now, never as the future', () => {
    expect(updatedPhrase(NOW, '2026-09-16T12:05:00Z')).toBe('just now');
  });

  it('says unknown rather than printing a confident lie', () => {
    // The failure this catches falls through to an epoch of zero and prints `56 years ago`,
    // which looks like data rather than like a parse that did not happen.
    expect(updatedPhrase(NOW, 'whenever')).toBe('unknown');
    expect(updatedPhrase(NOW, '')).toBe('unknown');
  });
});
