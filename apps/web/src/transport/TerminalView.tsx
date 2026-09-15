import { useEffect, useRef } from 'react';

import { useSnapshot } from '../store/hooks';
import { attachedStore } from './attachedStore';

/**
 * The terminal for the pane that is on screen.
 *
 * One surface is mounted at a time, because only visible panes get a renderer (§7.3, D-7).
 * Everything else is in Rust: switching tabs takes this pane's renderer away, and the next
 * one draws from the daemon rather than from anything held here. That is what makes thirty
 * sessions cheap and the window genuinely disposable.
 *
 * The component is deliberately thin — a host element and a lifecycle — because everything
 * worth testing lives below it in `XtermSurface`, where the node-only suite can reach it
 * (D-18). A component that decided when to acknowledge bytes, or what to do on context
 * loss, would have put that logic in the one place this project's tests cannot run.
 *
 * It renders nothing when the store is not daemon-backed, so the mock provider still drives
 * the chrome for anyone working on it without a daemon.
 */
export function TerminalView() {
  // Not `useCommands()`, and deliberately not `StoreContext` either — `eslint.config.js`
  // grants that exactly two carve-outs and this is neither. What this needs is not a store
  // command at all: it is the transport's own terminal router, and this component is part of
  // the transport rather than a consumer of it. `attachedStore()` is how the transport
  // reaches its own instance, without widening a door the store module closed on purpose.
  const store = attachedStore();
  const { activeTab } = useSnapshot();
  const host = useRef<HTMLDivElement>(null);

  // Read during render, not inside the effect, so they can be dependencies. A reconnect
  // disposes every surface, and the effect has to run again to mount the new one. Keyed on
  // `activeTab` alone it would not, and the pane would hold a surface that was disposed with
  // the connection it belonged to.
  const stream = store === null || activeTab === null ? null : store.surfaceStream(activeTab);

  // **And the id alone is not enough.** A daemon whose id counter restarts hands the first
  // session id 1 again, so a reconnect can leave this pane's id exactly as it was while the
  // surface behind it has been disposed. The effect then never reran: the pane kept a dead
  // surface, the output went to a fresh one the delivery path built lazily and nothing had
  // shown, and it buffered as hidden — 256 KiB and then dropped — while the status bar said
  // ready. The only way back was to switch tabs away and return.
  const connection = store === null ? 0 : store.streamEpoch;

  useEffect(() => {
    const element = host.current;
    if (element === null || activeTab === null || store === null || stream === null) {
      return;
    }

    const surface = store.terminals.surface(stream);
    surface.show(element);
    surface.focus();

    // A hidden pane whose buffer overflowed threw output away to stay bounded. The surface
    // has reset the parser, but the bytes are gone, and a terminal that silently skips a
    // stretch of its own output is worse than one that says so — the user reads what is
    // left as if it followed on.
    const dropped = store.terminals.takeDroppedWhileHidden(stream);
    if (dropped > 0) {
      store.reportDroppedOutput(stream, dropped);
    }

    // The daemon sizes the PTY, so a pane that resized without telling it leaves the shell
    // wrapping at the old width — the commonest visible symptom of a terminal that is
    // almost right. Measuring is the surface's job (only it knows its cell metrics); this
    // only notices that the host changed and passes the answer on.
    let last = '';
    const remeasure = () => {
      const fitted = surface.fit();
      if (fitted === null) {
        return;
      }
      const size = `${fitted.cols}x${fitted.rows}`;
      if (size === last) {
        return;
      }
      last = size;
      void store.resize(activeTab, fitted.cols, fitted.rows).catch(() => undefined);
    };
    remeasure();
    const observer = new ResizeObserver(remeasure);
    observer.observe(element);

    const stopInput = surface.onInput((data) => {
      // A failed keystroke gets no notice of its own: the disconnect that caused it already
      // has one, and one notice per character typed while the daemon is down would bury
      // every other message in the list.
      void store.sendInput(activeTab, data).catch(() => undefined);
    });

    return () => {
      observer.disconnect();
      stopInput();
      // Hidden, not disposed. The surface belongs to the router, which keeps it for as long
      // as the session does — so a tab switched away from and back keeps its place in the
      // daemon's stream instead of asking for a full replay every time.
      surface.hide();
    };
  }, [store, activeTab, stream, connection]);

  return (
    <div
      ref={host}
      // `min-h-0` so the terminal shrinks with the pane rather than pushing the prompt row
      // off the bottom: xterm measures its host, and a host that cannot shrink sizes the
      // grid wrong on every resize.
      className="min-h-0 flex-1 overflow-hidden"
      data-testid="terminal-surface"
    />
  );
}
