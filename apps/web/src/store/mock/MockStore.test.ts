import { describe, expect, it } from 'vitest';

import { describeStoreContract } from '../storeContract';
import { createMockStore } from './MockStore';
import { SEED_ACTIVE_PROJECT, SEED_ACTIVE_TAB, SEED_PROJECTS, SEED_TABS } from './seed';

describeStoreContract('MockStore', () => createMockStore());

describe('MockStore seed', () => {
  it('serves the projects and tabs the design mock shows', () => {
    const snapshot = createMockStore().getSnapshot();
    expect(snapshot.projects.map((project) => project.name)).toEqual(
      SEED_PROJECTS.map((project) => project.name),
    );
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
});
