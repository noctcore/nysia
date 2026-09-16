import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { createAgentNotificationSink } from '../store/agentNotifications';
import { statusChange, statusOf } from '../store/agentStatusFixture';
import { MockStore } from '../store/mock/MockStore';
import { createSeedSnapshot } from '../store/mock/seed';
import { StoreProvider } from '../store/StoreProvider';
import type { AddProjectState } from '../store/addProject';
import type { StoreSnapshot } from '../store/types';
import { ProjectsSidebar } from './ProjectsSidebar';

/*
 * The sidebar half of design-spec.md §6.2: *sessions nest under projects, and the sidebar
 * shows live status and age per session.*
 *
 * Node-only (D-18) — `renderToStaticMarkup` runs no effects and needs no DOM. It cannot see
 * the 30-second tick that keeps the age moving, which is `useNow`'s to hold; what it can see
 * is that the dot reflects the store rather than a literal, and that is the claim v0.2 rests
 * on.
 *
 * The expanded project is the seed's `shiroani`, whose branch block holds one agent and one
 * shell — so a sidebar that painted a dot for everything and a sidebar that painted one for
 * nothing both fail here.
 */

const KIREI_PANE = 'tab_1:leaf_1';

function render(store: MockStore): string {
  return renderToStaticMarkup(
    createElement(StoreProvider, {
      store,
      children: createElement(ProjectsSidebar),
    }),
  );
}

function withoutStatus(): StoreSnapshot {
  return { ...createSeedSnapshot(), agentStatus: [] };
}

