import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { statusChange, statusOf } from '../store/agentStatusFixture';
import { MockStore } from '../store/mock/MockStore';
import { createSeedSnapshot } from '../store/mock/seed';
import { StoreProvider } from '../store/StoreProvider';
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
    const store = new MockStore(createSeedSnapshot());
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
