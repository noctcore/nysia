import { describe, expect, it } from 'vitest';

import type { DaemonBridge } from './bridge';
import { attachedStore, setAttachedStore } from './attachedStore';
import { DaemonStore } from './DaemonStore';
import { TerminalRouter } from './terminals';

/**
 * A bridge that answers nothing.
 *
 * None of these tests connects: what is under test is the *shape* of what the singleton
 * hands out, which is decided at `setAttachedStore` and never touches the daemon.
 */
const silentBridge: DaemonBridge = {
  invoke: () => new Promise(() => undefined),
  attachChannel: () => new Promise(() => undefined),
  window: {
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
  },
};

function store(): DaemonStore {
  return new DaemonStore({
    bridge: silentBridge,
    // Never called: no test here mounts a surface, and a factory that throws says so rather
    // than quietly handing back a stub that could make a broken test pass.
    router: new TerminalRouter({
      bridge: silentBridge,
      createTerminal: () => {
        throw new Error('no test here mounts a surface');
      },
    }),
  });
}

describe('the attached transport', () => {
  /**
   * The defect this closes. `eslint.config.js` bans importing `StoreContext` so that no
   * component can hold the raw provider and drop a command promise, and that ban is intact —
   * but a singleton returning the `DaemonStore` reproduced the banned *shape* by another
   * route, with every store command one property access away from `TerminalView`.
   *
   * Reverting `setAttachedStore` to `attached = store` fails this and nothing else, which is
   * what makes it a proof rather than a restatement (traps register #12).
   */
  it('does not hand a component any store command', () => {
    setAttachedStore(store());
    const transport = attachedStore();

    expect(transport).not.toBeNull();
    for (const command of [
      'selectNav',
      'selectProject',
      'selectTab',
      'openTab',
      'closeTab',
      'dismissError',
      'getSnapshot',
      'subscribe',
      'dispose',
      'run',
      'window',
    ]) {
      expect(transport === null || command in transport).toBe(false);
    }
  });

  it('hands over exactly what the pane component needs, and nothing more', () => {
    setAttachedStore(store());
    const transport = attachedStore();

    expect(transport === null ? [] : Object.keys(transport).sort()).toEqual([
      'reportDroppedOutput',
      'resize',
      'sendInput',
      'streamEpoch',
      'surfaceStream',
      'terminals',
    ]);
  });

  /**
   * A facade over a *copy* would satisfy every shape assertion above and still be wrong: the
   * pane would report into an object nobody reads. This drives one member through the facade
   * and looks for its effect on the store itself.
   */
  it('reaches the store this window is actually running', () => {
    const running = store();
    setAttachedStore(running);
    const transport = attachedStore();

    expect(running.getSnapshot().errors).toEqual([]);
    transport?.reportDroppedOutput(4096);

    const [recorded] = running.getSnapshot().errors;
    expect(recorded?.message).toContain('4096');
  });

  /**
   * `streamEpoch` changes on every reconnect, and `TerminalView` reads it during render as
   * the dependency that remounts a pane whose surface was disposed with the connection it
   * belonged to. Copied once at `setAttachedStore` it would be frozen at 0 for the life of
   * the window, and the pane would keep a dead surface — the exact failure the epoch was
   * added to fix, reintroduced by the facade rather than by the store.
   */
  it('reads the stream epoch live rather than freezing the one it was built with', () => {
    setAttachedStore(store());
    const transport = attachedStore();
    const descriptor =
      transport === null ? undefined : Object.getOwnPropertyDescriptor(transport, 'streamEpoch');

    expect(descriptor?.get).toBeTypeOf('function');
    expect(descriptor?.value).toBeUndefined();
  });
});
