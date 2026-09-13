import type { PaneKey } from '../../generated/PaneKey';
import type { SessionHandle } from '../../generated/SessionHandle';
import type {
  LauncherId,
  NavSection,
  ProjectId,
  Store,
  StoreSnapshot,
  Tab,
  WindowControls,
} from '../types';
import { SEED_SNAPSHOT } from './seed';

/**
 * The wave-1 store: the design mock's seed data, plus the state transitions the chrome
 * needs to be operable rather than a screenshot.
 *
 * It deliberately implements the same contract W5's daemon-backed provider will — commands
 * are async, the snapshot is immutable and referentially stable, and no component knows
 * which provider it is talking to. `storeContract.ts` is the executable statement of that
 * contract and both providers run it.
 *
 * Every method is an arrow property: `useSyncExternalStore` receives `subscribe` and
 * `getSnapshot` detached from the object.
 */
export class MockStore implements Store {
  #snapshot: StoreSnapshot;
  readonly #listeners = new Set<() => void>();
  #nextTab: number;

  constructor(initial: StoreSnapshot = SEED_SNAPSHOT) {
    this.#snapshot = initial;
    this.#nextTab = initial.tabs.length + 1;
  }

  getSnapshot = (): StoreSnapshot => this.#snapshot;

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  selectNav = async (section: NavSection): Promise<void> => {
    this.#update((current) =>
      current.nav === section ? current : { ...current, nav: section },
    );
  };

  selectProject = async (id: ProjectId): Promise<void> => {
    this.#update((current) =>
      current.activeProjectId === id || !current.projects.some((p) => p.id === id)
        ? current
        : { ...current, activeProjectId: id },
    );
  };

  selectTab = async (paneKey: PaneKey): Promise<void> => {
    this.#update((current) =>
      current.activeTab === paneKey || !current.tabs.some((t) => t.paneKey === paneKey)
        ? current
        : { ...current, activeTab: paneKey },
    );
  };

  closeTab = async (paneKey: PaneKey): Promise<void> => {
    this.#update((current) => {
      const index = current.tabs.findIndex((t) => t.paneKey === paneKey);
      if (index === -1) {
        return current;
      }
      const tabs = current.tabs.filter((t) => t.paneKey !== paneKey);
      if (current.activeTab !== paneKey) {
        return { ...current, tabs };
      }
      // Closing the focused tab hands focus to its right-hand neighbour, or to the new
      // last tab when it was the rightmost — the behaviour every tabbed editor has.
      const next = tabs[Math.min(index, tabs.length - 1)];
      return { ...current, tabs, activeTab: next?.paneKey ?? null };
    });
  };

  openTab = async (launcher: LauncherId): Promise<void> => {
    this.#update((current) => {
      const item = current.launchers
        .flatMap((group) => group.items)
        .find((candidate) => candidate.id === launcher);
      if (!item) {
        return current;
      }
      const paneKey: PaneKey = `tab_${this.#nextTab}:leaf_1`;
      const handle: SessionHandle = `sess_${mockUuid(this.#nextTab)}`;
      this.#nextTab += 1;
      const tab: Tab = { paneKey, handle, kind: item.kind, title: item.label };
      return { ...current, tabs: [...current.tabs, tab], activeTab: paneKey };
    });
  };

  /**
   * No-ops until W5 wires them to the Tauri window.
   *
   * They still live on the store rather than in the titlebar, because `apps/web` may not
   * import `@tauri-apps` outside `src/transport` (D-1, D-2) — so the affordance has to be
   * a command from day one or the component would have to change to gain one.
   */
  readonly window: WindowControls = {
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
  };

  #update(next: (current: StoreSnapshot) => StoreSnapshot): void {
    const updated = next(this.#snapshot);
    if (updated === this.#snapshot) {
      return;
    }
    this.#snapshot = updated;
    for (const listener of [...this.#listeners]) {
      listener();
    }
  }
}

/** Shaped like a uuid so the handles look like the real thing; not one, and never wired. */
function mockUuid(seq: number): string {
  const tail = seq.toString(16).padStart(12, '0');
  return `00000000-0000-4000-8000-${tail}`;
}

export function createMockStore(initial?: StoreSnapshot): Store {
  return new MockStore(initial);
}
