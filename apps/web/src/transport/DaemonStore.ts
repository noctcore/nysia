import type { PaneKey } from '../generated/PaneKey';
import type { SessionHandle } from '../generated/SessionHandle';
import type { SessionKind } from '../generated/SessionKind';
import type { SessionSummary as WireSession } from '../generated/SessionSummary';
import type { ShellProfile } from '../generated/ShellProfile';
import { StoreCommandError, type StoreCommandName, type StoreError } from '../store/errors';
import { SEED_ACTIVE_PROJECT, SEED_PROJECT_NAMES } from '../store/mock/seed';
import {
  emptySnapshot,
  type LauncherGroup,
  type NavSection,
  type Project,
  type ProjectId,
  type Store,
  type StoreSnapshot,
  type Tab,
  type WindowControls,
} from '../store/types';
import { describeFailure, isRetryable, type DaemonBridge } from './bridge';
import type { StreamId } from './frames';
import { streamIdFor } from './terminals';
import type { TerminalRouter } from './terminals';

/**
 * The daemon-backed provider, behind W3's `Store` interface.
 *
 * Swapping this in is one line in `main.tsx`, and not one component file changes — which is
 * what the interface was built for. The rules it has to honour are stated in
 * `store/types.ts` and asserted in `store/storeContract.ts`; this file runs that same suite.
 *
 * ## What comes from the daemon, and what does not
 *
 * **Sessions and tabs are real.** `session_list` is the authority, and it is what makes D-1
 * observable: a window that has just started, attaching to a daemon that has been running
 * for hours, gets back the sessions that outlived the last window.
 *
 * **Projects are not, yet.** `nysia-proto` has no project or worktree verb — under §3 they
 * live in the daemon's JSON settings store, and the verb to read that store is deferred
 * with a named owner. The alternative that was considered and rejected was deriving a
 * project from each distinct session `cwd`: that invents a domain object out of an
 * unrelated signal, which is precisely what D-5 deleted the local task model to stop. So
 * the project *list* is W3's seed, held client-side and marked as such, and the sessions
 * shown against the active project are the daemon's real ones rather than seeded
 * placeholders. Nothing on screen claims to be running unless it is.
 *
 * **Launchers are derived**, from `ShellProfile`'s four variants. Which shells exist on the
 * machine is properly the daemon's answer, but the four *kinds* are on the wire already,
 * and deriving a menu from a closed enum is reading the protocol rather than inventing.
 *
 * ## Selection lives here
 *
 * Which tab is focused and which rail destination is showing are properties of *this
 * window*, not of the daemon — two windows on one daemon do not share a cursor. So
 * `selectNav`, `selectTab` and `selectProject` never leave the process, and inventing
 * daemon verbs for them would have made the daemon wrong about something it cannot know.
 *
 * ## Every failure reaches the user
 *
 * `runCommand` only surfaces `StoreCommandError`; anything else reaches `console.error`,
 * where nobody sees it. A socket closing mid-command is the likeliest real failure in this
 * whole file, so every path out of it — including the reconnect loop — goes through
 * {@link DaemonStore.fail}, which records *and* rejects.
 */
export class DaemonStore implements Store {
  #snapshot: StoreSnapshot = emptySnapshot('connecting');
  readonly #listeners = new Set<() => void>();
  readonly #bridge: DaemonBridge;
  readonly #router: TerminalRouter;
  readonly #retryDelaysMs: readonly number[];

  /** Session handles in the order the daemon first reported them — see `streamIdFor`. */
  #order: SessionHandle[] = [];
  #nextErrorId = 1;
  #disposed = false;

