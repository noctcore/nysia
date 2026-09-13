import { describe, expect, it } from 'vitest';

/*
 * A tripwire on the close button's mousedown guard, read out of the source.
 *
 * The guard is one `preventDefault()` call, and everything about it invites removal: the
 * body does nothing visible, the handler looks redundant next to `onClick`, and no other
 * gate notices it going. `tabIndex={-1}` leaves the button click-focusable in Chromium and
 * WebView2, so without it mousedown moves `document.activeElement` onto the button before
 * `onClick` runs and the focus test in `onClick` becomes a tautology that answers "yes"
 * every time. Closing a tab with the pointer then drags the caret out of wherever it was.
 *
 * That failure is invisible to everything this repo can otherwise run. The render test
 * uses static markup and dispatches no events; a synthetic `element.click()` moves no focus
 * either, so a probe driving the UI that way reports success whether the guard is present
 * or not. The only honest check is a real pointer event, which needs a browser this project
 * deliberately does not run in CI (D-18).
 *
 * So this reads the file instead. It is a weaker check than behaviour — it proves the code
 * is written, not that it works — but it turns "a later tidy-up deletes this and every gate
 * stays green" into "a later tidy-up deletes this and a test names it". `colourGuard.test.ts`
 * already reads raw source the same way, so the mechanism is not new here.
 */
const source = String(
  Object.values(
    import.meta.glob('./TabStrip.tsx', { query: '?raw', import: 'default', eager: true }),
  )[0],
);

/** The close button's JSX, from its `type` attribute to the end of the opening tag. */
function closeButtonJsx(): string {
  const start = source.indexOf('aria-label={`Close ');
  expect(start, 'the close button moved or was renamed').toBeGreaterThan(-1);
  const open = source.lastIndexOf('<button', start);
  const end = source.indexOf('\n      >', start);
  return source.slice(open, end === -1 ? undefined : end);
}

describe('the tab close button', () => {
  it('is read from a file that exists and holds the strip', () => {
    // Without this the cases below pass vacuously against an empty string if the glob ever
    // stops resolving.
    expect(source.length).toBeGreaterThan(1000);
    expect(source).toContain('role="tablist"');
  });

  it('suppresses the default on mousedown', () => {
    const jsx = closeButtonJsx();
    expect(jsx, 'the close button lost its onMouseDown handler').toContain('onMouseDown');
    expect(
      jsx,
      'the close button has an onMouseDown that does not call preventDefault',
    ).toMatch(/onMouseDown[\s\S]*?event\.preventDefault\(\)/);
  });

  it('stays out of the tab order, which is what makes the guard necessary', () => {
    // The two belong together: `tabIndex={-1}` is why the button is click-focusable but
    // not tab-focusable, and the guard is what stops the click half moving the caret. If
    // the button ever becomes a real tab stop this case should be revisited, not deleted.
    expect(closeButtonJsx()).toContain('tabIndex={-1}');
  });

  it('reads the focus question before issuing the command', () => {
    // The node is unmounted by the time `onSettled` runs, so asking then would be asking
    // about an element that no longer exists.
    const jsx = source.slice(source.indexOf('onClick={(event) => {', source.indexOf('aria-label={`Close ')));
    const asked = jsx.indexOf('document.activeElement');
    const issued = jsx.indexOf('commands.closeTab');
    expect(asked).toBeGreaterThan(-1);
    expect(issued).toBeGreaterThan(-1);
    expect(asked, 'the focus test moved after the command').toBeLessThan(issued);
  });
});
