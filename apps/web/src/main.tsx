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
 * The one line wave 2 changed: the mock became the daemon-backed provider, and no component
 * moved. Both satisfy `store/storeContract.ts`, so the chrome cannot tell them apart.
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
