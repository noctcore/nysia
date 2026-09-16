import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { MockStore } from '../store/mock/MockStore';
import { createSeedIssues, createSeedSnapshot } from '../store/mock/seed';
import { StoreProvider } from '../store/StoreProvider';
import type { StoreSnapshot, TaskStartState, TasksState } from '../store/types';
import type { TaskUnavailableReason } from './tasks';
import { TasksScreen } from './TasksScreen';

/*
 * The Tasks screen, rendered — still node-only (D-18).
 *
 * `renderToStaticMarkup` needs no DOM and runs no effects, which is enough for what is
 * claimed here: that the design's table reaches the markup, that the four empty screens are
 * four different screens, and that a control with nothing behind it says so. What it cannot
 * see is anything needing a browser — hover, focus, the effect that fires the first query —
 * and none of those is what this file is for.
 *
 * Written with `createElement` rather than JSX so the file stays `.test.ts` and inside the
 * `src/**\/*.test.ts` glob the shared vitest config defines.
 */

function render(snapshot: StoreSnapshot): string {
  return renderToStaticMarkup(
    createElement(StoreProvider, {
      store: new MockStore(snapshot),
      children: createElement(TasksScreen),
    }),
  );
}

/** The seeded snapshot with the task fields replaced, which is the only axis here. */
function showing(tasks: TasksState, taskStart: TaskStartState = { phase: 'idle' }): string {
  return render({ ...createSeedSnapshot(), nav: 'tasks', tasks, taskStart });
}

const ISSUES = createSeedIssues();

function unavailable(
  reason: TaskUnavailableReason,
  message: string,
  nextSteps: readonly string[],
): TasksState {
  return { phase: 'unavailable', reason, message, nextSteps };
}

