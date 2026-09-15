import type { Store } from '../store/types';
import { setAttachedStore } from './attachedStore';
import { createTauriBridge, type DaemonBridge } from './bridge';
import { DaemonStore } from './DaemonStore';
import { createTransportLog } from './log';
import { createXterm } from './surface/xterm';
import type { TerminalFactory } from './surface/XtermSurface';
import { TerminalRouter } from './terminals';

export { TerminalView } from './TerminalView';

/**
 * The daemon-backed store, wired to the real Tauri bridge and a real xterm.
 *
 * This is the one line wave 2 changes in `main.tsx`. Everything the chrome reads comes
 * through the `Store` interface, so no component knows which provider it is talking to —
 * which is the whole reason W3 built the boundary before the transport existed.
 *
 * The connect loop is started here and deliberately not awaited. The window has to render
 * before the daemon answers: the chrome draws from the empty snapshot with
 * `status: 'connecting'`, and the first frame replaces it. A `createDaemonStore` that
 * awaited the socket would hold the window blank for as long as the daemon took, and
 * forever if it never came.
 */
export function createDaemonStore(options?: {
  readonly bridge?: DaemonBridge;
  readonly createTerminal?: TerminalFactory;
  readonly platform?: string;
}): Store {
  const bridge = options?.bridge ?? createTauriBridge();
  const router = new TerminalRouter({
    bridge,
    createTerminal: options?.createTerminal ?? createXterm,
    // No platform yet on purpose: the router runs the cautious renderer policy until
    // `host_platform` answers below. See `TerminalRouter`'s constructor for why guessing
    // is the wrong side to err on.
    ...(options?.platform === undefined ? {} : { platform: options.platform }),
  });

  // The real logger, which reaches the window's log file through `client_log`. Everything
  // below this line that goes wrong leaves a trace; before it, the one surface the user
  // touches left none. See `log.ts`.
  const store = new DaemonStore({ bridge, router, log: createTransportLog(bridge) });
  // How `TerminalView` finds the router without reaching through `StoreContext`, which the
  // store module closed to components on purpose. See `attachedStore.ts`.
  setAttachedStore(store);

  void bridge
    .invoke<string>('host_platform')
    .then((platform) => router.setPlatform(platform))
    .catch(() => {
      // The renderer policy is a performance decision, not a correctness one: an
      // unanswered `host_platform` leaves the window drawing with the default, which is
      // never *wrong*, only potentially slower.
    });

  void store.run();
  return store;
}
