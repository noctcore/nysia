import { createElement, type ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { NewTabButton } from '../chrome/NewTabButton';
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
 *
 * **Nothing here names the terminal's own markup, which is half of #40.** Two assertions
 * used to: an exact count of the `div` elements in the rendered pane, and the presence of
 * `data-testid="terminal-surface"` under the mock store. Both belonged to `src/transport`,
 * and both were loaded — `TerminalView`'s doc comment says it renders nothing when the
 * store is not daemon-backed, which is the only kind of store this file has, while the code
 * renders the host unconditionally. Whichever way the transport owner settles that
 * disagreement, a file that cannot see the decision goes red for it. What this pane owns is
 * the branch it takes, its own geometry and its own copy; that is what is asserted here,
 * and the terminal is asserted in the package that owns it.
 */
function through(node: ReactElement, snapshot?: StoreSnapshot): string {
  return renderToStaticMarkup(
    createElement(ThemeProvider, {
      children: createElement(StoreProvider, {
        store: createMockStore(snapshot),
        children: node,
      }),
    }),
  );
}

function render(snapshot?: StoreSnapshot): string {
  return through(createElement(SessionPane), snapshot);
}

/** The seed, with every session closed — what a first-run window shows. */
function noSessions(): StoreSnapshot {
  return { ...createSeedSnapshot(), tabs: [], activeTab: null };
}

/**
 * What a screen reader calls the button the empty state points at.
 *
 * Read off `NewTabButton` rather than typed here, because the assertion is that the copy
 * and the accessible name agree (#40). A literal on both sides would go on passing while
 * the button was renamed underneath it, which is the drift this is for.
 */
function newSessionName(): string {
  const markup = through(createElement(NewTabButton));
  const name = /aria-label="([^"]+)"/.exec(markup)?.[1];
  if (name === undefined) {
    throw new Error('the new-session button renders without an accessible name');
  }
  return name;
}

describe('the session pane', () => {
  const withSession = render();

  it('gives the pane to the terminal rather than to the empty state', () => {
    // The branch is what this component decides; what the terminal then draws is not.
    // The root tag is asserted first so the absence below cannot pass on an empty string,
    // which is how every assertion in this file could otherwise be satisfied.
    expect(withSession).toMatch(/^<div class="[^"]*px-3/);
    expect(withSession).not.toContain('No session open');
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

  it('wraps the terminal in nothing of its own', () => {
    // The prompt row and the 12px spacer that used to follow it are what this is the
    // absence of. It was a count of `div` elements, two of which were the terminal's; what
    // the pane actually owns is that it contributes no words and no spacer around the grid.
    // A reintroduced prompt row puts text back here whatever element it is made of, and
    // whatever the component below it renders.
    expect(withSession.replace(/<[^>]*>/g, '').trim()).toBe('');
    expect(withSession).not.toContain('h-3');
    expect(withSession).not.toContain('pt-4');
  });

  it('keeps the grow chain intact from the pane down', () => {
    // Without `flex-1` on the pane root, a column-flex child sizes to its content: the host
    // then has nothing to grow into, xterm measures the box it sized itself, and `fit()`
    // returns the rows it already had. The terminal looks right and never learns the
    // geometry. Both halves are asserted because either one alone breaks it, and every
    // element the chain actually emits is covered rather than a fixed number of them.
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

  it('renders an empty state rather than giving the pane away', () => {
    expect(empty.length).toBeGreaterThan(200);
    expect(empty).toContain('No session open');
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

  it('calls the affordance what a screen reader calls it', () => {
    // The other half of #40. The line read "Press + in the title bar": the button is
    // announced by its accessible name and nothing is bound to a key, so a listener was
    // told to press a control that has neither that name nor a shortcut. The expectation
    // is read off the button, so the copy and the name cannot part company quietly.
    expect(empty).toContain(newSessionName());
    expect(empty).toContain('title bar');
    expect(empty).not.toMatch(/\bpress\b/i);
  });

  it('keeps the glyph as decoration beside that name', () => {
    // The `+` is how the button is found by eye and is noise read aloud, so it sits next to
    // the name as an `aria-hidden` span rather than standing in for it.
    const hidden = [...empty.matchAll(/<span[^>]*aria-hidden="true"[^>]*>([^<]*)<\/span>/g)];
    expect(hidden.map((match) => match[1])).toContain(GLYPH.add);
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
