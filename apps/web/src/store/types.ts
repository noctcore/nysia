import type { PaneKey } from '../generated/PaneKey';
import type { SessionHandle } from '../generated/SessionHandle';
import type { SessionKind } from '../generated/SessionKind';

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

/** The right-hand side of the status bar. */
export interface DaemonStatus {
  readonly connected: boolean;
  /** Resident set of the daemon, in bytes. Formatted at the edge, like every other metric. */
  readonly memoryBytes: number;
  readonly terminalCount: number;
  readonly worktreeCount: number;
}

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
  readonly nav: NavSection;
  readonly projects: readonly Project[];
  readonly activeProjectId: ProjectId | null;
  readonly tabs: readonly Tab[];
  readonly activeTab: PaneKey | null;
  readonly launchers: readonly LauncherGroup[];
  readonly daemon: DaemonStatus;
  readonly usage: readonly UsageWindow[];
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

export interface Store {
  /** Referentially stable until a command mutates it. */
  getSnapshot(): StoreSnapshot;
  /** Returns the unsubscribe function, as `useSyncExternalStore` expects. */
  subscribe(listener: () => void): () => void;

  selectNav(section: NavSection): Promise<void>;
  selectProject(id: ProjectId): Promise<void>;
  selectTab(paneKey: PaneKey): Promise<void>;
  closeTab(paneKey: PaneKey): Promise<void>;
  openTab(launcher: LauncherId): Promise<void>;

  readonly window: WindowControls;
}
