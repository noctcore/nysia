import type { PaneKey } from '../generated/PaneKey';
import type { SessionHandle } from '../generated/SessionHandle';
import type { SessionKind } from '../generated/SessionKind';
import type { StoreError } from './errors';

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

/**
 * How a project is addressed.
 *
 * `nysia-proto` does not export a project identity yet (W1 owns the wire surface), so this
 * is a local alias rather than a hand-rolled duplicate of a generated type: the mock keys
 * projects by repository path, which is what the daemon will do too.
 */
export type ProjectId = string;

/** How a launcher in the `+` menu is addressed. */
export type LauncherId = string;

/** Which of the three rail destinations is showing. */
export type NavSection = 'session' | 'tasks' | 'history';

/**
 * Agent lifecycle, as the status palette will paint it (design-spec.md §1).
 *
 * Carried from v0.1 even though nothing paints it yet: the sidebar's session dot is the
 * accent, because there it only has to say "an agent lives here". The lifecycle colours
 * belong to the Tasks table (v0.3), where a row has to be triaged at a glance among
 * dozens. Modelling the state now means the daemon-backed provider has somewhere to put
 * it without the interface changing shape.
 */
export type SessionStatus = 'idle' | 'running' | 'needsInput' | 'queued' | 'failed';

/** One agent or shell, as the sidebar and the tab strip see it. */
export interface SessionSummary {
  readonly paneKey: PaneKey;
  readonly handle: SessionHandle;
  readonly kind: SessionKind;
  readonly title: string;
  readonly status: SessionStatus;
  /** Epoch milliseconds. The sidebar's `21h` is derived, never stored pre-formatted. */
  readonly startedAt: number;
}

/**
 * The sessions on one branch.
 *
 * Keyed by branch, never by task id (D-6): a worktree outlives the task that created it,
 * and two tasks on one branch share it.
 */
export interface Worktree {
  readonly branch: string;
  readonly isPrimary: boolean;
  readonly sessions: readonly SessionSummary[];
}

export interface Project {
  readonly id: ProjectId;
  readonly name: string;
  /** The sidebar group header — `Dev` in the design mock. */
  readonly group: string;
  readonly worktrees: readonly Worktree[];
}

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
  readonly activeProjectId: ProjectId | null;
  readonly tabs: readonly Tab[];
  readonly activeTab: PaneKey | null;
  readonly launchers: readonly LauncherGroup[];
  readonly daemon: DaemonMetrics;
  readonly usage: readonly UsageWindow[];
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
    activeProjectId: null,
    tabs: [],
    activeTab: null,
    launchers: [],
    daemon: { memoryBytes: 0, terminalCount: 0, worktreeCount: 0 },
    usage: [],
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
 * `snapshot.errors`. Components never call one bare: `runCommand` in `./runCommand` is the
 * only call site shape, because a bare call is an unhandled rejection waiting for the
 * first missing shell binary.
 */
export interface Store {
  /** Never null, and referentially stable between notifications. */
  getSnapshot(): StoreSnapshot;
  /** Returns the unsubscribe function, as `useSyncExternalStore` expects. */
  subscribe(listener: () => void): () => void;

  selectNav(section: NavSection): Promise<void>;
  selectProject(id: ProjectId): Promise<void>;
  selectTab(paneKey: PaneKey): Promise<void>;
  closeTab(paneKey: PaneKey): Promise<void>;
  openTab(launcher: LauncherId): Promise<void>;
  /** Drops one recorded failure. Unknown ids are not an error — dismissal is idempotent. */
  dismissError(id: string): Promise<void>;

  readonly window: WindowControls;
}
