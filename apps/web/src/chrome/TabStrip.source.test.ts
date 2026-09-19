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

  it('reads the focus question before issuing the request', () => {
    // The node is unmounted by the time `onSettled` runs, so asking then would be asking
    // about an element that no longer exists.
    const jsx = source.slice(source.indexOf('onClick={(event) => {', source.indexOf('aria-label={`Close ')));
    const asked = jsx.indexOf('document.activeElement');
    const issued = jsx.indexOf('requestClose(tab');
    expect(asked).toBeGreaterThan(-1);
    expect(issued).toBeGreaterThan(-1);
    expect(asked, 'the focus test moved after the request').toBeLessThan(issued);
  });
});

/*
 * The strip's two closes, and the gate between them and the session.
 *
 * `stopAgent.test.ts` and `StopAgentDialog.render.test.ts` prove the gate asks before a
 * working agent is closed and that Cancel leaves it running — which proves nothing if the
 * strip names `closeTab` itself and never reaches the gate. That wiring is inside a component
 * with hooks, which a node-only suite cannot press (D-18), so it is read out of the source
 * like the mousedown guard above: weaker than behaviour, and it still turns "someone restored
 * a spelling of the identifier and every gate stayed green" into a named failure.
 *
 * The check is the identifier, not a call. An alias, a destructure, a bracket, an optional
 * call and `.call` still write `closeTab`. A helper in another file does not, and a
 * single-file read cannot see it.
 */
const CLOSE_TAB = /\bcloseTab\b/;

/** Drop line and block comments so a doc comment that names the identifier does not trip. */
function withoutComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '');
}

function namesCloseTab(src: string): boolean {
  return CLOSE_TAB.test(withoutComments(src));
}

describe('closing a tab', () => {
  it('does not name closeTab', () => {
    expect(namesCloseTab(source), 'the strip closes a tab without going through the stop gate').toBe(
      false,
    );
  });

  it('trips on each in-file spelling of the identifier', () => {
    // Trap 12: the five spellings a `closeTab(` probe let through, plus the direct call that
    // probe already caught. Each is planted in a copy of the source the check reads, so a
    // guard that only plants the case it already caught goes red here rather than shipping.
    const plants: readonly { name: string; snippet: string }[] = [
      { name: 'variable alias', snippet: 'const shut = commands.closeTab; shut(tab.paneKey)' },
      { name: 'destructure-rename', snippet: 'const { closeTab: shut } = commands' },
      { name: 'bracket access', snippet: 'commands["closeTab"](tab.paneKey)' },
      { name: 'optional call', snippet: 'commands.closeTab?.(tab.paneKey)' },
      { name: '.call', snippet: 'commands.closeTab.call(commands, tab.paneKey)' },
      { name: 'direct call', snippet: 'commands.closeTab(tab.paneKey)' },
    ];
    for (const { name, snippet } of plants) {
      expect(namesCloseTab(`${source}\n${snippet}\n`), name).toBe(true);
    }
  });

  it('does not trip on a comment that names closeTab', () => {
    expect(namesCloseTab(`${source}\n// closeTab\n`)).toBe(false);
    expect(namesCloseTab(`${source}\n/* closeTab */\n`)).toBe(false);
  });

  it('sends the close button through the gate', () => {
    const jsx = source.slice(source.indexOf('onClick={(event) => {', source.indexOf('aria-label={`Close ')));
    expect(jsx).toContain('requestClose(tab, ');
    expect(source).toContain('requestClose={gate.request}');
  });

  it('sends Delete and Backspace through the gate', () => {
    const start = source.indexOf("event.key === 'Delete'");
    expect(start, 'the Delete branch moved or was renamed').toBeGreaterThan(-1);
    const branch = source.slice(start, source.indexOf('\n    }\n', start));
    expect(branch).toContain('gate.request(tab, ');
  });

  it('mounts the question outside the tablist', () => {
    // The tablist holds nothing but tabs (`App.render.test.ts`); a dialog inside it would be
    // a child a screen reader is told is a tab.
    const tablistEnd = source.indexOf('</div>', source.indexOf('role="tablist"'));
    expect(source.indexOf('<StopAgentDialog')).toBeGreaterThan(tablistEnd);
  });

  it('shows the question only through openQuestion', () => {
    // `stopAgent.test.ts` holds `openQuestion` to dropping a question whose session left the
    // strip or whose pane key was reused; that is only true of the strip if it asks it.
    expect(source).toMatch(/openQuestion\(\s*useSyncExternalStore\(gate\.subscribe/);
    expect(source).toContain('pending={question}');
  });
});
