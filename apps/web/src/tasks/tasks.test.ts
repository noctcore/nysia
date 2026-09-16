import { describe, expect, it } from 'vitest';

import { repositoryOf, updatedPhrase, type Issue } from './issue';
import {
  isTasksBusy,
  issuesOf,
  tasksNotice,
  unavailableReason,
  type TaskUnavailableReason,
  type TasksState,
} from './tasks';

/*
 * The four ways this screen shows nothing, and the requirement that they are four.
 *
 * `an empty list for any of those is a lie` is the contract's own phrasing, and the test it
 * turns into is the one below: every ending that draws a table with nothing in it produces a
 * heading, and no two of those headings are equal. Asserted over the reasons *derived from
 * the type* rather than a list written out here, so a fourth reason added to
 * `TaskUnavailableReason` without a heading fails this file rather than shipping as a blank
 * screen.
 */

const EVERY_REASON: readonly TaskUnavailableReason[] = [
  'gh_missing',
  'gh_unauthenticated',
  'query_failed',
];

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

  it('shows no notice while there is a table, or while one is being fetched', () => {
    expect(tasksNotice({ phase: 'loaded', issues: [AN_ISSUE] })).toBeNull();
    expect(tasksNotice({ phase: 'loading' })).toBeNull();
    expect(tasksNotice({ phase: 'idle' })).toBeNull();
  });
});

describe('reading a refusal', () => {
  it('recognises each of the three codes the contract names', () => {
    expect(unavailableReason('gh_missing')).toBe('gh_missing');
    expect(unavailableReason('gh_unauthenticated')).toBe('gh_unauthenticated');
    expect(unavailableReason('query_failed')).toBe('query_failed');
  });

  it('falls back to query_failed rather than inventing a state', () => {
    // The codes are proposed rather than generated until wave C1 lands, so this is the case
    // that decides whether a guessed spelling is safe. It degrades to a heading one notch
    // less specific, carrying the daemon's own sentence — never to a blank table.
    expect(unavailableReason('unsupported')).toBe('query_failed');
    expect(unavailableReason('something_a_newer_daemon_added')).toBe('query_failed');
    expect(unavailableReason(null)).toBe('query_failed');
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
