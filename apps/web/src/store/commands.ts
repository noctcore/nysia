import type { PaneKey } from '../generated/PaneKey';
import type { Issue } from '../tasks/issue';
import { StoreCommandError } from './errors';
import type { StoreCommandName } from './errors';
import { reportUnexpectedFailure } from './unexpectedFailures';
import type { LauncherId, NavSection, ProjectId, Store, StoreSnapshot } from './types';

/**
 * The command surface a component is handed.
 *
 * "Every call site goes through `runCommand`" used to be a convention, and a convention is
 * not a structure: nothing stopped `void store.closeTab(key)`, which compiled, linted
 * clean, and produced an unhandled rejection with nothing on screen. All the call sites
 * were correct; the next one was the problem.
 *
 * So the raw `Store` no longer reaches components. `useCommands()` hands them this
 * instead — the same verbs, already routed, each returning `void` because there is no
 * promise left for a caller to drop. The unrouted shape is not something a component can
 * write by accident; reaching it now takes an explicit `useContext(StoreContext)`, which
 * is a deliberate act rather than a slip. TypeScript has no module privacy and the lint
 * config lives outside this package, so that last door cannot be locked from here — but it
 * is a door someone has to walk through on purpose.
 *
 * `getSnapshot` is here because the tab strip needs the settled state inside a callback.
 * `subscribe` is not: that is `useSnapshot`'s job, and keeping it in one place is what
 * stops a component growing its own subscription.
 */
export interface WindowCommands {
  minimize(onSettled?: () => void): void;
  toggleMaximize(onSettled?: () => void): void;
  close(onSettled?: () => void): void;
}

export interface StoreCommands {
  /** The snapshot as it stands right now, for a callback that has to read it back. */
  getSnapshot(): StoreSnapshot;

  selectNav(section: NavSection, onSettled?: () => void): void;
  selectProject(id: ProjectId, onSettled?: () => void): void;
  selectTab(paneKey: PaneKey, onSettled?: () => void): void;
  closeTab(paneKey: PaneKey, onSettled?: () => void): void;
  openTab(launcher: LauncherId, onSettled?: () => void): void;
  refreshLaunchers(onSettled?: () => void): void;
  addProject(onSettled?: () => void): void;
  dismissAddProject(onSettled?: () => void): void;
  refreshTasks(onSettled?: () => void): void;
  startTask(issue: Issue, onSettled?: () => void): void;
  dismissError(id: string, onSettled?: () => void): void;

  readonly window: WindowCommands;
}

/**
 * Issue one command and make sure its outcome cannot vanish.
 *
 * A `StoreCommandError` is an expected outcome the provider has *already* written into
 * `snapshot.errors`, so there is a notice on screen and nothing left to do here. Anything
 * else is a provider bug or a transport failure that was not wrapped — `errors.ts` says a
 * dropped connection mid-request has to reach the user, and a provider that lets a raw
 * `TypeError` out has broken that promise. It goes to the failure sink rather than only to
 * the console, because the whole point of this layer is that failures stop being silent
 * and it must not become a new way to be silent.
 *
 * `onSettled` runs after either outcome, for the caller that has to move DOM focus once
 * the store has converged.
 */
export function runCommand(
  command: StoreCommandName,
  promise: Promise<void>,
  onSettled?: () => void,
): void {
  promise
    .catch((cause: unknown) => {
      if (cause instanceof StoreCommandError) {
        return;
      }
      reportUnexpectedFailure(command, cause);
    })
    .finally(() => onSettled?.());
}

/**
 * Wrap a provider's verbs so a component cannot hold an unhandled promise.
 *
 * Called once per store instance and memoised by `useCommands`, so the object is
 * referentially stable across renders and an `onClick` closure does not change identity
 * every time something repaints.
 */
export function routeCommands(store: Store): StoreCommands {
  return {
    getSnapshot: () => store.getSnapshot(),

    selectNav: (section, onSettled) =>
      runCommand('selectNav', store.selectNav(section), onSettled),
    selectProject: (id, onSettled) =>
      runCommand('selectProject', store.selectProject(id), onSettled),
    selectTab: (paneKey, onSettled) =>
      runCommand('selectTab', store.selectTab(paneKey), onSettled),
    closeTab: (paneKey, onSettled) =>
      runCommand('closeTab', store.closeTab(paneKey), onSettled),
    openTab: (launcher, onSettled) =>
      runCommand('openTab', store.openTab(launcher), onSettled),
    refreshLaunchers: (onSettled) =>
      runCommand('refreshLaunchers', store.refreshLaunchers(), onSettled),
    addProject: (onSettled) => runCommand('addProject', store.addProject(), onSettled),
    dismissAddProject: (onSettled) =>
      runCommand('dismissAddProject', store.dismissAddProject(), onSettled),
    // Under its own name, though it is documented never to reject. The only notice it can
    // produce comes from `reportUnexpectedFailure` — a provider that broke that promise — and
    // naming that one `startTask` would tell the user the wrong verb about a failure that is
    // already a surprise.
    refreshTasks: (onSettled) => runCommand('refreshTasks', store.refreshTasks(), onSettled),
    startTask: (issue, onSettled) =>
      runCommand('startTask', store.startTask(issue), onSettled),
    dismissError: (id, onSettled) =>
      runCommand('dismissError', store.dismissError(id), onSettled),

    window: {
      minimize: (onSettled) =>
        runCommand('window.minimize', store.window.minimize(), onSettled),
      toggleMaximize: (onSettled) =>
        runCommand('window.toggleMaximize', store.window.toggleMaximize(), onSettled),
      close: (onSettled) => runCommand('window.close', store.window.close(), onSettled),
    },
  };
}
