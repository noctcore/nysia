import { createElement, type ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { beforeEach, describe, expect, it } from 'vitest';

import type { AgentState } from '../generated/AgentState';
import {
  agentNotifications,
  createAgentNotificationSink,
} from '../store/agentNotifications';
import { STATE_LABEL, STATE_TONE, TONE_CLASS } from '../store/agentStatus';
import {
  PERMITTED,
  SESSION_BOUNDARY,
  statusChange,
  statusOf,
} from '../store/agentStatusFixture';
import { MockStore } from '../store/mock/MockStore';
import { createSeedSnapshot } from '../store/mock/seed';
import { StoreProvider } from '../store/StoreProvider';
import { AgentNotifications } from './AgentNotifications';
import { StatusDot } from './StatusDot';
import { TabStrip } from './TabStrip';

/*
 * The dots, rendered — still node-only (D-18).
 *
 * `renderToStaticMarkup` needs no DOM and runs no effects, which is enough for what is
 * being claimed here: that the mapping in `store/agentStatus.ts` reaches the markup, that a
 * shell tab does not get a dot, and that a session start produces no notice all the way
 * through the component. What it cannot see is anything that needs a browser — hover,
 * focus, the 30-second tick — and none of those is what this file is for.
 *
 * Assertions are on `data-status` and the token class rather than on the surrounding
 * markup: which element wraps a dot is the tab strip's business and it has changed once
 * already, whereas "the amber dot is the one with the waiting row" is this feature's.
 */

const EVERY_STATE: readonly AgentState[] = ['working', 'waiting', 'done', 'interrupted'];
const PANE = 'tab_1:leaf_1';

/** The seeded agent tabs. The other two in the strip are shells. */
const AGENT_TABS = 2;

function render(node: ReactElement, store = new MockStore()): string {
  return renderToStaticMarkup(
    createElement(StoreProvider, { store, children: node }),
  );
}

/**
 * A dot rendered against a clock that agrees with the row.
 *
 * The fixture's default `observed_at` is a fixed instant, so a `working` row measured
 * against `Date.now()` is years stale and decays. Every case that is not *about* staleness
 * stamps the row with the same `now` it renders against.
 */
function dot(state: AgentState, now = Date.now()): string {
  return renderToStaticMarkup(
    createElement(StatusDot, { status: statusOf(PANE, state, { observedAt: now }), now }),
  );
}

describe('the dot itself', () => {
  it('paints each state with its palette token and says which it is', () => {
    for (const state of EVERY_STATE) {
      const markup = dot(state);
      expect(markup, state).toContain(TONE_CLASS[STATE_TONE[state]]);
      expect(markup, state).toContain(`data-status="${STATE_TONE[state]}"`);
      expect(markup, state).toContain(`title="${STATE_LABEL[state]}"`);
      // Labelled, not hidden: the dot is the only place the state appears, so a reader that
      // skipped it would be told a session exists and not what it is doing.
      expect(markup, state).toContain('role="img"');
    }
  });

  it('keeps the accent off every state-bearing dot', () => {
    // design-spec.md §1: status colours are independent of the accent, deliberately. This is
    // that sentence at the rendering layer — `theme/tokens.test.ts` holds it at the token
    // layer, where the switcher never emits a status variable at all.
    for (const state of EVERY_STATE) {
      expect(dot(state), state).not.toContain('bg-acc');
    }
  });

  it('falls back to the accent when the daemon has no row for the pane', () => {
    const markup = renderToStaticMarkup(
      createElement(StatusDot, { status: undefined, now: Date.now() }),
    );
    expect(markup).toContain('bg-acc');
    expect(markup).toContain('data-status="unknown"');
    expect(markup).toContain('no status yet');
  });

  it('decays a stale working dot without changing its colour', () => {
    const now = Date.now();
    const markup = renderToStaticMarkup(
      createElement(StatusDot, {
        status: statusOf(PANE, 'working', { observedAt: now - 4 * 60 * 60 * 1000 }),
        now,
      }),
    );
    expect(markup).toContain(TONE_CLASS.running);
    expect(markup).toContain('opacity-50');
    expect(markup).toContain('data-status="running"');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    const markup = EVERY_STATE.map((state) => dot(state)).join('');
    expect([...markup.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});

describe('the tab strip', () => {
  it('gives a dot to every agent tab and none to a shell', () => {
    // A tab is a session and a session is a terminal *or* an agent. A dot beside `pwsh`
    // would be claiming a lifecycle a shell does not have.
    const markup = render(createElement(TabStrip));
    expect(markup.match(/data-status="/g)).toHaveLength(AGENT_TABS);
  });

  it('shows the state the store carries, not a default', () => {
    // The seed puts the Kirei agent mid-turn and the Codex one blocked on a question, so a
    // strip that painted one colour for every agent would fail here.
    const markup = render(createElement(TabStrip));
    expect(markup).toContain('data-status="running"');
    expect(markup).toContain('data-status="needsInput"');
  });

  it('follows a change the store received', () => {
    // Its own sink, although this case is about dots and not notices. `receiveAgentStatus`
    // feeds both, so the module-level sink would take a write from a test that never looks
    // at it — harmless while the block below clears in `beforeEach`, and a shared-global
    // write that a future reorder turns into a cross-test dependency.
    const store = new MockStore(createSeedSnapshot(), {
      notifications: createAgentNotificationSink(),
    });
    store.receiveAgentStatus(statusChange(statusOf(PANE, 'interrupted')));

    const markup = render(createElement(TabStrip), store);
    expect(markup).toContain('data-status="failed"');
    expect(markup).toContain(`title="${STATE_LABEL.interrupted}"`);
  });
});

describe('the notices', () => {
  beforeEach(() => {
    // The sink is module-level, like `unexpectedFailures` — one window, one stack of notices
    // in the corner of it — so a case that left one behind would leak into the next.
    agentNotifications.clear();
  });

  it('renders nothing at all when there is nothing to say', () => {
    expect(render(createElement(AgentNotifications))).toBe('');
  });

  it('names the session rather than the pane key when a tab is showing it', () => {
    const store = new MockStore(createSeedSnapshot(), { notifications: agentNotifications });
    store.receiveAgentStatus(statusChange(statusOf(PANE, 'waiting')));

    const markup = render(createElement(AgentNotifications), store);
    expect(markup).toContain('Needs input');
    expect(markup).toContain('Kirei deps');
    expect(markup).toContain(TONE_CLASS.needsInput);
  });

  it('falls back to the pane key for a session whose tab is gone', () => {
    // An agent finishing is exactly when someone closes its tab, so the notice outliving it
    // is ordinary rather than hypothetical.
    const store = new MockStore(createSeedSnapshot(), { notifications: agentNotifications });
    store.receiveAgentStatus(statusChange(statusOf('tab_99:leaf_1', 'done')));

    expect(render(createElement(AgentNotifications), store)).toContain('tab_99:leaf_1');
  });

  it('shows nothing for a session start, end to end', () => {
    // The whole rule, through the component that would have shown it: `SessionStart` maps to
    // `done`, and `done` is a state that normally notifies.
    const store = new MockStore(createSeedSnapshot(), { notifications: agentNotifications });
    store.receiveAgentStatus(
      statusChange(statusOf(PANE, 'done', { sessionBoundary: true }), SESSION_BOUNDARY),
    );

    expect(render(createElement(AgentNotifications), store)).toBe('');
    // …and the dot still moved. Suppressing the notice is not suppressing the status.
    expect(store.getSnapshot().agentStatus[0]?.lead.state).toBe('done');
  });

  it('offers a dismiss control per notice', () => {
    const store = new MockStore(createSeedSnapshot(), { notifications: agentNotifications });
    store.receiveAgentStatus(statusChange(statusOf(PANE, 'done'), PERMITTED));

    const markup = render(createElement(AgentNotifications), store);
    expect(markup).toMatch(/aria-label="Dismiss Done for [^"]+"/);
    expect(markup).toContain('aria-live="polite"');
  });
});
