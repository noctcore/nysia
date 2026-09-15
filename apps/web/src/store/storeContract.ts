import { describe, expect, it } from 'vitest';

import { StoreCommandError, hasDistinctIds } from './errors';
import type { Store, StoreSnapshot } from './types';

/**
 * The executable statement of the store contract.
 *
 * Anything asserted here is something a component is allowed to rely on, and therefore
 * something every provider must do. That makes the suite's own shape load-bearing: a
 * contract that quietly assumes the mock's timing is not a contract, it is a second copy
 * of the mock's tests. The first version of this file did assume it in three places, and a
 * daemon-shaped provider failed eight of fourteen cases:
 *
 *  - it asserted seeded content at t=0, which no socket-backed provider can satisfy —
 *    hence `create` may be async and every content assertion runs after `status` reaches
 *    `ready`;
 *  - it required `getSnapshot()` to be *referentially* identical after a command that
 *    changed nothing, which forbids a provider that acknowledges every request with a
 *    fresh frame — hence the comparisons are structural, and reference stability is
 *    asserted only where React needs it, between notifications;
 *  - it required an unknown identity to resolve silently, which is a decision rather than
 *    a fact — hence `./errors`, which decides that such a command rejects *and* records,
 *    and this suite asserts that.
 *
 * `AsyncProbeStore` is the guard on all of that: a deliberately hostile provider whose
 * first frame lands on a timer and which emits two frames per command. If the suite ever
 * drifts back toward the mock's shape, the probe fails before W5 does.
 *
 * It is a plain module, not a `.test.ts`, so vitest does not pick it up on its own — it is
 * a suite factory, and each provider's own test file supplies the factory.
 */
export type StoreFactory = () => Store | Promise<Store>;

/** How long a provider gets to reach `ready` before the suite calls it hung. */
const READY_TIMEOUT_MS = 2000;

