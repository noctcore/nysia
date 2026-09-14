import type { PaneKey } from '../generated/PaneKey';
import type { DaemonStore } from './DaemonStore';
import type { StreamId } from './frames';
import type { TerminalRouter } from './terminals';

/**
 * What the transport's own pane component may reach, and nothing else.
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
 * ## Why the module singleton hands out this and not the store
 *
 * The ban exists to stop a component holding the raw provider and dropping a command
 * promise. A singleton that returned {@link DaemonStore} reproduced exactly that shape by a
 * different route: the eslint rule was untouched and its carve-outs were not widened, so
 * the *import* ban held, while `attachedStore()` handed the same object over anyway.
 * `selectNav`, `selectProject`, `selectTab`, `openTab`, `closeTab` and `dismissError` all
 * reject with a `StoreCommandError` and were all one property access away from a component
 * that has no business calling them.
 *
 * `CLAUDE.md` §6: a security default an ordinary caller can undo is not a default, it is a
 * suggestion. So this is a **runtime object**, not a narrower type over the same instance —
 * a type would be undone by one `as DaemonStore` and leave nothing in review to catch it.
 * What is not on this interface cannot be reached from the value at all.
 *
 * ## Why these six are safe to expose
 *
 * Four are reads or notices that cannot fail. The two that do reach the daemon —
 * {@link sendInput} and {@link resize} — **record their own failures and never reject**:
 * each catches internally and deduplicates the notice, precisely because one notice per
 * character typed while the daemon is down would bury every other message in the list. So
 * the promise they return carries no failure a caller could drop. That is a property of
 * those two methods, not a general licence, and it is why adding a seventh member here
 * means re-reading this paragraph first.
 *
 * There is deliberately **no escape hatch** to the raw store. Nothing needs one, and §6's
 * separately-named-entry-point rule is for overrides that are genuinely needed — not for
 * keeping a door open in case.
 */
export interface AttachedTransport {
  /** The router that owns every surface, for the component that mounts them. */
  readonly terminals: TerminalRouter;
  /** Which stream connection {@link surfaceStream} is currently answering for. */
  readonly streamEpoch: number;
  /** The surface a pane draws into, or `null` before the daemon has named the session. */
  surfaceStream(paneKey: PaneKey): StreamId | null;
  /** Tell the user that output was thrown away while a pane was hidden. */
  reportDroppedOutput(bytes: number): void;
  /** Forward what the user typed. Records its own failure; never rejects. */
  sendInput(paneKey: PaneKey, text: string): Promise<void>;
  /** Tell the daemon a pane changed size, in cells. Records its own failure; never rejects. */
  resize(paneKey: PaneKey, cols: number, rows: number): Promise<void>;
}

/**
 * The transport this window is running, or `null` when it is not daemon-backed.
 *
 * `null` under the mock provider, which is the case that matters for anyone working on the
 * chrome without a daemon: `TerminalView` renders its host element and mounts nothing.
 *
 * There is one window, one daemon connection and one store — `main.tsx` constructs it once
 * and nothing else can — so a module singleton holds exactly what already exists. It is
 * scoped to `src/transport`, where the store, the router and the view all live, and nothing
 * outside can reach it: this module is not exported from `index.ts`.
 */
let attached: AttachedTransport | null = null;

/**
 * Record the store this window is running. Called once, from `createDaemonStore`.
 *
 * The store goes in and only {@link AttachedTransport} comes out. The narrowing happens
 * **here**, at the one call site that has the store, rather than at the many that read it —
 * which is what makes it impossible to get the rest back rather than merely inconvenient.
 */
export function setAttachedStore(store: DaemonStore): void {
  attached = {
    terminals: store.terminals,
    get streamEpoch() {
      return store.streamEpoch;
    },
    surfaceStream: (paneKey) => store.surfaceStream(paneKey),
    reportDroppedOutput: (bytes) => {
      store.reportDroppedOutput(bytes);
    },
    sendInput: (paneKey, text) => store.sendInput(paneKey, text),
    resize: (paneKey, cols, rows) => store.resize(paneKey, cols, rows),
  };
}

/** The transport this window is running, or `null` when it is not daemon-backed. */
export function attachedStore(): AttachedTransport | null {
  return attached;
}
