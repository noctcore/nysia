import type { PaneKey } from '../generated/PaneKey';
import { StoreCommandError, type StoreCommandName, type StoreError } from './errors';
import { emptySnapshot } from './types';
import type {
  LauncherId,
  NavSection,
  ProjectId,
  Store,
  StoreSnapshot,
  Tab,
  WindowControls,
} from './types';

/**
 * A deliberately daemon-shaped provider, built to break the contract suite.
 *
 * `MockStore` is synchronous, so a contract suite written only against it drifts into
 * assuming synchronous behaviour without anyone noticing — which is exactly what happened
 * to the first version of `storeContract.ts`, and why a reviewer's socket-shaped provider
 * failed eight of its fourteen cases. This store exists so that drift fails here, in wave
 * 1, rather than in wave 2 when W5 is holding a socket.
 *
 * It is hostile in four specific ways, each matching something a real transport does:
 *
 *  1. **The first frame arrives on a timer.** Nothing is on screen at construction and
 *     `status` is `connecting`, like a client that has not finished its `hello` handshake.
 *  2. **Every command emits two frames** — an acknowledgement and then the state — so a
 *     suite that pins a notification count to exactly one cannot pass.
 *  3. **Even a no-op command allocates a fresh snapshot.** Referential equality across a
 *     command is therefore never available; only stability *between* notifications is.
 *  4. **Commands settle a turn late**, so anything asserted before the returned promise
 *     resolves is asserted against stale state.
 *  5. **Telemetry ticks on every frame.** Resident set and quota windows move whether or
 *     not a command touched anything, which is what a transport streaming metrics looks
 *     like — and which a contract comparing whole snapshots would fail on `memoryBytes`
 *     alone while the session state it meant to check was untouched.
 *
 * It is not a fixture for component tests and it is not shipped to the window. Its only
 * job is to be run through `describeStoreContract` alongside the mock.
 */
export class AsyncProbeStore implements Store {
  #snapshot: StoreSnapshot = emptySnapshot('connecting');
  readonly #listeners = new Set<() => void>();
  #nextTab = 1;
  #nextError = 1;
  #frame = 0;

  constructor(seed: StoreSnapshot, connectDelayMs = 0) {
    setTimeout(() => {
      this.#emit(() => ({ ...seed, status: 'ready', errors: [] }));
    }, connectDelayMs);
  }

  getSnapshot = (): StoreSnapshot => this.#snapshot;

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  selectNav = async (section: NavSection): Promise<void> => {
    await this.#ack();
    this.#emit((current) => ({ ...current, nav: section }));
  };

  selectProject = async (id: ProjectId): Promise<void> => {
    await this.#ack();
    if (!this.#snapshot.projects.some((project) => project.id === id)) {
      throw this.#fail('selectProject', `No project ${id} is open.`);
    }
    this.#emit((current) => ({ ...current, activeProjectId: id }));
  };

  selectTab = async (paneKey: PaneKey): Promise<void> => {
    await this.#ack();
    if (!this.#snapshot.tabs.some((tab) => tab.paneKey === paneKey)) {
      throw this.#fail('selectTab', `Session ${paneKey} is no longer open.`);
    }
    this.#emit((current) => ({ ...current, activeTab: paneKey }));
  };

  closeTab = async (paneKey: PaneKey): Promise<void> => {
    await this.#ack();
    if (!this.#snapshot.tabs.some((tab) => tab.paneKey === paneKey)) {
      throw this.#fail('closeTab', `Session ${paneKey} is no longer open.`);
    }
    this.#emit((current) => {
      const tabs = current.tabs.filter((tab) => tab.paneKey !== paneKey);
      if (current.activeTab !== paneKey) {
        return { ...current, tabs };
      }
      // Deliberately *not* the mock's neighbour rule: a daemon may activate whatever it
      // likes. If the contract ever re-acquires an opinion about which one, this fails.
      return { ...current, tabs, activeTab: tabs[0]?.paneKey ?? null };
    });
  };

  openTab = async (launcher: LauncherId): Promise<void> => {
    await this.#ack();
    const item = this.#snapshot.launchers
      .flatMap((group) => group.items)
      .find((candidate) => candidate.id === launcher);
    if (!item) {
      throw this.#fail('openTab', `No launcher ${launcher} is available.`);
    }
    this.#emit((current) => {
      const paneKey: PaneKey = `probe_${this.#nextTab}:leaf_1`;
      const tab: Tab = {
        paneKey,
        handle: `sess_probe-${this.#nextTab}`,
        kind: item.kind,
        title: item.label,
      };
      this.#nextTab += 1;
      // Prepended, not appended — the contract must find the new tab by identity.
      return { ...current, tabs: [tab, ...current.tabs], activeTab: paneKey };
    });
  };

  /**
   * Settles a turn late and through two frames, like every other verb here.
   *
   * It resolves to a *refusal* rather than to idle, which is the hostile choice and the
   * deliberate one: the contract says `addProject` resolves on every answer the daemon can
   * give, so a probe that only ever reached `idle` would let a suite pass that had quietly
   * assumed a refusal rejects. The mock takes the other branch, so between them both are
   * exercised.
   */
  addProject = async (): Promise<void> => {
    await this.#ack();
    this.#emit((current) => ({
      ...current,
      addProject: {
        phase: 'refused',
        code: 'many_repositories',
        message: 'that folder is not a git repository, but 3 of the folders in it are',
        nextSteps: ['Pick one of these and register that folder instead: nysia, orca, valve.'],
      },
    }));
  };

  dismissAddProject = async (): Promise<void> => {
    await this.#ack();
    this.#emit((current) => ({ ...current, addProject: { phase: 'idle' } }));
  };

  dismissError = async (id: string): Promise<void> => {
    await this.#ack();
    this.#emit((current) => ({
      ...current,
      errors: current.errors.filter((error) => error.id !== id),
    }));
  };

  readonly window: WindowControls = {
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
  };

  /** The acknowledgement frame: a fresh snapshot carrying no new information. */
  async #ack(): Promise<void> {
    await Promise.resolve();
    this.#emit((current) => ({ ...current }));
  }

  #fail(command: StoreCommandName, message: string): StoreCommandError {
    const id = `probe_err_${this.#nextError}`;
    this.#nextError += 1;
    const error: StoreError = { id, command, message, at: Date.now() };
    this.#emit((current) => ({ ...current, errors: [...current.errors, error] }));
    return new StoreCommandError(command, message, id);
  }

  /**
   * Always allocates, always notifies, and always moves the telemetry — the opposite of
   * the mock's short-circuit.
   */
  #emit(next: (current: StoreSnapshot) => StoreSnapshot): void {
    this.#snapshot = this.#tick(next(this.#snapshot));
    for (const listener of [...this.#listeners]) {
      listener();
    }
  }

  /** Metrics that move on their own schedule, as a transport streaming them would. */
  #tick(snapshot: StoreSnapshot): StoreSnapshot {
    this.#frame += 1;
    return {
      ...snapshot,
      daemon: {
        ...snapshot.daemon,
        memoryBytes: snapshot.daemon.memoryBytes + this.#frame * 4096,
      },
      usage: snapshot.usage.map((window) => ({
        ...window,
        percentLeft: Math.max(0, window.percentLeft - 0.01),
      })),
    };
  }
}