  constructor(options: {
    readonly bridge: DaemonBridge;
    readonly router: TerminalRouter;
    /**
     * Backoff between reconnect attempts, in milliseconds.
     *
     * A list rather than a formula so a test can pass `[0]` and a real window can back off
     * without either of them needing a clock they can control.
     */
    readonly retryDelaysMs?: readonly number[];
  }) {
    this.#bridge = options.bridge;
    this.#router = options.router;
    this.#retryDelaysMs = options.retryDelaysMs ?? [250, 500, 1000, 2000, 5000];
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
      throw this.fail('selectProject', `No project ${id} is open.`);
    }
    this.#update((current) =>
      current.activeProjectId === id ? current : { ...current, activeProjectId: id },
    );
  };

  selectTab = async (paneKey: PaneKey): Promise<void> => {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      throw this.fail('selectTab', `Session ${paneKey} is no longer open.`);
    }
    this.#update((current) =>
      current.activeTab === paneKey ? current : { ...current, activeTab: paneKey },
    );
  };

  closeTab = async (paneKey: PaneKey): Promise<void> => {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      throw this.fail('closeTab', `Session ${paneKey} is no longer open.`);
    }

    try {
      await this.#bridge.invoke('session_close', { handle: tab.handle });
    } catch (cause) {
      throw this.fail('closeTab', describeFailure(cause));
    }

    const stream = streamIdFor(tab.handle, this.#order);
    if (stream !== null) {
      this.#router.close(stream);
    }
    await this.#refresh();
  };

  openTab = async (launcher: string): Promise<void> => {
    const item = this.#snapshot.launchers
      .flatMap((group) => group.items)
      .find((candidate) => candidate.id === launcher);
    if (!item) {
      throw this.fail('openTab', `No launcher ${launcher} is available.`);
    }

    let handle: SessionHandle;
    try {
      handle = await this.#bridge.invoke<SessionHandle>('session_create', {
        request: {
          kind: item.kind,
          paneKey: null,
          profile: profileFor(launcher),
          cwd: null,
          envOverrides: {},
          cols: 80,
          rows: 24,
        },
      });
    } catch (cause) {
      // The daemon's own message and next step, verbatim — "pwsh is not on PATH" and what
      // to do about it, rather than a menu that closed and a tab that never appeared.
      throw this.fail('openTab', describeFailure(cause));
    }

    await this.#refresh();
    const opened = this.#snapshot.tabs.find((tab) => tab.handle === handle);
    if (opened) {
      this.#update((current) => ({ ...current, activeTab: opened.paneKey }));
    }
  };

  dismissError = async (id: string): Promise<void> => {
    this.#update((current) => {
      const errors = current.errors.filter((error) => error.id !== id);
      return errors.length === current.errors.length ? current : { ...current, errors };
    });
  };

  get window(): WindowControls {
    return this.#bridge.window;
  }

  /** The surface a pane draws into, or `null` before the daemon has named the session. */
  surfaceStream(paneKey: PaneKey): StreamId | null {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    return tab ? streamIdFor(tab.handle, this.#order) : null;
  }

  /** The router, for the pane component that mounts surfaces. */
  get terminals(): TerminalRouter {
    return this.#router;
  }

  /**
   * Forward what the user typed to the session in `paneKey`.
   *
   * Not a `Store` command, and deliberately so: `StoreCommandName` enumerates the commands
   * a *notice* can name, and a keystroke is not one of them. One notice per character typed
   * while the daemon is down would bury every other message in the list, and the disconnect
   * that caused it already has a notice of its own.
   *
   * Rejects rather than recording, so the caller decides. `TerminalView` swallows it.
   */
  async sendInput(paneKey: PaneKey, text: string): Promise<void> {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      throw new Error(`Session ${paneKey} is no longer open.`);
    }
    await this.#bridge.invoke('terminal_send', {
      request: { handle: tab.handle, text, enter: false, interrupt: false },
    });
  }

  /**
   * Tell the daemon a pane changed size, in cells.
   *
   * Same reasoning as {@link sendInput}: a resize that failed because the socket dropped is
   * the disconnect's notice, not its own.
   */
  async resize(paneKey: PaneKey, cols: number, rows: number): Promise<void> {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      return;
    }
    // The surface has already sized itself — it is what measured the host — so this only
    // carries the answer to the daemon, which owns the PTY.
    await this.#bridge.invoke('terminal_resize', {
      request: { handle: tab.handle, cols, rows },
    });
  }

  /**
   * Connect, load, and keep reconnecting for as long as the window lives.
   *
   * Started once, from {@link createDaemonStore}. It never throws: a store whose connect
   * loop rejected would leave the chrome rendering `connecting` forever with nothing said
   * about why, which is the state this whole file exists to avoid.
   */
  async run(): Promise<void> {
    let attempt = 0;
    while (!this.#disposed) {
      const connected = await this.#connectOnce();
      if (connected) {
        attempt = 0;
        // Resolves when the socket drops — a blocking command rather than an `emit`,
        // because tauri#12724 leaks on sustained emits and a second mechanism for one
        // edge is not worth the leak.
        await this.#bridge.invoke('daemon_watch').catch(() => undefined);
        if (this.#disposed) {
          return;
        }
        this.#update((current) => ({ ...current, status: 'reconnecting' }));
      } else {
        attempt += 1;
      }

      const delay =
        this.#retryDelaysMs[Math.min(attempt, this.#retryDelaysMs.length - 1)] ?? 1000;
      await sleep(delay);
    }
  }

  /** Stop reconnecting and release every surface. */
  dispose(): void {
    this.#disposed = true;
    this.#router.dispose();
  }

  /**
   * Record a failure and produce the rejection that goes with it.
   *
   * Both, in that order, and before the promise settles — the contract in `store/errors.ts`
   * is explicit that rejecting without recording loses the message and recording without
   * rejecting leaves a caller unable to sequence.
   */
  fail(command: StoreCommandName, message: string): StoreCommandError {
    const id = `err_${this.#nextErrorId}`;
    this.#nextErrorId += 1;
    const error: StoreError = { id, command, message, at: Date.now() };
    this.#update((current) => ({ ...current, errors: [...current.errors, error] }));
    return new StoreCommandError(command, message, id);
  }

  /** One connection attempt. `true` once the first snapshot is on screen. */
  async #connectOnce(): Promise<boolean> {
    try {
      await this.#bridge.invoke('daemon_connect');
    } catch (cause) {
      // `failed` rather than `reconnecting` once the daemon has said something that waiting
      // will not change — a protocol mismatch never resolves itself, and a window that kept
      // saying "reconnecting" would be lying about what it is waiting for.
      const status = isRetryable(cause) ? 'reconnecting' : 'failed';
      this.#update((current) => ({ ...current, status }));
      this.#recordOnce('openTab', describeFailure(cause));
      return false;
    }

    try {
      await this.#bridge.attachChannel((delivery) => {
        const unreadable = this.#router.deliver(delivery);
        if (unreadable) {
          // There is no delimiter to resynchronise on, so the channel is finished. Dropping
          // the connection is what gets a fresh one.
          this.#recordOnce('openTab', unreadable.message);
        }
      });
      await this.#refresh();
    } catch (cause) {
      this.#update((current) => ({ ...current, status: 'reconnecting' }));
      this.#recordOnce('openTab', describeFailure(cause));
      return false;
    }

    this.#update((current) => ({ ...current, status: 'ready' }));
    return true;
  }

  /** Re-read the session list and rebuild everything derived from it. */
  async #refresh(): Promise<void> {
    const sessions = await this.#bridge.invoke<WireSession[]>('session_list');

    // Append-only, so a session keeps the stream id it was first given. Rebuilding the
    // order on every refresh would renumber every stream behind a closed pane and route
    // output into the wrong terminal.
    for (const session of sessions) {
      if (!this.#order.includes(session.handle)) {
        this.#order.push(session.handle);
      }
    }

    const tabs = sessions.map(toTab);
    this.#update((current) => {
      const activeTab =
        tabs.find((tab) => tab.paneKey === current.activeTab)?.paneKey ??
        tabs[0]?.paneKey ??
        null;
      const activeProjectId =
        current.activeProjectId ?? SEED_ACTIVE_PROJECT;
      return {
        ...current,
        tabs,
        activeTab,
        projects: projectsWith(sessions, activeProjectId),
        activeProjectId,
        launchers: LAUNCHERS,
        daemon: { ...current.daemon, terminalCount: sessions.length },
      };
    });
  }

  /** Record a connection failure, but not the same one over and over. */
  #recordOnce(command: StoreCommandName, message: string): void {
    // A reconnect loop that appended a notice per attempt would bury everything else in the
    // list within a minute of the daemon being down.
    if (this.#snapshot.errors.some((error) => error.message === message)) {
      return;
    }
    this.fail(command, message);
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