describe('the table', () => {
  const loaded = showing({ phase: 'loaded', issues: ISSUES });

  it('lays out the five columns design-spec.md §4 specifies', () => {
    // The header and the body have to share them, so the count is asserted rather than the
    // presence: a table whose header drifted from its rows is the failure this catches, and
    // one occurrence would satisfy a `toContain`.
    const tracks = loaded.match(/grid-cols-\[80px_1fr_90px_110px_120px\]/g);
    expect(tracks?.length).toBe(ISSUES.length + 1);
  });

  it('names four of the five columns and leaves the actions column unnamed', () => {
    for (const heading of ['ID', 'Title / context', 'Status', 'Updated']) {
      expect(loaded, heading).toContain(`role="columnheader">${heading}<`);
    }
    expect(loaded.match(/role="columnheader"/g)).toHaveLength(4);
  });

  it('renders one row per issue the store carries, not a fixed list', () => {
    expect(loaded.match(/role="row"/g)).toHaveLength(ISSUES.length + 1);
    for (const issue of ISSUES) {
      expect(loaded, issue.title).toContain(`#${issue.number}`);
    }
  });

  it('draws the status pill in the accent, as a pill', () => {
    // `--acc35` border and `--acc` text, radius 99 — and the count says every row has one,
    // so a pill that only appeared on the first row would fail.
    const pills = loaded.match(/border-acc35 text-acc text-chip rounded-pill/g);
    expect(pills).toHaveLength(ISSUES.length);
  });

  it('gives every label pill a token background rather than GitHub’s own colour', () => {
    // `gh` sends a hex per label. Rendering it would be a pixel the accent picker cannot
    // reach, so only the name survives — see `./issue.ts`.
    expect(loaded).toContain('bg-bg3 rounded-pill');
    // The name reaches the pill; nothing else about the label does.
    expect(loaded).toContain('enhancement');
    expect(loaded).toContain('wayfinder:map');
  });

  it('names each Start button after its own issue', () => {
    // The visible label is the same five characters on every row, so a screen reader reading
    // "Start" eleven times would say nothing about which one is focused.
    const starts = [...loaded.matchAll(/aria-label="Start #(\d+):/g)].map((m) => m[1]);
    expect(starts).toHaveLength(ISSUES.length);
    expect(new Set(starts).size).toBe(ISSUES.length);
  });

  it('writes the age in words, from the store’s timestamp', () => {
    // The seed's rows are offsets from now, so this is arithmetic on a wire value rather
    // than the design mock's pre-formatted string reaching the screen intact.
    expect(loaded).toContain('7 days ago');
    expect(loaded).toContain('49 days ago');
  });
});

describe('the four ways to show nothing', () => {
  const screens = {
    empty: showing({ phase: 'loaded', issues: [] }),
    missing: showing(unavailable('gh_missing', 'gh is not on PATH', ['Install it.'])),
    unauthenticated: showing(
      unavailable('gh_unauthenticated', 'nobody is signed in', ['Run gh auth login.']),
    ),
    failed: showing(unavailable('query_failed', 'github.com did not answer', ['Try again.'])),
  };

  it('shows a different heading for each, which is the whole requirement', () => {
    // "An empty list for any of those is a lie. A user with no issues and a user whose token
    // expired must not see the same screen." Asserted on the *rendered* screens rather than
    // on the module that chooses the headings, because the module passing and the component
    // rendering one of them is exactly the gap worth closing.
    const headings = Object.values(screens).map(headingOf);
    expect(headings.every((heading) => heading.length > 0)).toBe(true);
    expect(new Set(headings).size, headings.join(' | ')).toBe(4);
  });

  it('never draws a table on any of them', () => {
    for (const [name, markup] of Object.entries(screens)) {
      expect(markup, name).not.toContain('role="columnheader"');
    }
  });

  it('repeats the daemon’s sentence and its steps verbatim', () => {
    // The panel's own heading is four words; the part that says `gh auth login` is the
    // daemon's, and this is the last place it could be thrown away.
    expect(screens.unauthenticated).toContain('nobody is signed in');
    expect(screens.unauthenticated).toContain('Run gh auth login.');
  });

  it('says a repository with no issues in a voice that is not a failure', () => {
    // `empty` takes the plain border; `failed` takes the status colour. A working screen
    // that reads as a broken one is the failure here.
    expect(screens.empty).toContain('border-line');
    expect(screens.empty).not.toContain('border-status-failed');
    expect(screens.failed).toContain('border-status-failed');
    // Both `gh` states are setup rather than failure, so neither wears the failure colour.
    expect(screens.missing).not.toContain('border-status-failed');
    expect(screens.unauthenticated).not.toContain('border-status-failed');
  });

  it('offers a way back on every one of them', () => {
    for (const [name, markup] of Object.entries(screens)) {
      expect(markup, name).toContain('Ask again');
    }
  });

  it('says a query is running rather than showing nothing at all', () => {
    // Not one of the four, and the one state where a blank region would be indistinguishable
    // from a screen that had quietly given up. There is nothing to ask again while one is in
    // flight, so the button is not offered.
    const loading = showing({ phase: 'loading' });
    expect(loading).toContain('Asking GitHub…');
    expect(loading).not.toContain('Ask again');
    expect(loading).not.toContain('role="columnheader"');
  });
});

describe('what it says about a Start that happened', () => {
  const started = (adopted: boolean): string =>
    showing(
      { phase: 'loaded', issues: ISSUES },
      { phase: 'started', issue: 200, branch: 'issue/200-add-it', paneKey: 'tab_9:leaf_1', adopted },
    );

  it('tells a created worktree from an adopted one', () => {
    // The flag wave C's contract requires the answer to carry. "Created a worktree" and
    // "moved into the one that was already there, with whatever is in it" are different
    // enough to act on, so they must not render as the same sentence.
    expect(started(false)).toContain('in a new worktree on issue/200-add-it');
    expect(started(true)).toContain('in the worktree already on issue/200-add-it');
    expect(started(false)).not.toContain('already on');
  });

  it('does not move the window on its own, but offers the way there', () => {
    // Starting three issues in a row is ordinary; a window that jumped after each one would
    // make it impossible. So the screen stays and the button says where it goes.
    expect(started(true)).toContain('Open the session');
  });

  it('marks the issue being started as busy rather than leaving it pressable', () => {
    const starting = showing(
      { phase: 'loaded', issues: ISSUES },
      { phase: 'starting', issue: 200 },
    );
    expect(starting).toContain('Starting…');
    expect(starting.match(/Starting…/g)).toHaveLength(1);
  });

  it('holds every other Start shut while one is in flight', () => {
    // The store refuses a second start by *resolving*, so a row left pressable would swallow
    // the click and show nothing for it. One worktree at a time, said on screen.
    const starting = showing(
      { phase: 'loaded', issues: ISSUES },
      { phase: 'starting', issue: 200 },
    );
    const starts = [...starting.matchAll(/<button(?=[^>]*aria-label="Start #)[^>]*>/g)].map(
      (match) => match[0],
    );
    expect(starts).toHaveLength(ISSUES.length);
    expect(starts.every((tag) => tag.includes('disabled=""'))).toBe(true);

    // And live again the moment nothing is starting, or the screen is a dead end.
    const idle = showing({ phase: 'loaded', issues: ISSUES });
    expect(idle).not.toMatch(/<button(?=[^>]*aria-label="Start #)[^>]*disabled=""/);
  });
});

describe('honesty', () => {
  const loaded = showing({ phase: 'loaded', issues: ISSUES });

  it('gives every control that cannot work a reason and a when', () => {
    // The standard `App.render.test.ts` holds the rail and the sidebar's `+` to: an
    // affordance that looks live and does nothing costs somebody a bug report. The row's
    // `⋮` is disabled on every row, so the count is the row count plus the five in the two
    // header rows that are drawn from the design and not yet wired.
    //
    // Matched on `disabled="` and not on `disabled`, which is not pedantry: Tailwind's
    // variant spells the class `disabled:opacity-60`, so the looser pattern counts the
    // *live* refresh button as disabled — and this assertion passed against it while
    // meaning nothing, until the refresh button had no title and said so.
    const disabled = [...loaded.matchAll(/<button(?=[^>]*\sdisabled=")[^>]*>/g)].map((m) => m[0]);
    expect(disabled.length).toBeGreaterThan(ISSUES.length);
    for (const tag of disabled) {
      expect(tag, tag).toMatch(/title="[^"]+"/);
    }
  });

  it('leaves the refresh live, because it is the one the failure panel points at', () => {
    const refresh = tagContaining(loaded, 'aria-label="Refresh the issue list"');
    expect(refresh).not.toContain('disabled="');
  });

  it('shows the repository in the source chip, recovered from an issue URL', () => {
    // `gh issue list --json` has no `repository` field, so this is the one place either half
    // appears. The seed's issues are on `Shironex/Settly`.
    expect(loaded).toContain('GitHub · Local · Shironex/Settly');
  });

  it('says GitHub · Local alone when there is no issue to read a repository from', () => {
    // Rather than a guess. With no rows there is no URL, and inventing an owner would put a
    // person's name on somebody else's repository.
    expect(showing({ phase: 'loaded', issues: [] })).toContain('GitHub · Local<');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    // Deliberately not `App.render.test.ts`'s assertion, which is that the markup carries no
    // hex at all. It cannot hold here: an issue number renders as a hash followed by three
    // digits, and `#200` is a valid three-digit hex — the same collision that makes
    // `theme/colourGuard.ts` trip on an issue number written into a comment.
    //
    // So the claim is made exactly: every hex-shaped run in the markup is an issue id the
    // store handed over. A hardcoded colour would be a match that is not one of these.
    const found = [...loaded.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0]);
    const ids = new Set(ISSUES.map((issue) => `#${issue.number}`));
    expect(found.filter((match) => !ids.has(match))).toEqual([]);
    expect(found.length, 'the ids should be there at all').toBeGreaterThan(0);
    // The other three shapes a fixed colour can take have no such excuse.
    expect(loaded).not.toContain('style=');
    expect(loaded).not.toMatch(/\b(?:rgb|rgba|hsl|hsla|oklch)\(/);
  });
});

describe('with no project', () => {
  it('says so rather than showing a failed query', () => {
    // Nothing was asked and nothing refused, so this is not one of `TasksState`'s endings.
    // Reporting it as one would send somebody to check their `gh` installation.
    const markup = render({
      ...createSeedSnapshot(),
      nav: 'tasks',
      projects: [],
      activeProjectId: null,
    });
    expect(markup).toContain('No project is selected');
    expect(markup).not.toContain('could not be fetched');
    expect(markup).not.toContain('role="columnheader"');
  });
});

/** The bold line of whichever panel is showing. */
function headingOf(markup: string): string {
  return markup.match(/class="text-row font-semibold">([^<]*)</)?.[1] ?? '';
}

/** The whole opening tag that carries `marker`, however its attributes are ordered. */
function tagContaining(source: string, marker: string): string {
  const at = source.indexOf(marker);
  return source.slice(source.lastIndexOf('<', at), source.indexOf('>', at) + 1);
}
