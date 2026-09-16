import type { AgentStatus } from '../generated/AgentStatus';
import type { PaneKey } from '../generated/PaneKey';
import type { Project } from '../generated/Project';
import type { ProjectId } from '../generated/ProjectId';
import type { SessionHandle } from '../generated/SessionHandle';
import type { SessionKind } from '../generated/SessionKind';
import type { AddProjectState } from './addProject';
import type { StoreError } from './errors';

/**
 * The wire's shapes, re-exported so a component imports them from one place.
 *
 * `Project`, `Worktree`, `ProjectId` and `SessionSummary` were hand-written here while
 * `nysia-proto` had no word for a project. It has one now, so these are the generated types
 * and nothing else — D-13 is one way only, and a second definition of a shape Rust already
 * exports is a definition that drifts. The re-export exists so that `store/types.ts` stays
 * the module the chrome reads, not so that anything here may alter them.
 *
 * # What happened to `SessionStatus` and `startedAt`
 *
 * The hand-written `SessionSummary` here had `status` and `startedAt`; proto's has
 * `exitStatus` and `createdAtMs`. One of those pairs is a rename and one is a real
 * difference, and **proto was right about both**.
 *
 * `createdAtMs` is the rename: the same epoch milliseconds under the name the wire uses,
 * and the sidebar's `21h` was always derived from it rather than stored pre-formatted.
 *
 * `status` is the real difference, and it was already dead. Its own doc comment said so:
 * *"nothing paints from this"*. v0.1 guessed that a five-state `SessionStatus` would become
 * the sidebar's dot; it did not — agent lifecycle arrived on the wire in v0.2 as
 * `AgentState`, the dots come from `StoreSnapshot.agentStatus`, and the only thing this
 * side could ever fill `status` with was a guess derived from `exitStatus`. `DaemonStore`
 * did exactly that, mapping a live session to `running` and an exited one to `failed` — so
 * a shell that exited zero was reported as *failed*, because there was no honest value to
 * give it. `exitStatus` answers the question the daemon can actually answer: how the child
 * ended, or `null` while it is still running. The guess is gone with the field.
 */
export type { Project, ProjectId };
export type { SessionSummary } from '../generated/SessionSummary';
export type { Worktree } from '../generated/Worktree';
export type { AddProjectState } from './addProject';

/**
 * The one boundary between the chrome and whatever is behind it.
 *
 * Every component in `apps/web` reads session, project and tab data through `Store` and
 * nothing else. Wave 1 ships a mock provider seeded from the design mock; wave 2 (W5)
 * replaces it with a daemon-backed provider over the multiplexed Channel, and the swap is
 * one line in `main.tsx` — not a single component file changes. That is the whole point of
 * this module, so the rules are worth stating:
 *
 *  - **No component imports the mock.** It reaches the tree through `StoreProvider`.
 *  - **Commands return promises.** The mock resolves immediately; a daemon round-trip does
 *    not, and a synchronous signature would have to be broken to admit that.
 *  - **`getSnapshot()` is referentially stable** until a command replaces it. React's
 *    `useSyncExternalStore` re-renders forever against a store that allocates per call.
 *  - **Identities come from `src/generated`** (D-13). `PaneKey` outlives the process, the
 *    window and the daemon, so it — not an array index — is what the UI keys on.
 *  - **`subscribe` and `getSnapshot` are bound.** `useSyncExternalStore` is handed them
 *    detached, so an implementation that reads `this` through them breaks on the first
 *    render. The contract suite checks it.
 *  - **There is always a snapshot.** A socket-backed provider has nothing to show until
 *    its first frame arrives, so the empty snapshot is a real state with a name
 *    (`status: 'connecting'`) rather than a null the chrome has to guard.
 *  - **A command that cannot be satisfied rejects *and* records.** See `./errors`.
 */

/** How a launcher in the `+` menu is addressed. */
export type LauncherId = string;

