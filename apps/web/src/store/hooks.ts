import { useContext, useMemo, useSyncExternalStore } from 'react';

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
 * Failures that escaped a provider without being wrapped.
 *
 * Read separately from the snapshot because they are a statement about the provider rather
 * than part of its state — see `unexpectedFailures.ts`.
 */
export function useUnexpectedFailures(): readonly StoreError[] {
  return useSyncExternalStore(
    unexpectedFailures.subscribe,
    unexpectedFailures.getSnapshot,
    unexpectedFailures.getSnapshot,
  );
}
