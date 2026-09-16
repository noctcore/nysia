import type { AgentStatusChange } from '../../generated/AgentStatusChange';
import type { PaneKey } from '../../generated/PaneKey';
import type { SessionHandle } from '../../generated/SessionHandle';
import type { Issue } from '../../tasks/issue';
import { isTasksBusy } from '../../tasks/tasks';
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
import { createSeedIssues, createSeedSnapshot } from './seed';

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
  /**
   * What {@link refreshTasks} answers with.
   *
   * Held beside the snapshot rather than inside it because the snapshot starts `idle`: the
   * screen has to *ask*, and a seed that arrived already loaded would hide a screen that
   * never asks. Built once per store so the `7 days ago` on each row stays put while the
   * window is open, for `seed.ts`'s reason — the ages are offsets from construction.
   */
  readonly #issues: readonly Issue[] = createSeedIssues();
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
      current.activeProjectId === id
        ? current
        : // The issues belong to the project that was showing, and so does the line saying
          // what the last `Start →` did. Carrying either across would put one repository's
          // work under another's name, and `Start →` would then derive a branch for the
          // wrong worktree.
          {
            ...current,
            activeProjectId: id,
            tasks: { phase: 'idle' },
            taskStart: { phase: 'idle' },
          },
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

  /**
   * The mock has no daemon, so it answers with the design mock's own issues.
   *
   * The same stance `seed.ts` takes everywhere else: what the mock shows is transcribed from
   * `docs/design/Nysia-ADE.dc.html` rather than invented, so the screen this store drives is
   * the screen the design draws. A provider that refused instead would make the mock useless
   * for exactly the thing it exists for.
   *
   * It goes through `loading` first, even though nothing is awaited between the two frames.
   * A store that jumped straight to `loaded` would let a `↻` that is only disabled while
   * busy pass every test here and stay live in front of a daemon.
   */
  refreshTasks = async (): Promise<void> => {
    if (this.#snapshot.activeProjectId === null || isTasksBusy(this.#snapshot.tasks)) {
      return;
    }
    this.#update((current) => ({ ...current, tasks: { phase: 'loading' } }));
    this.#update((current) => ({
      ...current,
      tasks: { phase: 'loaded', issues: this.#issues },
    }));
  };

  /**
   * There is no worktree to create and no daemon to create it, so this refuses.
   *
   * Deliberately a refusal rather than a tab appearing out of nowhere: `Start →` creates a
   * worktree on a real disk, and a mock that pretended to would be the one affordance in
   * this store that looks live and does nothing. The message says which provider said so,
   * because the alternative is somebody debugging a daemon that was never involved.
   */
  startTask = async (issue: Issue): Promise<void> => {
    throw this.#fail(
      'startTask',
      `Nothing can be started from the mock store — #${issue.number} needs a daemon to create a worktree in.`,
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
