import { createContext } from 'react';

import type { Store } from './types';

/**
 * The store the tree reads through.
 *
 * Its own module so `StoreProvider.tsx` exports nothing but a component — `pnpm lint` runs
 * ESLint with `--max-warnings 0`, and `react-refresh/only-export-components` is a warning.
 */
export const StoreContext = createContext<Store | null>(null);
