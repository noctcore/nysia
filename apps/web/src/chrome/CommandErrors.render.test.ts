import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { MockStore } from '../store/mock/MockStore';
import { createSeedSnapshot } from '../store/mock/seed';
import { StoreProvider } from '../store/StoreProvider';
import type { StoreSnapshot } from '../store/types';
import { CommandErrors } from './CommandErrors';

/*
 * The notice that reports a missing runtime, carrying the path to the runtime (#71).
 *
 * Launching in dev with no daemon shows `openTab failed`, and the daemon's `nextSteps`
 * carries an absolute path. On Windows that is `C:\Users\…\target\debug\nysia.exe`: no
 * spaces, therefore no break opportunities, therefore a box that only knows how to break
 * between words neither wraps it nor clips it and the text runs past the border. This is the
 * common case rather than the edge — the notice about a missing binary is guaranteed to name
 * one.
 *
 * **What a node-only test can say about a layout bug.** Nothing directly: there is no DOM
 * here and no layout engine, so nothing in this file can observe an overflow. What it can do
 * is what `TabStrip.source.test.ts` does for its mousedown guard — hold that the fix is
 * *written*. Each case below names the failure the utility prevents, so removing one gets a
 * test that says why rather than silence. Seeing the overflow itself needs the real window,
 * which is where the bug was found and where it was confirmed gone.
 */

/** The path from the bug report: the shape that has no break opportunity in it. */
const DAEMON_PATH =
  'C:\\Users\\kacpe\\Documents\\Projekty\\nysia\\target\\debug\\nysia.exe';

function withError(message: string): StoreSnapshot {
  return {
    ...createSeedSnapshot(),
    errors: [{ id: 'err_1', command: 'openTab', message, at: 0 }],
  };
}

function render(snapshot: StoreSnapshot): string {
  return renderToStaticMarkup(
    createElement(StoreProvider, {
      store: new MockStore(snapshot),
      children: createElement(CommandErrors),
    }),
  );
}

/** The whole opening tag that carries `marker`, however its attributes are ordered. */
function tagContaining(source: string, marker: string): string {
  const at = source.indexOf(marker);
  expect(at, `no element carries ${marker}`).toBeGreaterThan(-1);
  return source.slice(source.lastIndexOf('<', at), source.indexOf('>', at) + 1);
}

describe('a failure notice carrying a path', () => {
  const markup = render(
    withError(`Nysia could not find its daemon. Build it: ${DAEMON_PATH}`),
  );

  it('prints the path rather than cutting it short', () => {
    // Whatever the wrapping does, the path is what the user has to act on — a fix that
    // truncated it would look tidy and be useless.
    expect(markup).toContain(DAEMON_PATH);
  });

  it('breaks the message inside an unbroken run', () => {
    /*
     * `wrap-anywhere` is `overflow-wrap: anywhere`, and the difference from `break-words`
     * (`overflow-wrap: break-word`) is the one that decides this case: `anywhere` counts the
     * break opportunities when the min-content width is computed, so a flex item carrying a
     * path can actually shrink. `break-words` leaves min-content at the full width of the
     * path, which is why it is not enough here.
     */
    const body = tagContaining(markup, 'text-fg2');
    expect(body, 'the message lost its break-anywhere wrapping').toContain('wrap-anywhere');
    expect(body, '`break-all` breaks ordinary prose mid-word too').not.toContain('break-all');
  });

  it('leaves the heading wrapping on words', () => {
    // `openTab failed` is words. Breaking it anywhere would be a cure applied where there
    // was no disease.
    expect(tagContaining(markup, 'font-semibold')).not.toContain('wrap-anywhere');
  });

  it('lets the text column shrink at all', () => {
    // A flex item's default `min-width: auto` is its min-content width. Without `min-w-0`
    // the item refuses to shrink however the text wraps, and the dismiss button is pushed
    // out of the box instead.
    expect(tagContaining(markup, 'flex-1')).toContain('min-w-0');
  });

  it('caps the column against the window rather than pinning it', () => {
    // 380px beside `left-3.5` reaches the right edge of a narrow window and then past it.
    const column = tagContaining(markup, 'aria-label="Command failures"');
    expect(column).toContain('max-w-[');
  });

  it('caps the height and scrolls instead of growing over the sidebar', () => {
    // The no-timeout decision is about time, not size: a notice that stays until dismissed
    // is right, and one that grows without limit while it stays is not. `nextSteps` is as
    // long as the daemon needs it to be.
    const body = tagContaining(markup, 'text-fg2');
    expect(body).toMatch(/max-h-/);
    expect(body).toContain('overflow-y-auto');
  });

  it('still keeps the notice until it is dismissed', () => {
    // The behaviour the fix was not allowed to change. A launch that failed is something the
    // user has to act on.
    expect(markup).toContain('aria-label="Dismiss openTab failure"');
  });
});