export function describeStoreContract(name: string, create: StoreFactory): void {
  /** A store that has finished connecting, which is where most cases start. */
  async function ready(): Promise<Store> {
    const store = await create();
    await waitForReady(store);
    return store;
  }

  describe(`${name} — Store contract`, () => {
    describe('fixture', () => {
      // Not requirements on a provider in the field — a daemon on a fresh machine has no
      // projects at all. They are what this suite needs in order to exercise selection,
      // and a failure here means the fixture is too small, not that the provider is wrong.
      it('reaches ready with at least two tabs, two projects and one launcher', async () => {
        const snapshot = (await ready()).getSnapshot();
        expect(snapshot.tabs.length, 'fixture needs ≥ 2 tabs').toBeGreaterThanOrEqual(2);
        expect(snapshot.projects.length, 'fixture needs ≥ 2 projects').toBeGreaterThanOrEqual(2);
        expect(snapshot.launchers.length, 'fixture needs ≥ 1 launcher group').toBeGreaterThanOrEqual(1);
        expect(snapshot.launchers[0]?.items.length, 'fixture needs ≥ 1 launcher').toBeGreaterThanOrEqual(1);
      });
    });

    describe('the snapshot', () => {
      it('exists before anything has connected', async () => {
        // The chrome renders from the first frame React asks for, which is before any
        // socket has opened. A provider that returns undefined here blanks the window.
        const snapshot = (await create()).getSnapshot();
        expect(snapshot).toBeDefined();
        expect(snapshot.errors).toEqual([]);
        expect(['connecting', 'ready', 'reconnecting', 'failed']).toContain(snapshot.status);
      });

      it('is referentially stable between notifications', async () => {
        // The requirement React actually has: a provider that allocates per call renders
        // forever under useSyncExternalStore. It says nothing about whether a command
        // produces a new frame — a provider that acks every request with one is fine.
        const store = await ready();
        expect(store.getSnapshot()).toBe(store.getSnapshot());

        await store.selectNav('tasks');
        const settled = store.getSnapshot();
        expect(store.getSnapshot()).toBe(settled);
        expect(store.getSnapshot()).toBe(settled);
      });

      it('exposes subscribe and getSnapshot already bound', async () => {
        // useSyncExternalStore receives both detached from the object.
        const { getSnapshot, subscribe } = await create();
        expect(() => getSnapshot()).not.toThrow();
        const unsubscribe = subscribe(() => {});
        expect(() => unsubscribe()).not.toThrow();
      });

      it('keys every tab and every project uniquely', async () => {
        const snapshot = (await ready()).getSnapshot();
        const panes = snapshot.tabs.map((tab) => tab.paneKey);
        expect(new Set(panes).size).toBe(panes.length);
        const projects = snapshot.projects.map((project) => project.id);
        expect(new Set(projects).size).toBe(projects.length);
      });

      it('points activeTab and activeProjectId at something that exists', async () => {
        const snapshot = (await ready()).getSnapshot();
        expect(snapshot.tabs.some((tab) => tab.paneKey === snapshot.activeTab)).toBe(true);
        expect(
          snapshot.projects.some((project) => project.id === snapshot.activeProjectId),
        ).toBe(true);
      });
    });

    describe('notification', () => {
      it('notifies subscribers on a change and stops after unsubscribe', async () => {
        const store = await ready();
        let calls = 0;
        const unsubscribe = store.subscribe(() => {
          calls += 1;
        });

        await store.selectNav('tasks');
        // At least one: a provider is entitled to emit an acknowledgement frame and then
        // the state frame, and pinning the count to 1 is how this suite used to exclude
        // every provider that talks to a socket.
        expect(calls).toBeGreaterThanOrEqual(1);
        expect(store.getSnapshot().nav).toBe('tasks');

        const seen = calls;
        unsubscribe();
        await store.selectNav('history');
        expect(calls).toBe(seen);
        expect(store.getSnapshot().nav).toBe('history');
      });

      it('leaves the observable state unchanged when a command changes nothing', async () => {
        const store = await ready();
        const before = store.getSnapshot();

        await store.selectNav(before.nav);
        await store.selectTab(before.activeTab ?? '');

        // Structural, not referential. Whether a no-op produces a new frame is a
        // provider's business; what the chrome sees must not move.
        expect(observable(store.getSnapshot())).toEqual(observable(before));
      });
    });

    describe('commands that cannot be satisfied', () => {
      // The decision, stated once: rejecting *and* recording. Rejecting alone loses the
      // message; recording alone leaves a caller unable to sequence. A provider that
      // resolved silently would leave the user staring at a menu that closed and a tab
      // that never appeared.
      it('rejects with a StoreCommandError naming the command', async () => {
        const store = await ready();
        await expect(store.selectTab('tab_nope:leaf_nope')).rejects.toBeInstanceOf(
          StoreCommandError,
        );
        await expect(store.selectProject('nowhere')).rejects.toBeInstanceOf(StoreCommandError);
        await expect(store.openTab('launcher.that.does.not.exist')).rejects.toBeInstanceOf(
          StoreCommandError,
        );
        await expect(store.closeTab('tab_nope:leaf_nope')).rejects.toBeInstanceOf(
          StoreCommandError,
        );
      });

      it('has already recorded the failure by the time the promise rejects', async () => {
        // Ordering, not just presence. A provider that rejects on one frame and records on
        // the next leaves a window in which the UI knows something failed and has nothing
        // to show for it.
        const store = await ready();
        const before = store.getSnapshot().errors.length;

        const rejection = await store.openTab('launcher.that.does.not.exist').catch(
          (cause: unknown) => cause,
        );
        expect(rejection).toBeInstanceOf(StoreCommandError);

        const errors = store.getSnapshot().errors;
        expect(errors).toHaveLength(before + 1);
        const recorded = errors[errors.length - 1];
        expect(recorded?.command).toBe('openTab');
        expect(recorded?.message.length).toBeGreaterThan(0);
        expect(recorded?.id).toBe(
          rejection instanceof StoreCommandError ? rejection.errorId : undefined,
        );
      });

      it('gives every failure its own id', async () => {
        // `CommandErrors` keys the notice list on this id and dismisses by it, so a
        // provider that reused one id for every failure would pass every other case here
        // and then, on screen, render duplicate React keys and a dismiss button that
        // cleared the whole list at once.
        const store = await ready();
        await store.openTab('launcher.that.does.not.exist').catch(() => {});
        await store.selectProject('nowhere').catch(() => {});

        const errors = store.getSnapshot().errors;
        expect(errors.length).toBeGreaterThanOrEqual(2);
        expect(hasDistinctIds(errors), errors.map((e) => e.id).join(', ')).toBe(true);
      });

      it('keeps ids distinct when the same command fails the same way twice', async () => {
        // Two failures a second apart are two notices, not one that flickers — which only
        // holds if the id is per failure rather than per command or per message.
        //
        // This cannot see the failure mode a real provider is most likely to have. A
        // millisecond clock passes, because two failures on a round trip straddle a
        // millisecond, and only a provider fast enough to fail twice inside one — a mock —
        // goes red. `hasDistinctIds` in ./errors carries the rule that follows from that:
        // a counter or a uuid, never a clock.
        const store = await ready();
        await store.openTab('launcher.that.does.not.exist').catch(() => {});
        await store.openTab('launcher.that.does.not.exist').catch(() => {});

        const repeated = store
          .getSnapshot()
          .errors.filter((error) => error.command === 'openTab');
        expect(repeated).toHaveLength(2);
        expect(repeated[0]?.id).not.toBe(repeated[1]?.id);
      });

      it('changes nothing else', async () => {
        const store = await ready();
        const before = store.getSnapshot();

        await store.selectTab('tab_nope:leaf_nope').catch(() => {});
        await store.selectProject('nowhere').catch(() => {});

        expect(observable(store.getSnapshot())).toEqual(observable(before));
      });

      it('dismisses a recorded failure, and dismissing twice is not an error', async () => {
        const store = await ready();
        await store.openTab('launcher.that.does.not.exist').catch(() => {});
        const recorded = store.getSnapshot().errors.at(-1);
        expect(recorded).toBeDefined();

        await store.dismissError(recorded?.id ?? '');
        expect(store.getSnapshot().errors.some((e) => e.id === recorded?.id)).toBe(false);
        await expect(store.dismissError(recorded?.id ?? '')).resolves.toBeUndefined();
      });
    });

    describe('selection', () => {
      it('selects a tab and a project by identity', async () => {
        const store = await ready();
        const { tabs, projects, activeTab, activeProjectId } = store.getSnapshot();
        const otherTab = tabs.find((tab) => tab.paneKey !== activeTab);
        const otherProject = projects.find((project) => project.id !== activeProjectId);
        expect(otherTab, 'fixture needs a second tab').toBeDefined();
        expect(otherProject, 'fixture needs a second project').toBeDefined();

        await store.selectTab(otherTab?.paneKey ?? '');
        expect(store.getSnapshot().activeTab).toBe(otherTab?.paneKey);

        await store.selectProject(otherProject?.id ?? '');
        expect(store.getSnapshot().activeProjectId).toBe(otherProject?.id);
      });

      it('moves the rail between its three destinations', async () => {
        const store = await ready();
        for (const section of ['tasks', 'history', 'session'] as const) {
          await store.selectNav(section);
          expect(store.getSnapshot().nav).toBe(section);
        }
      });
    });

    describe('opening and closing', () => {
      it('opens a tab from a launcher and focuses it', async () => {
        const store = await ready();
        const launcher = store.getSnapshot().launchers[0]?.items[0];
        expect(launcher).toBeDefined();
        const before = new Set(store.getSnapshot().tabs.map((tab) => tab.paneKey));

        await store.openTab(launcher?.id ?? '');
        const after = store.getSnapshot();

        // By set difference, not by position: that the new tab is focused is a contract,
        // that it was appended at the end is this provider's layout choice.
        const opened = after.tabs.filter((tab) => !before.has(tab.paneKey));
        expect(opened).toHaveLength(1);
        expect(opened[0]?.paneKey).toBe(after.activeTab);
        expect(opened[0]?.kind).toBe(launcher?.kind);
        expect(new Set(after.tabs.map((tab) => tab.paneKey)).size).toBe(after.tabs.length);
      });

      it('leaves the active tab pointing at a tab that exists after a close', async () => {
        // Which tab a provider activates next is its own decision — a daemon may well
        // pick the most recently used rather than the neighbour. What the chrome needs is
        // that it never points at a pane that is gone.
        const store = await ready();
        const closed = store.getSnapshot().activeTab ?? '';

        await store.closeTab(closed);
        const after = store.getSnapshot();
        expect(after.tabs.some((tab) => tab.paneKey === closed)).toBe(false);
        expect(after.activeTab).not.toBe(closed);
        expect(after.tabs.some((tab) => tab.paneKey === after.activeTab)).toBe(true);
      });

      it('never leaves activeTab pointing at nothing, even after closing them all', async () => {
        // Not `tabs.length === 0`: a daemon that opens a fresh shell when the last session
        // closes would fail that, and which sessions exist is its business — the same
        // over-specification that was removed from the close-policy case above. What the
        // chrome needs is only that `activeTab` is null or names a tab that is there.
        const store = await ready();
        for (const tab of [...store.getSnapshot().tabs]) {
          await store.closeTab(tab.paneKey);
        }
        const after = store.getSnapshot();
        expect(
          after.activeTab === null ||
            after.tabs.some((tab) => tab.paneKey === after.activeTab),
        ).toBe(true);
      });

      it('closing an unfocused tab leaves focus where it was', async () => {
        const store = await ready();
        const { tabs, activeTab } = store.getSnapshot();
        const other = tabs.find((tab) => tab.paneKey !== activeTab);
        expect(other, 'fixture needs a second tab').toBeDefined();

        await store.closeTab(other?.paneKey ?? '');
        expect(store.getSnapshot().activeTab).toBe(activeTab);
      });
    });

    it('exposes window controls that resolve', async () => {
      const store = await ready();
      await expect(store.window.minimize()).resolves.toBeUndefined();
      await expect(store.window.toggleMaximize()).resolves.toBeUndefined();
      await expect(store.window.close()).resolves.toBeUndefined();
    });
  });
}