/** Which of the three rail destinations is showing. */
export type NavSection = 'session' | 'tasks' | 'history';

/** One tab in the strip. A tab is a session, and a session is an agent or a shell. */
export interface Tab {
  readonly paneKey: PaneKey;
  readonly handle: SessionHandle;
  readonly kind: SessionKind;
  readonly title: string;
}

/** One entry in the `+` menu. What can be launched is something only the daemon knows. */
export interface Launcher {
  readonly id: LauncherId;
  readonly label: string;
  /** Right-aligned mono hint — `pwsh`, `cmd`, `bash`, or `default` for the default agent. */
  readonly hint: string;
  readonly kind: SessionKind;
}

export interface LauncherGroup {
  readonly label: string;
  readonly items: readonly Launcher[];
}

/**
 * The four figures on the right of the status bar.
 *
 * Whether the daemon is reachable is deliberately **not** here: that is `StoreSnapshot.status`,
 * which distinguishes a first connection from a reconnect from a give-up. Carrying a second
 * `connected` boolean beside it would be two answers to one question, and the two would
 * disagree the first time a reconnect half-succeeded.
 */
export interface DaemonMetrics {
  /** Resident set of the daemon, in bytes. Formatted at the edge, like every other metric. */
  readonly memoryBytes: number;
  readonly terminalCount: number;
  readonly worktreeCount: number;
}

/**
 * Where the connection to the daemon stands.
 *
 * `connecting` is the first attempt and the state every provider starts in; `reconnecting`
 * is a re-attempt with a snapshot already on screen, which the chrome renders as stale
 * rather than blank; `failed` is a provider that has given up and needs the user to do
 * something. Under D-1/D-2 the daemon outlives the window, so these three are the only
 * thing the window can honestly say about it.
 */
export type StoreStatus = 'connecting' | 'ready' | 'reconnecting' | 'failed';

/**
 * One quota window, for the status bar summary only.
 *
 * The multi-provider usage surface — the popover with per-provider rows and the hover
 * flyout — is v0.4. This carries just enough to render `100% left 5h · 97% left 6d`.
 */
export interface UsageWindow {
  readonly label: string;
  readonly percentLeft: number;
}

export interface StoreSnapshot {
  readonly status: StoreStatus;
  /**
   * Failed commands, oldest first, until the user dismisses them.
   *
   * A list rather than one `lastError` slot: two shells can fail to launch before anyone
   * looks at the screen, and the second overwriting the first is how a user learns to
   * distrust the notice.
   */
  readonly errors: readonly StoreError[];
  readonly nav: NavSection;
  readonly projects: readonly Project[];
  /**
   * Why the project list is not what the daemon holds, or `null` when it is.
   *
   * The sentence comes off the daemon's own error envelope and is shown as the sidebar's
   * empty state rather than as a failed command. It is **not** a notice: nobody asked for
   * this list, it is fetched on every connect, and a red *"addProject failed"* box on every
   * launch is how a user learns to dismiss the notice list unread.
   *
   * There is one answer it is guaranteed to carry today. v0.3 wave C1 serves the project
   * verbs; until it lands the daemon answers `unsupported`, with a sentence saying which
   * build is which and how to compare them. That is the honest thing for the sidebar to say
   * — and the reason the ten seeded project names had to go, because *"the daemon has never
   * heard of a project"* and a list of plausible names is the one thing that cannot be read
   * off the screen.
   */
  readonly projectsUnavailable: string | null;
  readonly activeProjectId: ProjectId | null;
  readonly tabs: readonly Tab[];
  readonly activeTab: PaneKey | null;
  readonly launchers: readonly LauncherGroup[];
  readonly daemon: DaemonMetrics;
  readonly usage: readonly UsageWindow[];
  /**
   * What each agent pane is doing, as the daemon last reported it.
   *
   * The wire type, unaltered: `AgentStatus` per pane, lead row plus subagent roster. D-13
   * makes Rust the sole authority on that shape, so this carries it rather than a
   * client-side rewrite of it — `./agentStatus` holds the *reading* of it, which is a
   * different job and a local one.
   *
   * A list, matching `AgentStatusList`'s `statuses: Array<AgentStatus>`, and empty until a
   * provider has something to put in it. Empty is a real answer: the daemon's status RPC
   * lands in v0.2 wave C, so the daemon-backed provider reports nothing here until then,
   * and a pane with no row is painted as *unknown* rather than as idle.
   */
  readonly agentStatus: readonly AgentStatus[];
  /** Where **Add a project** has got to. See `./addProject`. */
  readonly addProject: AddProjectState;
}