/** One wire session as the chrome sees it. */
function toTab(session: WireSession): Tab {
  return {
    paneKey: session.paneKey,
    handle: session.handle,
    kind: session.kind,
    title: session.title,
  };
}

/**
 * The project list, with the daemon's real sessions against the active one.
 *
 * The list itself is W3's seed until the projects verb lands. What is deliberately *not*
 * seeded is the session rows: showing four invented sessions beside a sidebar that claims
 * to reflect a live daemon is how a user learns to distrust everything else on the screen.
 */
function projectsWith(
  sessions: readonly WireSession[],
  activeProjectId: ProjectId,
): readonly Project[] {
  const now = Date.now();
  return SEED_PROJECT_NAMES.map((name): Project => {
    const id = `D:/dev/${name}`;
    if (id !== activeProjectId) {
      return { id, name, group: 'Dev', worktrees: [] };
    }
    return {
      id,
      name,
      group: 'Dev',
      worktrees: [
        {
          branch: 'master',
          isPrimary: true,
          sessions: sessions.map((session) => ({
            paneKey: session.paneKey,
            handle: session.handle,
            kind: session.kind,
            title: session.title,
            // Lifecycle detection is v0.2 (the hook and the OSC 133 state machine), so a
            // live session reports the one thing this build can prove: it exists, or it
            // has exited.
            status: session.exitStatus === null ? ('running' as const) : ('failed' as const),
            startedAt: session.createdAtMs > 0 ? session.createdAtMs : now,
          })),
        },
      ],
    };
  });
}