describe('the sidebar session rows', () => {
  it('shows one dot, for the one agent in the expanded branch', () => {
    // The shell row gets `>_` and no dot: a terminal has no agent and therefore no
    // lifecycle, and a grey dot beside it would be inventing one.
    const markup = render(new MockStore());
    expect(markup.match(/data-status="/g)).toHaveLength(1);
    expect(markup).toContain('data-status="running"');
  });

  it('keeps the age beside it, which is the other half of the spec sentence', () => {
    expect(render(new MockStore())).toContain('>21h<');
  });

  it('follows the store when the state changes', () => {
    // Its own sink: `receiveAgentStatus` feeds the notices as well as the dots, and this
    // file never renders a notice — so without it a sidebar case writes into the window's
    // module-level sink and leaves a notice behind for whatever runs next.
    const store = new MockStore(createSeedSnapshot(), {
      notifications: createAgentNotificationSink(),
    });
    store.receiveAgentStatus(statusChange(statusOf(KIREI_PANE, 'waiting')));

    const markup = render(store);
    expect(markup).toContain('data-status="needsInput"');
    expect(markup).toContain('title="Needs input"');
  });

  it('keeps the accent for an agent the daemon has no row for', () => {
    // What the window actually shows today: the daemon's status RPC is v0.2 wave C, so the
    // daemon-backed provider reports nothing and every agent row falls back to v0.1's
    // "an agent lives here" — which is the honest thing to say and not a lifecycle claim.
    const markup = render(new MockStore(withoutStatus()));
    expect(markup).toContain('data-status="unknown"');
    expect(markup).toContain('bg-acc');
    expect(markup).not.toContain('data-status="queued"');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    const markup = render(new MockStore());
    expect([...markup.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});

/*
 * v0.3 wave B2: the sidebar stops pretending, and `+` does something.
 *
 * What these assert is what this component owns — whether the affordance is reachable, and
 * which of the five outcomes it is showing. They do not count markup: the panel's copy is
 * `store/addProject.ts`'s and is asserted there without a DOM, which is the reason that
 * reading lives in a module of its own.
 */

function snapshotWith(patch: Partial<StoreSnapshot>): StoreSnapshot {
  return { ...createSeedSnapshot(), ...patch };
}

/** A daemon that answered, and has nothing. */
function empty(unavailable: string | null = null): StoreSnapshot {
  return snapshotWith({ status: 'ready', projects: [], projectsUnavailable: unavailable });
}

describe('adding a project from the sidebar', () => {
  it('offers the + rather than explaining why it cannot', () => {
    // It was `disabled`, titled "Adding a project arrives with the worktree manager in
    // v0.4" — an honest statement of a plan that has since changed, because `Start →`
    // creates a worktree *in a project* and there is nothing to key against until one is
    // registered (v0.3 plan §1).
    const markup = render(new MockStore());
    expect(markup).toContain('aria-label="Add a project"');
    expect(markup).not.toContain('v0.4');
    // The attribute, not the word: the button carries a `disabled:` Tailwind variant for the
    // state below, and matching the bare word would pass on a button that never enables.
    expect(markup).not.toContain('disabled=""');
  });

  it('does not promise to add a project to a particular group', () => {
    // The group is `Project::DEFAULT_GROUP`, decided by the daemon, and there is no verb to
    // change it. A label reading "Add a project to Dev" would promise one.
    expect(render(new MockStore())).not.toContain('Add a project to');
  });

  it('holds the + shut while a picker is already open', () => {
    // Two pickers is two registrations racing.
    const markup = render(new MockStore(snapshotWith({ addProject: { phase: 'browsing' } })));
    expect(markup).toContain('disabled=""');
    expect(markup).toContain('Waiting for the folder picker');
  });

  it('shows each outcome as its own thing', () => {
    // Four treatments across the five answers §3.2 allows. The one that matters is the
    // last: a folder that was already registered is a fact, and painting it in the failure
    // colour would teach a user that repeating themselves breaks Nysia.
    const outcomes: readonly [AddProjectState, string][] = [
      [{ phase: 'added', name: 'nysia', alreadyRegistered: false }, 'added'],
      [{ phase: 'added', name: 'nysia', alreadyRegistered: true }, 'known'],
      [
        {
          phase: 'refused',
          code: 'many_repositories',
          message: 'but 3 of the folders in it are',
          nextSteps: ['Pick one of these: nysia, orca, valve.'],
        },
        'choose',
      ],
      [
        {
          phase: 'refused',
          code: 'not_a_repository',
          message: 'that folder is not a git repository',
          nextSteps: ['Choose the folder that has the `.git` in it.'],
        },
        'refused',
      ],
    ];

    for (const [addProject, tone] of outcomes) {
      const markup = render(new MockStore(snapshotWith({ addProject })));
      expect(markup, tone).toContain(`data-outcome="${tone}"`);
    }

    // And the two that are not failures never reach the failure token.
    for (const alreadyRegistered of [false, true]) {
      const markup = render(
        new MockStore(
          snapshotWith({ addProject: { phase: 'added', name: 'nysia', alreadyRegistered } }),
        ),
      );
      expect(markup, String(alreadyRegistered)).not.toContain('status-failed');
    }
  });

  it('puts the daemon’s own words under a refusal, verbatim', () => {
    // The detail is the daemon's: the repositories it found, the `git init` that would fix
    // it. Neither is reachable from this side, and both are the part a person acts on.
    const markup = render(
      new MockStore(
        snapshotWith({
          addProject: {
            phase: 'refused',
            code: 'many_repositories',
            message: 'that folder is not a git repository, but 3 of the folders in it are',
            nextSteps: ['Pick one of these and register that folder instead: nysia, orca, valve.'],
          },
        }),
      ),
    );
    expect(markup).toContain('but 3 of the folders in it are');
    expect(markup).toContain('nysia, orca, valve');
    // Acting on that advice must not mean finding the menu again.
    expect(markup).toContain('Choose another folder');
  });

  it('shows nothing while the picker is open or the daemon is deciding', () => {
    for (const phase of ['browsing', 'registering'] as const) {
      expect(render(new MockStore(snapshotWith({ addProject: { phase } }))), phase).not.toContain(
        'data-outcome',
      );
    }
  });
});

describe('a sidebar with no projects', () => {
  it('says why, in the daemon’s words, and still offers the +', () => {
    // The `+` lives on a group header, and with no projects there are no headers — so the
    // one affordance that fixes an empty sidebar would have been missing from the empty
    // sidebar.
    const markup = render(
      new MockStore(empty('this daemon does not serve the project verbs yet')),
    );
    expect(markup).toContain('this daemon does not serve the project verbs yet');
    expect(markup).toContain('aria-label="Add a project"');
  });

  it('tells an empty daemon apart from one that would not answer', () => {
    expect(render(new MockStore(empty()))).toContain('No projects yet');
    expect(render(new MockStore(empty()))).not.toContain('Looking for the daemon');
  });

  it('does not ask for a folder before it has reached the daemon', () => {
    // "No projects yet" against a window that has not finished connecting is a claim it
    // cannot support, and the fix it offers would be the wrong one.
    const markup = render(
      new MockStore(snapshotWith({ status: 'connecting', projects: [], projectsUnavailable: null })),
    );
    expect(markup).toContain('Looking for the daemon');
    expect(markup).not.toContain('No projects yet');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    // `theme/colourGuard.ts` walks the source; this walks what the source rendered, for the
    // states the guard cannot construct.
    for (const snapshot of [
      empty('this daemon does not serve the project verbs yet'),
      snapshotWith({
        addProject: { phase: 'added', name: 'nysia', alreadyRegistered: true },
      }),
      snapshotWith({
        addProject: {
          phase: 'refused',
          code: 'path_unreadable',
          message: 'that path could not be read',
          nextSteps: ['Check the drive is plugged in.'],
        },
      }),
    ]) {
      const markup = render(new MockStore(snapshot));
      expect([...markup.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
    }
  });
});

describe('a project list the window could not re-read', () => {
  it('says so above the rows it is still drawing', () => {
    // The rows were true when the daemon said them and this says nothing about now, so
    // silence here would be the stalest thing the sidebar does.
    const markup = render(
      new MockStore(snapshotWith({ projectsUnavailable: 'the connection to the daemon failed' })),
    );
    expect(markup).toContain('last heard about');
    expect(markup).toContain('the connection to the daemon failed');
  });

  it('says nothing when the list is current', () => {
    expect(render(new MockStore())).not.toContain('last heard about');
  });

  it('does not say it twice when there are no rows at all', () => {
    // The empty state already carries the sentence, and a sidebar showing it above an
    // explanation of itself reads as two different things having gone wrong.
    const markup = render(new MockStore(empty('this daemon does not serve the project verbs yet')));
    expect(markup).not.toContain('last heard about');
    expect(markup).toContain('this daemon does not serve the project verbs yet');
  });
});
