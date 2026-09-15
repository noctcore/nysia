import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { createMockStore } from '../store/mock/MockStore';
import { StoreProvider } from '../store/StoreProvider';
import { ThemeProvider } from '../theme/ThemeProvider';
import { SETTINGS_TREE } from './nav';
import { SettingsScreen } from './SettingsScreen';

/*
 * The nav's glyph column, rendered node-only (D-18).
 *
 * `nav.test.ts` proves the tree decided on a mark for every entry; this proves the marks
 * reach the screen and land beside the right labels. The two failures it covers are the
 * ones that typecheck perfectly: a `NavItem` that stops forwarding `glyph`, and a slot
 * that closes up when there is nothing to draw, which would step the labels in and out
 * down the column and read as a rendering bug rather than as the deliberate gap `nav.ts`
 * argues for.
 *
 * `createElement` rather than JSX so the file stays `.test.ts` and inside the
 * `src/**\/*.test.ts` glob the shared vitest config defines.
 */
const screen = renderToStaticMarkup(
  createElement(ThemeProvider, {
    children: createElement(StoreProvider, {
      store: createMockStore(),
      children: createElement(SettingsScreen),
    }),
  }),
);

/** A label as it is spelled in the markup: `Git & source control` arrives escaped. */
function escaped(label: string): string {
  return label.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/** The markup of the nav row carrying `label`, from its opening tag to the label itself. */
function rowBefore(label: string): string {
  const at = screen.indexOf(`>${escaped(label)}<`);
  expect(at, `no nav row is labelled ${label}`).toBeGreaterThan(-1);
  const open = screen.lastIndexOf('<button', at);
  expect(open, `${label} is not inside a button`).toBeGreaterThan(-1);
  // `at` sits on the `>` that closes the tag in front of the label, so the slice takes it
  // too: stopping one character short leaves a half-written tag that no strip can remove.
  return screen.slice(open, at + 1);
}

describe('the settings nav', () => {
  it('rendered at all, with the tree it is given', () => {
    // Without this every case below would pass vacuously against an empty string.
    expect(screen).toContain('aria-label="Settings"');
    expect(screen).toContain('Search settings');
  });

  it('draws each entry its own mark, beside its own label', () => {
    for (const group of SETTINGS_TREE) {
      for (const entry of group.entries) {
        if (entry.glyph === null) {
          continue;
        }
        // `escaped` on the glyph too: `>_` reaches the markup as `&gt;_`.
        expect(rowBefore(entry.label), `${entry.id} lost its mark`).toContain(
          escaped(entry.glyph),
        );
      }
    }
  });

  it('keeps the slot, and its width, on the row that has no mark', () => {
    // The gap is a decision (`nav.ts`), and it only reads as one while the column stays
    // aligned. `w-4` is what reserves the cell; dropping the span would close it up.
    const row = rowBefore('Nysia account');
    expect(row, 'the empty slot lost its reserved width').toContain('w-4');
    expect(row.replace(/<[^>]*>/g, '').trim(), 'the empty slot drew something').toBe('');
  });

  it('gives the project rows the same empty slot, not a shared mark', () => {
    // A project is identified by its name. One mark repeated down every row would be the
    // #73 failure again, on this side of the window.
    const { projects } = createMockStore().getSnapshot();
    expect(projects.length, 'the fixture needs a project to render').toBeGreaterThan(0);
    for (const project of projects) {
      const row = rowBefore(project.name);
      expect(row, `${project.name} lost its reserved slot`).toContain('w-4');
      expect(row.replace(/<[^>]*>/g, '').trim(), `${project.name} drew a mark`).toBe('');
    }
  });

  it('lets the row carry the mark’s colour rather than naming one', () => {
    // The slot inherits `text-fg`/`text-fg2` from the button, so selection and hover move
    // the glyph with the label and the accent picker has nothing here to get wrong. A
    // colour of its own would not be a literal, so `colourGuard` would never see it — it
    // would just be a mark that stops agreeing with the row around it.
    const slot = rowBefore('Appearance').slice(rowBefore('Appearance').lastIndexOf('<span'));
    expect(slot, 'the glyph slot moved or was renamed').toContain('w-4');
    expect(slot, 'the glyph slot names its own colour').not.toMatch(/\btext-(fg|fg2|fg3|acc)\b/);
  });
});
