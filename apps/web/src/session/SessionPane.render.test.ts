import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { createMockStore } from '../store/mock/MockStore';
import { createSeedSnapshot } from '../store/mock/seed';
import { StoreProvider } from '../store/StoreProvider';
import type { StoreSnapshot } from '../store/types';
import { ThemeProvider } from '../theme/ThemeProvider';
import { GLYPH } from '../ui/glyphs';
import { SessionPane } from './SessionPane';

/*
 * The shape of the pane, asserted rather than assumed.
 *
 * The pane used to draw design-spec.md §3 item 4 — an accent `❯`, "Send a message" and a
 * block cursor — below the terminal. It was a span: nothing typed into it, nothing read it,
 * and in v0.1 the only place to type is the terminal. Deleting it is easy; keeping it
 * deleted is not, because the spec still draws it and the next reader is entitled to think
 * the app is behind the design. Hence a test that names the absence, and the doc comment on
 * `SessionPane` that says where the row actually goes.
 *
 * Static markup, node-only, no DOM (D-18) — the same mechanism `App.render.test.ts` uses,
 * and written with `createElement` so the file stays inside the `src/**\/*.test.ts` glob.
 * It cannot measure layout, so what it checks is the classes that produce it: the flex
 * chain that lets the terminal grow, and the absence of the rows that used to eat the
 * bottom of the pane. The fit itself is `TerminalView`'s `ResizeObserver`, which only has
 * something to observe if this chain is intact.
 */
function render(snapshot?: StoreSnapshot): string {
  return renderToStaticMarkup(
    createElement(ThemeProvider, {
      children: createElement(StoreProvider, {
        store: createMockStore(snapshot),
        children: createElement(SessionPane),
      }),
    }),
  );
}

/** The seed, with every session closed — what a first-run window shows. */
function noSessions(): StoreSnapshot {
  return { ...createSeedSnapshot(), tabs: [], activeTab: null };
}

describe('the session pane', () => {
  const withSession = render();

  it('renders the terminal for the active tab', () => {
    // Non-vacuity first: every case below is an absence, and an absence is satisfied by an
    // empty string.
    expect(withSession.length).toBeGreaterThan(80);
    expect(withSession).toContain('data-testid="terminal-surface"');
  });

  it('draws no prompt row under the terminal', () => {
    for (const gone of ['Send a message', GLYPH.prompt, 'shadow-focus', 'rounded-panel']) {
      expect(withSession, gone).not.toContain(gone);
    }
  });

  it('offers no second place to type, in either state', () => {
    // The point of the deletion, stated as behaviour rather than as markup: a shell session
    // has exactly one input, and it is the terminal.
    for (const markup of [withSession, render(noSessions())]) {
      expect(markup).not.toMatch(/<(input|textarea)\b/);
      expect(markup).not.toContain('contenteditable');
    }
  });

  it('gives the terminal the whole pane', () => {
    // Two elements: the pane and the terminal host. The prompt row and the 12px spacer that
    // used to follow it are what this counts the absence of — and a third element appearing
    // here is exactly the regression the doc comment warns about.
    expect(withSession.match(/<div/g)).toHaveLength(2);
    expect(withSession).not.toContain('h-3');
    expect(withSession).not.toContain('pt-4');
  });

  it('keeps the grow chain intact from the pane down to the terminal host', () => {
    // Without `flex-1` on the pane root, a column-flex child sizes to its content: the host
    // then has nothing to grow into, xterm measures the box it sized itself, and `fit()`
    // returns the rows it already had. The terminal looks right and never learns the
    // geometry. Both halves are asserted because either one alone breaks it.
    for (const tag of withSession.matchAll(/<div[^>]*>/g)) {
      expect(tag[0], 'an element in the pane chain cannot grow').toContain('flex-1');
      expect(tag[0], 'an element in the pane chain cannot shrink').toContain('min-h-0');
    }
  });

  it('tightens the gutter to 12px rather than the card gutter of the spec', () => {
    expect(withSession).toContain('px-3');
    expect(withSession).not.toContain('px-6');
  });
});

describe('the pane with no session open', () => {
  const empty = render(noSessions());

  it('renders an empty state rather than the terminal', () => {
    expect(empty.length).toBeGreaterThan(200);
    expect(empty).not.toContain('data-testid="terminal-surface"');
  });

  it('draws the shell glyph in a tile, from the shared glyph set', () => {
    // `>_` arrives escaped, which is the whole reason to build the expectation from `GLYPH`
    // rather than to type the characters here.
    const at = empty.indexOf('aria-hidden');
    const tile = empty.slice(empty.lastIndexOf('<div', at), empty.indexOf('</div>', at));
    expect(tile).toContain(GLYPH.shell.replace('>', '&gt;'));
    expect(tile).toContain('font-mono');
    expect(tile).toContain('text-acc');
  });

  it('names the affordance that actually opens a session', () => {
    expect(empty).toContain('No session open');
    expect(empty).toContain(GLYPH.add);
    expect(empty).toContain('title bar');
  });

  it('promises a shell, and dates the agent surface rather than implying it', () => {
    // The old line offered "an agent or a shell". v0.1 can start `claude`, but it starts it
    // in a pty — there is no transcript, no prompt and no chips row until v0.2, and an
    // empty state is a bad place to be vague about that.
    expect(empty).toContain('shell');
    expect(empty).not.toContain('start an agent');
    expect(empty).toContain('v0.2');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    // The package-wide sweep in `colourGuard.test.ts` reads source; this reads the pixels
    // the pane actually emits, in the state that sweep's callers never render.
    expect([...empty.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});
