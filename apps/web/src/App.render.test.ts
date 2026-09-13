import { createElement, type ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { App } from './App';
import { AppearancePane } from './settings/AppearancePane';
import { GeneralPane } from './settings/GeneralPane';
import { SettingsScreen } from './settings/SettingsScreen';
import { createMockStore } from './store/mock/MockStore';
import { StoreProvider } from './store/StoreProvider';
import { ThemeProvider } from './theme/ThemeProvider';

/*
 * A render smoke test, still node-only (D-18).
 *
 * `renderToStaticMarkup` is not browser mode and needs no DOM — effects never run, so
 * nothing here touches `document`. What it buys is the one failure the other gates cannot
 * see: a component that throws on first paint typechecks, lints and builds perfectly, and
 * the window comes up blank. It also proves the chrome is reading the store rather than
 * literals, which is the claim wave 2 has to be able to trust.
 *
 * Written with `createElement` rather than JSX so the file stays `.test.ts` and inside the
 * `src/**\/*.test.ts` glob the shared vitest config defines.
 */
function render(node: ReactElement): string {
  return renderToStaticMarkup(
    createElement(ThemeProvider, {
      children: createElement(StoreProvider, { store: createMockStore(), children: node }),
    }),
  );
}

describe('the window', () => {
  const app = render(createElement(App));

  it('lays out the two grids design-spec.md §2 specifies', () => {
    expect(app).toContain(
      'grid-rows-[var(--spacing-titlebar)_1fr_var(--spacing-statusbar)]',
    );
    expect(app).toContain('grid-cols-[var(--spacing-rail)_var(--spacing-sidebar)_1fr]');
    expect(app).toContain('w-wordmark');
  });

  it('draws custom chrome, never native decorations', () => {
    for (const control of ['Minimise', 'Maximise', 'Close']) {
      expect(app, control).toContain(`aria-label="${control}"`);
    }
    expect(app).toContain('⌘K');
  });

  it('makes the wordmark itself draggable, not just the bar behind it', () => {
    // Tauri checks the element the pointer landed on, not its ancestors. With the
    // attribute on the titlebar root alone the effective drag area was a 14px pad and a
    // couple of gap seams, because the strip and the controls fill the bar's height.
    const wordmark = app.slice(app.indexOf('w-wordmark') - 200, app.indexOf('nysia<'));
    expect(wordmark.match(/data-tauri-drag-region/g)?.length).toBeGreaterThanOrEqual(2);
    expect(app.match(/data-tauri-drag-region/g)?.length).toBeGreaterThanOrEqual(5);
  });

  it('keeps the drag region off every button', () => {
    // A drag region over a control swallows its clicks, which is how a titlebar ends up
    // with buttons that look live and do nothing.
    for (const tag of app.matchAll(/<button[^>]*>/g)) {
      expect(tag[0]).not.toContain('data-tauri-drag-region');
    }
  });

  it('renders one tab per seeded session, with the active one merged into the pane', () => {
    expect(app.match(/role="tab"/g)).toHaveLength(4);
    // The active tab paints its bottom border the colour of the body beneath it.
    expect(app).toContain('border-b-bg1');
  });

  it('puts nothing but tabs inside the tablist', () => {
    // The role promises a screen reader that every child is a tab. The `+` button and the
    // close buttons used to live in here, which made that a lie.
    const tablist = between(app, 'role="tablist"', '</div></div>');
    expect(tablist).not.toContain('New session');
    expect(tablist.match(/role="tab"/g)).toHaveLength(4);
  });

  it('gives the strip one tab stop and reaches the rest with arrows', () => {
    // Exactly one roving tab stop, and the close buttons out of the tab order — they are
    // reachable by mouse and by Delete on the focused tab (APG's deletable-tab pattern).
    const tabs = [...app.matchAll(/<div role="tab"[^>]*>/g)].map((match) => match[0]);
    expect(tabs).toHaveLength(4);
    expect(tabs.filter((tag) => tag.includes('tabindex="0"'))).toHaveLength(1);
    for (const close of [...app.matchAll(/<button[^>]*aria-label="Close [^"]*"[^>]*>/g)]) {
      expect(close[0]).toContain('tabindex="-1"');
    }
  });

  it('ships no affordance that looks live and does nothing', () => {
    // Every unbuilt settings entry names a release; the rest of the chrome is held to the
    // same standard. Help is the only rail item that is not built, so it is the only one
    // disabled — and its tooltip, not its label, is what says when.
    const help = tagContaining(app, 'aria-label="Help"');
    expect(help).toContain('disabled');
    expect(help).toMatch(/title="[^"]*later version[^"]*"/);

    const addProject = tagContaining(app, 'aria-label="Add a project');
    expect(addProject).toContain('disabled');
    expect(addProject).toMatch(/title="[^"]+"/);

    const live = [...app.matchAll(/<button(?![^>]*disabled)[^>]*>/g)];
    expect(live.length).toBeGreaterThan(5);
  });

  it('pushes Settings and Help below the three rail destinations', () => {
    for (const label of ['Session', 'Tasks', 'History', 'Settings', 'Help']) {
      expect(app, label).toContain(`aria-label="${label}"`);
    }
    expect(app.indexOf('aria-label="History"')).toBeLessThan(
      app.indexOf('aria-label="Settings"'),
    );
  });

  it('expands only the active project, showing its branch and session ages', () => {
    expect(app).toContain('Search projects');
    expect(app.match(/repeating-linear-gradient/g)).toHaveLength(10);
    expect(app).toContain('primary');
    expect(app).toContain('>21h<');
    expect(app).toContain('>3m<');
  });

  it('prints the daemon figures the store carries, not the mock strings', () => {
    expect(app).toContain('100% left 5h · 97% left 6d · 99% left Fable');
    expect(app).toContain('4.00 GB');
    expect(app).toContain('Open terminals');
    expect(app).toContain('Worktrees');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    // Every colour arrives as a Tailwind utility over a custom property. A hex in the
    // rendered output would be a pixel that ignores the accent picker.
    expect([...app.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});

describe('settings', () => {
  it('gives the search box a real input rather than a div that looks like one', () => {
    const settings = render(createElement(SettingsScreen));
    expect(settings).toContain('aria-label="Search settings"');
    expect(settings).toMatch(/<input[^>]*placeholder="Search settings"/);
  });

  it('renders the whole nav tree, including what v0.1 does not build', () => {
    const settings = render(createElement(SettingsScreen));
    for (const entry of ['Agents', 'Orchestration', 'General', 'Appearance', 'Terminal']) {
      expect(settings, entry).toContain(entry);
    }
    // Per-project settings follow the store's projects, not a literal list.
    expect(settings).toContain('shiroani');
    // Settings search has its own key (design-spec.md §5); the palette's is ⌘K.
    expect(settings).toContain('⌘F');
    expect(settings).not.toContain('⌘K');
  });

  it('offers both themes and every accent preset', () => {
    const appearance = render(createElement(AppearancePane));
    expect(appearance).toContain('Ember');
    expect(appearance).toContain('Graphite');
    expect(appearance.match(/role="radio"/g)?.length).toBeGreaterThanOrEqual(6);
  });

  it('renders the mock general groups with working controls', () => {
    const general = render(createElement(GeneralPane));
    expect(general).toContain('Session recaps');
    expect(general).toContain('Completion sound');
    expect(general).toContain('role="switch"');
    expect(general).toContain('role="radiogroup"');
  });
});

/** The slice between a marker and the first following terminator. */
function between(source: string, from: string, to: string): string {
  const start = source.indexOf(from);
  const end = source.indexOf(to, start);
  return source.slice(start, end === -1 ? undefined : end);
}

/** The whole opening tag that carries `marker`, however its attributes are ordered. */
function tagContaining(source: string, marker: string): string {
  const at = source.indexOf(marker);
  return source.slice(source.lastIndexOf('<', at), source.indexOf('>', at) + 1);
}
