import type { DaemonStore } from './DaemonStore';

/**
 * The daemon-backed store this window is running, for the transport's own component.
 *
 * ## Why this is not a React context
 *
 * `TerminalView` needs the terminal router, `surfaceStream`, `sendInput` and `resize` —
 * none of which are store *commands*, and none of which `useCommands()` can hand out. The
 * obvious way to reach them is `useContext(StoreContext)`, and `eslint.config.js` bans that
 * with exactly two carve-outs, neither of them here. That ban is right: the raw provider's
 * commands return promises a call site can drop silently, and widening the door for this one
 * component would reopen it for every component.
 *
 * A second context would mean a second provider in `main.tsx`, and that file's edit is meant
 * to stay the one-line swap it was designed to be.
 *
 * ## Why a module singleton is honest here
 *
 * There is one window, one daemon connection, and one store — `main.tsx` constructs it once
 * and nothing else can. So this holds exactly what already exists, and it is scoped to
 * `src/transport`, where the store, the router and the view all live. Nothing outside can
 * reach it: this module is not exported from `index.ts`.
 *
 * It is `null` under the mock provider, which is the case that matters for anyone working on
 * the chrome without a daemon: `TerminalView` renders its host element and mounts nothing.
 */
let attached: DaemonStore | null = null;

/** Record the store this window is running. Called once, from `createDaemonStore`. */
export function setAttachedStore(store: DaemonStore): void {
  attached = store;
}

/** The store this window is running, or `null` when it is not daemon-backed. */
export function attachedStore(): DaemonStore | null {
  return attached;
}
