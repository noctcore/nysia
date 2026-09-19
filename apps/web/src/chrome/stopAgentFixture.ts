import type { AgentState } from '../generated/AgentState';
import type { PaneKey } from '../generated/PaneKey';
import { memoryStorage } from '../settings/memoryStorage';
import { statusOf } from '../store/agentStatusFixture';
import { routeCommands } from '../store/commands';
import { MockStore } from '../store/mock/MockStore';
import { createSeedSnapshot } from '../store/mock/seed';
import type { Tab } from '../store/types';
import { createStopGate, type StopGate } from './stopAgent';

/**
 * A strip's worth of sessions behind a real store, with the stop gate in front of it.
 *
 * A plain module rather than a `.test.ts`, like `store/agentStatusFixture.ts`: the gate's
 * suite and the dialog's both need it, and the claim both make is about the **store** —
 * whether the session is still there after Cancel — so the gate is wired to the mock
 * provider through `routeCommands`, exactly as `useCommands()` wires it in the window,
 * rather than to a spy that would only prove a function was not called.
 */

/** The fixed clock every scene is read against, so a row's age is the case's decision. */
export const NOW = 1_800_000_000_000;

const SEED = createSeedSnapshot(NOW);

function seededTab(index: number): Tab {
  const tab = SEED.tabs[index];
  if (tab === undefined) {
    throw new Error(`the seed has no tab at ${index}`);
  }
  return tab;
}

/** The seed's first tab: an agent. */
export const AGENT = seededTab(0);
/** The seed's second tab: a shell. */
export const SHELL = seededTab(1);
/** The seed's third tab: another agent. */
export const OTHER_AGENT = seededTab(2);

export interface Scene {
  readonly store: MockStore;
  readonly gate: StopGate;
  readonly storage: Storage;
  isOpen(tab: Tab): boolean;
}

/**
 * A scene whose panes report exactly `rows` — anything not named has no status row.
 *
 * `observedAt` defaults to {@link NOW}, so a row is fresh unless a case says otherwise.
 */
export function stopScene(
  rows: ReadonlyArray<readonly [PaneKey, AgentState, number?]> = [],
  storage: Storage = memoryStorage(),
): Scene {
  const store = new MockStore({
    ...SEED,
    agentStatus: rows.map(([pane, state, observedAt = NOW]) =>
      statusOf(pane, state, { observedAt }),
    ),
  });
  const gate = createStopGate(routeCommands(store), { storage: () => storage, now: () => NOW });
  return {
    store,
    gate,
    storage,
    isOpen: (tab) => store.getSnapshot().tabs.some((open) => open.paneKey === tab.paneKey),
  };
}

/**
 * Let every command in flight settle.
 *
 * `MockStore` happens to apply a close synchronously, and the daemon-backed provider does
 * not. Asserting that a session survived without waiting would pass against a gate that
 * closed it one tick later — the green-while-wrong shape this suite exists to avoid.
 */
export function settled(): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, 0);
  });
}