/**
 * The snapshot every provider starts from, before it has heard anything.
 *
 * Shipped beside the interface rather than left to each provider so that "the chrome
 * renders on an empty snapshot" stays true by construction: the mock, the async probe and
 * W5's daemon-backed provider all begin here, and a field added to `StoreSnapshot` cannot
 * be forgotten in three places.
 */
export function emptySnapshot(status: StoreStatus = 'connecting'): StoreSnapshot {
  return {
    status,
    errors: [],
    nav: 'session',
    projects: [],
    projectsUnavailable: null,
    activeProjectId: null,
    tabs: [],
    activeTab: null,
    launchers: [],
    daemon: { memoryBytes: 0, terminalCount: 0, worktreeCount: 0 },
    usage: [],
    agentStatus: [],
    addProject: { phase: 'idle' },
  };
}

/**
 * The window buttons.
 *
 * They live on the store because `apps/web` may not import `@tauri-apps` outside
 * `src/transport` (D-1, D-2, and the ESLint ban that enforces it). The chrome is custom on
 * every platform, so these three are the only way to minimise, maximise or close.
 */
export interface WindowControls {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}

/**
 * The provider behind the chrome.
 *
 * Every command resolves once the store has converged on the result, and rejects with a
 * `StoreCommandError` when it could not — having already appended the matching entry to
 * `snapshot.errors`. Components never see this interface at all: `useCommands()` in
 * `./hooks` hands them the routed surface from `./commands`, whose verbs return `void`, so
 * a bare call with an unhandled rejection is not a shape a component can write.
 */
export interface Store {
  /** Never null, and referentially stable between notifications. */
  getSnapshot(): StoreSnapshot;
  /** Returns the unsubscribe function, as `useSyncExternalStore` expects. */
  subscribe(listener: () => void): () => void;

  selectNav(section: NavSection): Promise<void>;
  selectProject(id: ProjectId): Promise<void>;
  /**
   * Browse for a folder and register it.
   *
   * **It resolves on every answer the daemon can give**, including the three refusals in
   * v0.3 §3.2 and a path that was already registered, writing the outcome into
   * `snapshot.addProject` for the dialog to render. Only a failure that is not an answer —
   * a dropped socket, a daemon that does not serve the verb — rejects and records.
   *
   * That split is the whole contract, and getting it wrong is visible to a user: a refusal
   * that rejected would be routed through `runCommand` into the notice list as *"addProject
   * failed"*, and §3.2 is explicit that registering a folder twice **is not an error and
   * must not look like one**. A folder holding several repositories is a choice to make,
   * not a fault either.
   *
   * Cancelling the picker is an answer too, and the quietest one: the state returns to
   * `idle` and nothing is said. Calling this while a picker is already open does nothing —
   * two pickers is two registrations racing.
   */
  addProject(): Promise<void>;
  /** Clear whatever `addProject` last said. Idempotent, like `dismissError`. */
  dismissAddProject(): Promise<void>;
  selectTab(paneKey: PaneKey): Promise<void>;
  closeTab(paneKey: PaneKey): Promise<void>;
  openTab(launcher: LauncherId): Promise<void>;
  /** Drops one recorded failure. Unknown ids are not an error — dismissal is idempotent. */
  dismissError(id: string): Promise<void>;

  readonly window: WindowControls;
}
