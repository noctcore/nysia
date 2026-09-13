import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { StoreProvider } from './store/StoreProvider';
import { createDaemonStore } from './transport';
import { ThemeProvider } from './theme/ThemeProvider';
import './index.css';

const container = document.getElementById('root');
if (!container) {
  throw new Error('index.html must provide #root');
}

/*
 * The line wave 2 changed.
 *
 * The mock became the daemon-backed provider from `src/transport`, and nothing else in
 * `apps/web` moved: every component reads session, project and tab data through the `Store`
 * interface, and both providers satisfy the same contract in `store/storeContract.ts`. The
 * mock is still there and still tested, for anyone working on the chrome without a daemon.
 */
const store = createDaemonStore();

createRoot(container).render(
  <StrictMode>
    <ThemeProvider>
      <StoreProvider store={store}>
        <App />
      </StoreProvider>
    </ThemeProvider>
  </StrictMode>,
);
