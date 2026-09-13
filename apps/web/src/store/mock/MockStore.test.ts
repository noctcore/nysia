import { describe, expect, it } from 'vitest';

import { describeStoreContract } from '../storeContract';
import { createMockStore } from './MockStore';
import {
  SEED_ACTIVE_PROJECT,
  SEED_ACTIVE_TAB,
  SEED_PROJECT_NAMES,
  SEED_TABS,
} from './seed';

describeStoreContract('MockStore', () => createMockStore());

describe('MockStore seed', () => {
  it('serves the projects and tabs the design mock shows', () => {
    const snapshot = createMockStore().getSnapshot();
    expect(snapshot.projects.map((project) => project.name)).toEqual(SEED_PROJECT_NAMES);
    expect(snapshot.tabs.map((tab) => tab.title)).toEqual(SEED_TABS.map((tab) => tab.title));
    expect(snapshot.activeTab).toBe(SEED_ACTIVE_TAB);
    expect(snapshot.activeProjectId).toBe(SEED_ACTIVE_PROJECT);
  });

  it('expands exactly one project, on its primary branch', () => {
    const snapshot = createMockStore().getSnapshot();
    const expanded = snapshot.projects.filter((project) => project.worktrees.length > 0);
    expect(expanded).toHaveLength(1);
    expect(expanded[0]?.id).toBe(snapshot.activeProjectId);
    expect(expanded[0]?.worktrees[0]?.branch).toBe('master');
    expect(expanded[0]?.worktrees[0]?.isPrimary).toBe(true);
  });

  it('gives two independent stores independent state', async () => {
    const a = createMockStore();
    const b = createMockStore();
    await a.selectNav('tasks');
    expect(b.getSnapshot().nav).toBe('session');
  });

  it('is ready the moment it is constructed', () => {
    // The one thing this store has that a socket-backed one cannot: no handshake. It is
    // asserted here rather than in the contract precisely because it is not a contract.
    expect(createMockStore().getSnapshot().status).toBe('ready');
  });
});

/*
 * Below: guarantees this store makes that the contract deliberately does not.
 *
 * They used to live in `storeContract.ts`, where they quietly required every future
 * provider to be synchronous and to short-circuit — which is how a contract suite stops
 * being reusable. They are real properties of this store and worth keeping; they are just
 * not things a component may assume, because a daemon cannot promise them.
 */
describe('MockStore policies the contract leaves open', () => {
  it('does not notify at all when a command changes nothing', async () => {
    const store = createMockStore();
    let calls = 0;
    store.subscribe(() => {
      calls += 1;
    });
    const before = store.getSnapshot();

    await store.selectNav(before.nav);
    await store.selectTab(before.activeTab ?? '');

    expect(calls).toBe(0);
    expect(store.getSnapshot()).toBe(before);
  });

  it('hands focus to the right-hand neighbour when the focused tab closes', async () => {
    const store = createMockStore();
    const { tabs, activeTab } = store.getSnapshot();
    const index = tabs.findIndex((tab) => tab.paneKey === activeTab);
    const neighbour = tabs[index + 1];
    expect(neighbour).toBeDefined();

    await store.closeTab(activeTab ?? '');
    expect(store.getSnapshot().activeTab).toBe(neighbour?.paneKey);
  });

  it('falls back to the new last tab when the rightmost one closes', async () => {
    const store = createMockStore();
    const tabs = store.getSnapshot().tabs;
    const last = tabs[tabs.length - 1];
    const penultimate = tabs[tabs.length - 2];

    await store.selectTab(last?.paneKey ?? '');
    await store.closeTab(last?.paneKey ?? '');
    expect(store.getSnapshot().activeTab).toBe(penultimate?.paneKey);
  });

  it('appends an opened tab rather than prepending it', async () => {
    const store = createMockStore();
    const launcher = store.getSnapshot().launchers[0]?.items[0];
    await store.openTab(launcher?.id ?? '');
    const tabs = store.getSnapshot().tabs;
    expect(tabs[tabs.length - 1]?.paneKey).toBe(store.getSnapshot().activeTab);
  });
});
