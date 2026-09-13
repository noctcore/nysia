import type { Store } from '../store/types';
import { createTauriBridge, type DaemonBridge } from './bridge';
import { DaemonStore } from './DaemonStore';
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
    // Windows is the assumption until `host_platform` answers, because it is the primary
    // development platform and the answer arrives within a frame. The only platform where
    // guessing wrong is expensive is macOS, and the correction runs before any pane has
    // asked for a renderer.
    platform: options?.platform ?? 'windows',
  });

  const store = new DaemonStore({ bridge, router });

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