/**
 * Block until the provider says it is connected.
 *
 * Checked once before subscribing, so a provider that is already `ready` — the mock, or a
 * reconnect that resolved between frames — does not wait for a notification that will
 * never come.
 */
async function waitForReady(store: Store): Promise<void> {
  if (store.getSnapshot().status === 'ready') {
    return;
  }
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      unsubscribe();
      reject(
        new Error(
          `store did not reach 'ready' within ${READY_TIMEOUT_MS}ms ` +
            `(stuck at '${store.getSnapshot().status}')`,
        ),
      );
    }, READY_TIMEOUT_MS);
    const unsubscribe = store.subscribe(() => {
      if (store.getSnapshot().status === 'ready') {
        clearTimeout(timer);
        unsubscribe();
        resolve();
      }
    });
  });
}

/**
 * The part of the snapshot a command is expected to leave alone.
 *
 * Four fields are excluded, each for a reason a provider should not have to work around:
 *
 *  - `errors`, because a failing command is *supposed* to append to it;
 *  - `status`, because a provider may legitimately flicker through `reconnecting` while a
 *    command is in flight;
 *  - `daemon` and `usage` by *value*, because they are telemetry. A real transport streams
 *    resident set, terminal count and quota windows on their own schedule, so requiring
 *    them to be frozen across an unrelated command would fail a provider on `memoryBytes`
 *    having ticked — while everything the assertion actually means, the session and
 *    project state, was untouched. A provider could pass by holding its metrics still
 *    between commands; it should not have to distort its design to satisfy a test.
 *
 * Dropping the values entirely gave something up, though, and `daemonKeys` gets part of it
 * back: a provider whose *failing* `selectTab` also replaced the metrics object with
 * something of a different shape was caught before and would not be otherwise. Which keys
 * exist is structure — a streaming provider moves every number inside them and stays green.
 *
 * `usage` is deliberately *not* treated the same way. Asking whether it still has entries
 * looks structural and is not: an empty quota list is a value like any other, and it is the
 * value `emptySnapshot()` ships. A provider that reaches `ready` on its handshake and
 * delivers its first quota sample on the next frame — a daemon polling quota separately
 * from the connection, which is an ordinary design — would fail on a field no session
 * assertion touches. That is the timing coupling this suite spent a round removing, so it
 * does not come back for a partial gain.
 *
 * `agentStatus` joins them for the same reason and a sharper one. Status arrives from the
 * daemon on its own schedule — a hook fires while a `selectTab` is in flight and the pane
 * list legitimately moves under it — so requiring it to be frozen across an unrelated
 * command would fail a correct provider on a dot that ticked. It is the one field here a
 * *command* is never supposed to touch, which is exactly why holding it still is the
 * transport's business rather than the contract's.
 *
 * What is left uncovered: a value replaced by a wrong value of the same shape, any change
 * to `usage` at all, and a command that silently rewrites `agentStatus`. All three are
 * W5's to hold, and it has been told so — `agentStatus.test.ts` is where the fold is held
 * to replacing one pane and leaving the rest alone.
 */
export interface Observable
  extends Omit<StoreSnapshot, 'errors' | 'status' | 'daemon' | 'usage' | 'agentStatus'> {
  readonly daemonKeys: readonly string[];
}

export function observable(snapshot: StoreSnapshot): Observable {
  return {
    nav: snapshot.nav,
    projects: snapshot.projects,
    activeProjectId: snapshot.activeProjectId,
    tabs: snapshot.tabs,
    activeTab: snapshot.activeTab,
    launchers: snapshot.launchers,
    daemonKeys: Object.keys(snapshot.daemon).sort(),
  };
}
