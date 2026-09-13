import { useContext, useSyncExternalStore } from 'react';

import { StoreContext } from './StoreContext';
import type { Store, StoreSnapshot } from './types';

/** The store itself, for issuing commands. Throws outside `StoreProvider`. */
export function useStore(): Store {
  const store = useContext(StoreContext);
  if (store === null) {
    throw new Error('useStore must be used inside <StoreProvider>');
  }
  return store;
}

/**
 * The current snapshot, re-rendering the caller when it changes.
 *
 * `useSyncExternalStore` rather than a context value holding state: the wave-2 provider is
 * fed by a daemon over a Channel, which is an external source by definition, and this is
 * the hook that makes such a source tear-free under concurrent rendering.
 */
export function useSnapshot(): StoreSnapshot {
  const store = useStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}
