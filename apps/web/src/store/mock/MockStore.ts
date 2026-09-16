import type { AgentStatusChange } from '../../generated/AgentStatusChange';
import type { PaneKey } from '../../generated/PaneKey';
import type { SessionHandle } from '../../generated/SessionHandle';
import { agentNotifications, type AgentNotificationSink } from '../agentNotifications';
import { applyAgentStatus, retainAgentStatus } from '../agentStatus';
import { StoreCommandError, type StoreCommandName, type StoreError } from '../errors';
import type {
  LauncherId,
  NavSection,
  ProjectId,
  Store,
  StoreSnapshot,
  Tab,
  WindowControls,
} from '../types';
import { createSeedSnapshot } from './seed';

/**
 * The wave-1 store: the design mock's seed data, plus the state transitions the chrome
 * needs to be operable rather than a screenshot.
 *
 * It implements the same contract W5's daemon-backed provider will — commands are async
 * and can fail, the snapshot is immutable and stable between notifications, and no
 * component knows which provider it is talking to. `storeContract.ts` is the executable
 * statement of that contract; this store and `AsyncProbeStore` both run it, and the probe
 * is the one that proves the contract does not quietly assume this store's shape.
 *
 * Every method is an arrow property: `useSyncExternalStore` receives `subscribe` and
 * `getSnapshot` detached from the object.
 */
export class MockStore implements Store {
  #snapshot: StoreSnapshot;
  readonly #listeners = new Set<() => void>();
  readonly #notifications: AgentNotificationSink;
  #nextTab: number;
  #nextError = 1;

  constructor(
    initial: StoreSnapshot = createSeedSnapshot(),
    options: { readonly notifications?: AgentNotificationSink } = {},
  ) {
    this.#snapshot = initial;
    this.#nextTab = initial.tabs.length + 1;
    // The window's sink by default, its own on request. Two mock stores share the module
    // one otherwise, and `MockStore.test.ts` asserts that two stores have independent
    // state — a shared notice list would make that assertion quietly false for one field.
    this.#notifications = options.notifications ?? agentNotifications;
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
    if (!this.#snapshot.projects.some((project) => project.id === id)) {
      throw this.#fail('selectProject', `No project ${id} is open.`);
    }
    this.#update((current) =>
      current.activeProjectId === id ? current : { ...current, activeProjectId: id },
    );
  };

  selectTab = async (paneKey: PaneKey): Promise<void> => {
    if (!this.#snapshot.tabs.some((tab) => tab.paneKey === paneKey)) {
      throw this.#fail('selectTab', `Session ${paneKey} is no longer open.`);
    }
    this.#update((current) =>
      current.activeTab === paneKey ? current : { ...current, activeTab: paneKey },
    );
  };

  closeTab = async (paneKey: PaneKey): Promise<void> => {
    if (!this.#snapshot.tabs.some((tab) => tab.paneKey === paneKey)) {
      throw this.#fail('closeTab', `Session ${paneKey} is no longer open.`);
    }
    this.#update((current) => {
      const index = current.tabs.findIndex((tab) => tab.paneKey === paneKey);
      const tabs = current.tabs.filter((tab) => tab.paneKey !== paneKey);
      // A `PaneKey` is durable and therefore reusable, so a row that outlived its tab would
      // eventually describe a different session under the same key.
      const agentStatus = retainAgentStatus(
        current.agentStatus,
        new Set(tabs.map((tab) => tab.paneKey)),
      );
      if (current.activeTab !== paneKey) {
        return { ...current, tabs, agentStatus };
      }
      // Closing the focused tab hands focus to its right-hand neighbour, or to the new
      // last tab when it was the rightmost — the behaviour every tabbed editor has. This
      // is *this* store's policy: the contract only requires that whatever ends up active
      // exists, because a daemon is entitled to choose differently.
      const next = tabs[Math.min(index, tabs.length - 1)];
      return { ...current, tabs, agentStatus, activeTab: next?.paneKey ?? null };
    });
  };

  openTab = async (launcher: LauncherId): Promise<void> => {
    const item = this.#snapshot.launchers
      .flatMap((group) => group.items)
      .find((candidate) => candidate.id === launcher);
    if (!item) {
      throw this.#fail('openTab', `No launcher ${launcher} is available.`);
    }
    this.#update((current) => {
      const paneKey: PaneKey = `tab_${this.#nextTab}:leaf_1`;
      const handle: SessionHandle = `sess_${mockUuid(this.#nextTab)}`;
      this.#nextTab += 1;
      const tab: Tab = { paneKey, handle, kind: item.kind, title: item.label };
      return { ...current, tabs: [...current.tabs, tab], activeTab: paneKey };
    });
  };

  /**
   * Deliver a status change, as the daemon's subscription will in wave C.
   *
   * **Not on `Store`.** Nothing in the chrome pushes status — it arrives, and a component
   * that could call this would be a component that could invent one. It is on the class so
   * a test can drive the dots and the notices without a socket, which is the whole reason
   * the store boundary exists.
   *
   * The fold runs before the sink is offered the change, so a listener woken by a notice
   * finds the dot already moved. The sink decides for itself whether a notice happens; this
   * never inspects `notify`.
   */
  receiveAgentStatus = (change: AgentStatusChange): void => {
    // Resolved here, and now, because a `PaneKey` outlives the session in it: a notice that
    // looked its own title up later could be relabelled with whatever reused the key. The
    // pane key is the fallback for a pane no tab is showing — an honest name rather than an
    // invented one.
    const pane = change.status.lead.pane;
    const session = this.#snapshot.tabs.find((tab) => tab.paneKey === pane)?.title ?? pane;

    this.#update((current) => ({
      ...current,
      agentStatus: applyAgentStatus(current.agentStatus, change),
    }));
    this.#notifications.report(change, session);
  };

  /**
   * The mock has no filesystem, so browsing resolves the way a cancelled picker does.
   *
   * Not a stub that throws and not one that invents a project: the mock stands in for a
   * daemon, and inventing a registration would mean inventing a `ProjectId`, which §3.1
   * derives from a canonical path on a real disk. "The user closed the dialog" is a real
   * outcome of this command and the only one reachable without one.
   *
   * A test that wants to see the panel builds a snapshot with the outcome already in it —
   * `addProject` is snapshot state precisely so that is possible without a picker.
   */
  addProject = async (): Promise<void> => {
    this.#update((current) =>
      current.addProject.phase === 'idle'
        ? current
        : { ...current, addProject: { phase: 'idle' } },
    );
  };

  dismissAddProject = async (): Promise<void> => {
    this.#update((current) =>
      current.addProject.phase === 'idle'
        ? current
        : { ...current, addProject: { phase: 'idle' } },
    );
  };

  dismissError = async (id: string): Promise<void> => {
    this.#update((current) => {
      const errors = current.errors.filter((error) => error.id !== id);
      return errors.length === current.errors.length ? current : { ...current, errors };
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

  /**
   * Record the failure, then hand back the rejection to throw.
   *
   * Recording first is the contract: by the time a caller sees the rejection, the notice
   * is already in the snapshot. A provider that rejected first and recorded on the next
   * frame would leave a window in which the UI knows something failed and has nothing to
   * show for it.
   */
  #fail(command: StoreCommandName, message: string): StoreCommandError {
    const id = `err_${this.#nextError}`;
    this.#nextError += 1;
    const error: StoreError = { id, command, message, at: Date.now() };
    this.#update((current) => ({ ...current, errors: [...current.errors, error] }));
    return new StoreCommandError(command, message, id);
  }

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