/**
 * The `+` menu, derived from `ShellProfile`'s four variants.
 *
 * Which shells are actually installed is the daemon's answer and will replace this; the
 * four kinds, though, are on the wire already, so deriving the menu from a closed enum is
 * reading the protocol rather than inventing a second one.
 *
 * The agent row is Claude alone. There is one agent in v1 and no provider trait (D-3, D-4),
 * and listing providers that cannot be started would be an affordance that looks live and
 * does nothing.
 */
const LAUNCHERS: readonly LauncherGroup[] = [
  {
    label: 'AGENTS',
    items: [
      { id: 'agent.claude', label: 'Claude', hint: 'default', kind: 'agent' as SessionKind },
    ],
  },
  {
    label: 'TERMINALS',
    items: [
      { id: 'shell.pwsh', label: 'PowerShell 7', hint: 'pwsh', kind: 'shell' as SessionKind },
      { id: 'shell.cmd', label: 'Command Prompt', hint: 'cmd', kind: 'shell' as SessionKind },
      { id: 'shell.git_bash', label: 'Git Bash', hint: 'bash', kind: 'shell' as SessionKind },
      { id: 'shell.wsl', label: 'WSL', hint: 'wsl', kind: 'shell' as SessionKind },
    ],
  },
];

/** The wire profile a launcher asks for, or `null` for the default agent. */
function profileFor(launcher: string): ShellProfile | null {
  switch (launcher) {
    case 'shell.pwsh':
      return { shell: 'pwsh' };
    case 'shell.cmd':
      return { shell: 'cmd' };
    case 'shell.git_bash':
      return { shell: 'git_bash' };
    case 'shell.wsl':
      return { shell: 'wsl', distro: null };
    default:
      return null;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
