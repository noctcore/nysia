import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { createMockStore } from './store/mock/MockStore';
import { StoreProvider } from './store/StoreProvider';
import { ThemeProvider } from './theme/ThemeProvider';
import './index.css';

const container = document.getElementById('root');
if (!container) {
  throw new Error('index.html must provide #root');
}

/*
 * The one line wave 2 changes.
 *
 * `createMockStore()` becomes the daemon-backed provider from `src/transport`, and nothing
 * else in `apps/web` moves: every component reads session, project and tab data through the
 * `Store` interface, and both providers satisfy the same contract in `store/storeContract.ts`.
 */
const store = createMockStore();

createRoot(container).render(
  <StrictMode>
    <ThemeProvider>
      <StoreProvider store={store}>
        <App />
      </StoreProvider>
    </ThemeProvider>
  </StrictMode>,
);
