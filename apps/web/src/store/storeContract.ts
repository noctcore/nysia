import { describe, expect, it } from 'vitest';

import type { Store } from './types';

/**
 * The executable statement of the store contract.
 *
 * `MockStore` runs it now; W5's daemon-backed provider runs the same suite when it lands,
 * which is what makes "swap the provider, touch no component" a checkable claim rather than
 * a hope. Anything asserted here is something a component is allowed to rely on.
 *
 * It is a plain module, not a `.test.ts`, so vitest does not pick it up on its own — it is
 * a suite factory, and each provider's own test file supplies the factory.
 */
export function describeStoreContract(name: string, create: () => Store): void {
  describe(`${name} — Store contract`, () => {
    it('returns a referentially stable snapshot while nothing changes', () => {
      // A provider that allocates per call renders forever under useSyncExternalStore.
      const store = create();
      expect(store.getSnapshot()).toBe(store.getSnapshot());
    });

    it('exposes subscribe and getSnapshot already bound', () => {
      // useSyncExternalStore receives both detached from the object.
      const { getSnapshot, subscribe } = create();
      expect(() => getSnapshot()).not.toThrow();
      const unsubscribe = subscribe(() => {});
      expect(() => unsubscribe()).not.toThrow();
    });

    it('seeds a snapshot the chrome can render', () => {
      const snapshot = create().getSnapshot();
      expect(snapshot.projects.length).toBeGreaterThan(0);
      expect(snapshot.tabs.length).toBeGreaterThan(0);
      expect(snapshot.launchers.length).toBeGreaterThan(0);
      expect(snapshot.usage.length).toBeGreaterThan(0);
      expect(snapshot.nav).toBe('session');
    });

    it('keys every tab and every project uniquely', () => {
      const snapshot = create().getSnapshot();
      const panes = snapshot.tabs.map((tab) => tab.paneKey);
      expect(new Set(panes).size).toBe(panes.length);
      const projects = snapshot.projects.map((project) => project.id);
      expect(new Set(projects).size).toBe(projects.length);
    });

    it('points activeTab and activeProjectId at something that exists', () => {
      const snapshot = create().getSnapshot();
      expect(snapshot.tabs.some((tab) => tab.paneKey === snapshot.activeTab)).toBe(true);
      expect(
        snapshot.projects.some((project) => project.id === snapshot.activeProjectId),
      ).toBe(true);
    });

    it('notifies subscribers on a change and stops after unsubscribe', async () => {
      const store = create();
      let calls = 0;
      const unsubscribe = store.subscribe(() => {
        calls += 1;
      });

      await store.selectNav('tasks');
      expect(calls).toBe(1);
      expect(store.getSnapshot().nav).toBe('tasks');

      unsubscribe();
      await store.selectNav('history');
      expect(calls).toBe(1);
      expect(store.getSnapshot().nav).toBe('history');
    });

    it('does not notify when a command changes nothing', async () => {
      const store = create();
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

    it('ignores an identity it does not know rather than clearing the selection', async () => {
      const store = create();
      const before = store.getSnapshot();

      await store.selectTab('tab_nope:leaf_nope');
      await store.selectProject('nowhere');
      await store.openTab('launcher.that.does.not.exist');

      expect(store.getSnapshot()).toBe(before);
    });

    it('selects a tab and a project by identity', async () => {
      const store = create();
      const { tabs, projects, activeTab, activeProjectId } = store.getSnapshot();
      const otherTab = tabs.find((tab) => tab.paneKey !== activeTab);
      const otherProject = projects.find((project) => project.id !== activeProjectId);
      expect(otherTab, 'contract needs at least two seeded tabs').toBeDefined();
      expect(otherProject, 'contract needs at least two seeded projects').toBeDefined();

      await store.selectTab(otherTab?.paneKey ?? '');
      expect(store.getSnapshot().activeTab).toBe(otherTab?.paneKey);

      await store.selectProject(otherProject?.id ?? '');
      expect(store.getSnapshot().activeProjectId).toBe(otherProject?.id);
    });

    it('opens a tab from a launcher and focuses it', async () => {
      const store = create();
      const launcher = store.getSnapshot().launchers[0]?.items[0];
      expect(launcher).toBeDefined();
      const before = store.getSnapshot().tabs.length;

      await store.openTab(launcher?.id ?? '');
      const after = store.getSnapshot();
      expect(after.tabs).toHaveLength(before + 1);
      const opened = after.tabs[after.tabs.length - 1];
      expect(opened?.paneKey).toBe(after.activeTab);
      expect(opened?.kind).toBe(launcher?.kind);
      expect(new Set(after.tabs.map((tab) => tab.paneKey)).size).toBe(after.tabs.length);
    });

    it('moves focus to a neighbour when the focused tab closes', async () => {
      const store = create();
      const focused = store.getSnapshot().activeTab ?? '';

      await store.closeTab(focused);
      const after = store.getSnapshot();
      expect(after.tabs.some((tab) => tab.paneKey === focused)).toBe(false);
      expect(after.activeTab).not.toBe(focused);
      expect(after.tabs.some((tab) => tab.paneKey === after.activeTab)).toBe(true);
    });

    it('leaves no tab focused once the last one closes', async () => {
      const store = create();
      for (const tab of [...store.getSnapshot().tabs]) {
        await store.closeTab(tab.paneKey);
      }
      const after = store.getSnapshot();
      expect(after.tabs).toHaveLength(0);
      expect(after.activeTab).toBeNull();
    });

    it('closing an unfocused tab leaves focus where it was', async () => {
      const store = create();
      const { tabs, activeTab } = store.getSnapshot();
      const other = tabs.find((tab) => tab.paneKey !== activeTab);
      expect(other).toBeDefined();

      await store.closeTab(other?.paneKey ?? '');
      expect(store.getSnapshot().activeTab).toBe(activeTab);
    });

    it('exposes window controls that resolve', async () => {
      const store = create();
      await expect(store.window.minimize()).resolves.toBeUndefined();
      await expect(store.window.toggleMaximize()).resolves.toBeUndefined();
      await expect(store.window.close()).resolves.toBeUndefined();
    });
  });
}
