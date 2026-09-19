import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { GeneralPane } from './GeneralPane';
import { CONFIRM_STOP_AGENT_LABEL, DEFAULT_GENERAL, saveGeneral } from './generalPreferences';
import { memoryStorage } from './memoryStorage';

/*
 * Settings › General, rendered node-only (D-18).
 *
 * The one row this file is about is the stop confirmation. The tab strip's dialog can turn
 * it off with *Don't ask again*, and a destructive confirmation that can be switched off
 * from one place and not found in another is worse than none — so what is claimed here is
 * that the row exists, is a live switch rather than a label, and shows what is stored.
 *
 * `localStorage` is stubbed because the pane reads it through `settingsStorage()`, and the
 * point is to render what a stored value produces rather than to reach past the pane.
 */

function render(): string {
  return renderToStaticMarkup(createElement(GeneralPane));
}

/** The opening tag of the switch the row carries. */
function stopSwitch(pane: string): string {
  const tag = pane.match(
    new RegExp(`<button[^>]*aria-label="${CONFIRM_STOP_AGENT_LABEL}"[^>]*>`),
  );
  expect(tag, `no switch is labelled ${CONFIRM_STOP_AGENT_LABEL}`).not.toBeNull();
  return tag?.[0] ?? '';
}

describe('the stop confirmation row', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('is a live switch, on when nothing is stored', () => {
    vi.stubGlobal('localStorage', memoryStorage());
    const row = stopSwitch(render());
    expect(row).toContain('role="switch"');
    expect(row).toContain('aria-checked="true"');
    expect(row, 'a switch nobody can press is not a way back').not.toContain('disabled');
  });

  it('shows the question switched off when that is what is stored', () => {
    const storage = memoryStorage();
    saveGeneral(storage, { ...DEFAULT_GENERAL, confirmStopAgent: false });
    vi.stubGlobal('localStorage', storage);
    expect(stopSwitch(render())).toContain('aria-checked="false"');
  });
});
