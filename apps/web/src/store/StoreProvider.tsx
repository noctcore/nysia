import type { ReactNode } from 'react';

import { StoreContext } from './StoreContext';
import type { Store } from './types';

/**
 * Puts one store in front of the whole window.
 *
 * Taking the store as a prop rather than constructing it is what makes the wave-2 swap a
 * one-line change in `main.tsx`: the mock provider and the daemon-backed provider are both
 * just a `Store`.
 */
export function StoreProvider({
  store,
  children,
}: {
  readonly store: Store;
  readonly children: ReactNode;
}) {
  return <StoreContext value={store}>{children}</StoreContext>;
}
