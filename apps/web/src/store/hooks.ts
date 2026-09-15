import { useContext, useMemo, useSyncExternalStore } from 'react';

import type { AgentStatus } from '../generated/AgentStatus';
import type { PaneKey } from '../generated/PaneKey';
import { agentNotifications, type AgentNotification } from './agentNotifications';
import { findAgentStatus } from './agentStatus';
import { routeCommands, type StoreCommands } from './commands';
import { StoreContext } from './StoreContext';
import type { Store, StoreSnapshot } from './types';
import { unexpectedFailures } from './unexpectedFailures';
import type { StoreError } from './errors';

/** The provider itself. Module-private: components are handed `useCommands()` instead. */
function useProvider(): Store {
  const store = useContext(StoreContext);
  if (store === null) {
    throw new Error('the store hooks must be used inside <StoreProvider>');
  }
  return store;
}

/**
 * The verbs, already routed.
 *
 * There is no `useStore()` any more, deliberately. It handed back the raw provider, whose
 * commands return promises, and "always wrap them in `runCommand`" was a convention that
 * nothing enforced — `void store.closeTab(key)` compiled, linted clean and produced an
 * unhandled rejection with nothing on screen. Every verb here returns `void`, so there is
 * no promise for a caller to drop, and removing the old hook means a call site that was
 * not updated fails to resolve rather than compiling into the old shape.
 *
 * Memoised on the provider so the object is referentially stable across renders.
 */
export function useCommands(): StoreCommands {
  const store = useProvider();
  return useMemo(() => routeCommands(store), [store]);
}

/**
 * The current snapshot, re-rendering the caller when it changes.
 *
 * `useSyncExternalStore` rather than a context value holding state: the wave-2 provider is
 * fed by a daemon over a Channel, which is an external source by definition, and this is
 * the hook that makes such a source tear-free under concurrent rendering.
 */
export function useSnapshot(): StoreSnapshot {
  const store = useProvider();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

/**
 * Failures that escaped a provider without being wrapped, and the way to dismiss one.
 *
 * Read separately from the snapshot because they are a statement about the provider rather
 * than part of its state — see `unexpectedFailures.ts`.
 *
 * Dismissal comes back with the list rather than being fetched off the singleton at the
 * call site. That was the shape this package spent a PR removing everywhere else: a
 * component reading through a hook and then reaching past it for the object to write to is
 * two routes to one thing, and the second is the one that gets copied.
 *
 * The pair is memoised because pairing them means building an object, and a fresh object
 * every render is a dependency array that never settles for whoever consumes this next.
 * `failures` is the whole dependency list: the sink is created once at module scope and
 * never reassigned, so `dismiss` is the same function for the life of the window, and
 * `getSnapshot` hands back the same array until the list actually changes.
 */
export interface UnexpectedFailures {
  readonly failures: readonly StoreError[];
  dismiss(id: string): void;
}

export function useUnexpectedFailures(): UnexpectedFailures {
  const failures = useSyncExternalStore(
    unexpectedFailures.subscribe,
    unexpectedFailures.getSnapshot,
    unexpectedFailures.getSnapshot,
  );
  return useMemo(() => ({ failures, dismiss: unexpectedFailures.dismiss }), [failures]);
}

/**
 * The status of one pane's agent, or `undefined` when the daemon has none for it.
 *
 * `undefined` is a real answer and not a gap to paper over: a shell has no agent at all,
 * and an agent has no row until its first hook reaches the daemon. `agentDot` in
 * `./agentStatus` paints that case as the accent rather than as a lifecycle colour, because
 * "nothing is known" and "idle" are different things.
 *
 * A hook per row rather than a lookup table built once: `useSnapshot` is
 * `useSyncExternalStore` and the rows are components already subscribed to it, so this adds
 * a `find` over a list of tens and no subscription that was not already there.
 */
export function useAgentStatus(paneKey: PaneKey): AgentStatus | undefined {
  return findAgentStatus(useSnapshot().agentStatus, paneKey);
}

/**
 * The notices raised by status changes, and the way to dismiss one.
 *
 * Read off the sink rather than the snapshot, for the reason `agentNotifications.ts` gives:
 * a notification is an event this window reacted to, not state the daemon holds. Shaped
 * exactly like `useUnexpectedFailures` — including the memoised pair, because pairing them
 * builds an object and a fresh one every render is a dependency array that never settles.
 */
export interface AgentNotifications {
  readonly notices: readonly AgentNotification[];
  dismiss(id: string): void;
}

export function useAgentNotifications(): AgentNotifications {
  const notices = useSyncExternalStore(
    agentNotifications.subscribe,
    agentNotifications.getSnapshot,
    agentNotifications.getSnapshot,
  );
  return useMemo(() => ({ notices, dismiss: agentNotifications.dismiss }), [notices]);
}
