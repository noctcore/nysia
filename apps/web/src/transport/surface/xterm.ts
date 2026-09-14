import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import { muteTerminalReplies } from './muteReplies';
import type { TerminalFactory, XtermLike } from './XtermSurface';

/**
 * The one module in the repository that imports `@xterm/*`.
 *
 * Everything else depends on {@link import('./TerminalSurface').TerminalSurface}, so when
 * `ghostty-web` is worth swapping in (§7.3) it is a sibling of this file and nothing above
 * it moves. Keeping the import in one place is what makes that claim checkable rather than
 * aspirational.
 *
 * The versions are the beta line VS Code and Orca actually ship — `@xterm/xterm`
 * 6.1.0-beta.30x with `@xterm/addon-webgl` 0.20.0-beta.30x — pinned in `package.json`.
 * xterm 6 removed the canvas renderer, so there is no middle option: WebGL or DOM.
 *
 * This file is deliberately not unit-tested. It constructs a `Terminal`, which needs a DOM
 * and a canvas, and v0.1's tests are node-only (D-18). Everything it would be worth testing
 * — when bytes are acknowledged, what a hidden pane does, what happens on context loss —
 * lives in `XtermSurface` behind the injected factory, where the node tests reach it.
 */

/**
 * Read a theme token off the document.
 *
 * xterm takes concrete colour strings, not CSS variables, so the values have to be resolved
 * here. Reading the live computed value rather than importing the token table is what keeps
 * the terminal following the runtime theme switcher: `ThemeProvider` writes the variables on
 * `<html>`, and a terminal built afterwards picks up whatever is in force. A literal here
 * would be a pixel that stops following the switch, which CLAUDE.md §6 calls a bug.
 */
function token(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return value.length > 0 ? value : fallback;
}

/**
 * The terminal's colours, read from the tokens in force.
 *
 * The fallbacks are `transparent` and `inherit` rather than colours: if a token is somehow
 * missing, the right outcome is that the terminal takes the surrounding surface's colour,
 * not that it paints a hardcoded one the theme switcher will never touch again.
 */
function themeColours(): Record<string, string> {
  return {
    background: token('--color-bg0', 'transparent'),
    foreground: token('--color-fg', 'inherit'),
    cursor: token('--color-acc', 'inherit'),
    cursorAccent: token('--color-bg0', 'transparent'),
    selectionBackground: token('--color-acc35', 'transparent'),
  };
}

/**
 * Build a real xterm terminal.
 *
 * `renderer` is the pool's decision, not a preference: on macOS a `webgl` answer has already
 * been weighed against WebKit's app-wide sixteen-context cap, and asking for one anyway
 * would be the surface overruling the only thing that counts them.
 */
export const createXterm: TerminalFactory = ({
  cols,
  rows,
  renderer,
  onContextLoss,
}): XtermLike => {
  const terminal = new Terminal({
    cols,
    rows,
    // design-spec.md §1: Fira Code at 12.5px, 1.65 line height.
    fontFamily: '"Fira Code", ui-monospace, monospace',
    fontSize: 12.5,
    lineHeight: 1.65,
    theme: themeColours(),
    // The daemon owns the scrollback (§7.2) and the webview is a display cache (D-7), so
    // xterm keeps only enough to scroll the current screen comfortably. A second copy here
    // would be a second authority on what the terminal contains, and the one that is wrong
    // after a reattach.
    scrollback: 1000,
    allowProposedApi: true,
  });

  // **Before anything is written.** The daemon has already answered every query this
  // terminal is about to parse — its virtual terminal has a reply sink and the pump writes
  // what that sink collects straight to the pty — so a second answer from here is not a
  // duplicate, it is the display cache typing into the child on the user's behalf (D-7).
  // On Windows ConPTY reads the end of a cursor-position report as F3. See `muteReplies.ts`
  // for the table, and for the dispatch ordering it was read out of xterm's source to rest
  // on.
  muteTerminalReplies(terminal.parser);

  // The addon that knows how many cells fit in the host. It is loaded for every renderer,
  // because measuring is not a WebGL concern.
  const fit = new FitAddon();
  terminal.loadAddon(fit);

  if (renderer === 'webgl') {
    const addon = new WebglAddon();
    // §7.3: dispose and fall back to DOM. Disposing the addon here rather than leaving it
    // attached matters — a WebglAddon whose context is gone keeps its canvas alive and the
    // context is never returned to WebKit's pool.
    addon.onContextLoss(() => {
      addon.dispose();
      onContextLoss();
    });
    terminal.loadAddon(addon);
  }

  return Object.assign(terminal, {
    fit: (): { cols: number; rows: number } | undefined => {
      // `proposeDimensions` returns undefined before the host has a layout, which is the
      // state on the first frame after mounting. Calling `fit()` then would resize the
      // terminal to NaN cells and the daemon would be told a size no PTY can take.
      const proposed = fit.proposeDimensions();
      if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) {
        return undefined;
      }
      fit.fit();
      return { cols: proposed.cols, rows: proposed.rows };
    },
  });
};
