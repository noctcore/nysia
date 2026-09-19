import ts from 'typescript';
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
 * strip reaches `closeTab` itself and never goes through the gate. That wiring is inside a
 * component with hooks, which a node-only suite cannot press (D-18), so it is read out of the
 * source like the mousedown guard above: weaker than behaviour, and it still turns "someone
 * restored the direct call and every gate stayed green" into a named failure.
 *
 * The check fails on any identifier or string literal in `TabStrip.tsx` whose value is
 * `closeTab`. The value, not the text: TypeScript unescapes both, so `close\u0054ab` is the
 * same identifier and `'close\x54ab'` the same string. That takes in a direct call, an alias,
 * a destructure (renamed, shorthand or computed), an optional call, `.call`, and a bracket
 * holding a plain string, an escaped one or a template with no substitution — `SPELLED` below
 * plants each of them. A comment is not an identifier, so one that names `closeTab` does not
 * trip it.
 *
 * It parses the file rather than matching characters, because both cheaper readers came out
 * weaker than the plain `closeTab(` probe they would replace. A regex that stripped comments
 * before looking could not see strings, so a `//` inside a URL, or a `/*` inside a line
 * comment, hid a direct call from it. A bare `ts.createScanner` loop knows strings from
 * comments but not a regex literal from a division, which is the parser's call, so a quote
 * inside `/'/` hid one too; and on this file it lost its place at the first template
 * substitution and read the whole close button as one string. Those inputs are planted below,
 * and the close button is a plant site because of the second. `colourGuard.ts` walks a syntax
 * tree for its fourth rule for the same reason.
 *
 * What it cannot see, each pinned as staying green by a case in `BEYOND`:
 *
 *  - a name built at runtime — `'close' + 'Tab'`, a template with a substitution,
 *    `Reflect.get` on a built string. No single token spells `closeTab`, and seeing through
 *    these takes evaluating the code, not reading it;
 *  - a helper in another file that closes the tab under a name of its own. This reads one
 *    file and nothing it imports.
 */

/** One parse of a source: the names the check reads, and the callee of each call. */
interface Reading {
  /** The value of each identifier and string literal, in source order. */
  readonly names: readonly string[];
  /** Each call's callee, as `gate.request` or `requestClose`, for the floor. */
  readonly calls: readonly string[];
}

/**
 * Parse `text` as the strip and read it.
 *
 * Walked for names: `Identifier`, and `StringLiteralLike` — a string literal, or a template
 * with no substitution. Not walked, on purpose:
 *
 *  - comments, which are exactly what the check must not read;
 *  - JSX text, which is words on the screen, not a name the code looks anything up by;
 *  - a private name such as `#closeTab`, which names a class's own member and cannot reach
 *    the store's;
 *  - the pieces of a template with a substitution, which only spell a name once evaluated —
 *    the first limit above.
 */
function read(text: string): Reading {
  const file = ts.createSourceFile(
    'TabStrip.tsx',
    text,
    ts.ScriptTarget.Latest,
    false,
    ts.ScriptKind.TSX,
  );
  const names: string[] = [];
  const calls: string[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isIdentifier(node) || ts.isStringLiteralLike(node)) {
      names.push(node.text);
    } else if (ts.isCallExpression(node)) {
      calls.push(calleeName(node.expression));
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return { names, calls };
}

/** `gate.request` for a method on a bare name, the name for a plain call, `''` otherwise. */
function calleeName(callee: ts.Expression): string {
  if (ts.isIdentifier(callee)) {
    return callee.text;
  }
  if (ts.isPropertyAccessExpression(callee) && ts.isIdentifier(callee.expression)) {
    return `${callee.expression.text}.${callee.name.text}`;
  }
  return '';
}

function namesCloseTab(text: string): boolean {
  return read(text).names.includes('closeTab');
}

/** The parser's complaints about `text`, so a plant is known to be code the file would take. */
function syntaxErrors(text: string): readonly string[] {
  const { diagnostics = [] } = ts.transpileModule(text, {
    fileName: 'TabStrip.tsx',
    reportDiagnostics: true,
    compilerOptions: { jsx: ts.JsxEmit.Preserve },
  });
  return diagnostics.map((diagnostic) =>
    ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n'),
  );
}

/**
 * The two closes, each found by the gate call it makes.
 *
 * A case is planted just before that call, inside the handler. Planting after the end of the
 * file proves only that the end of the file is read: a stripper whose block pattern was made
 * greedy deleted most of the body and still passed every case planted there.
 */
const SITES = [
  {
    where: 'the Delete branch',
    after: "event.key === 'Delete'",
    call: 'gate.request(tab, ',
    callee: 'gate.request',
  },
  {
    where: "the close button's onClick",
    after: 'aria-label={`Close ',
    call: 'requestClose(tab, ',
    callee: 'requestClose',
  },
] as const;
type Site = (typeof SITES)[number];
const [DELETE_BRANCH, CLOSE_BUTTON] = SITES;

function plant(snippet: string, site: Site): string {
  const from = source.indexOf(site.after);
  expect(from, `${site.where} moved or was renamed`).toBeGreaterThan(-1);
  const at = source.indexOf(site.call, from);
  expect(at, `${site.where} no longer calls ${site.callee}`).toBeGreaterThan(-1);
  const planted = `${source.slice(0, at)}${snippet}\n${source.slice(at)}`;
  expect(syntaxErrors(planted), 'the plant is not valid TSX').toEqual([]);
  return planted;
}

interface Case {
  readonly name: string;
  readonly snippet: string;
}

/**
 * The escaped spellings, backslashes doubled so each snippet carries the escape itself.
 *
 * Held to that below: an escape collapsed to the letter it stands for is a plain `closeTab`,
 * which trips the check while proving nothing about escapes.
 */
const ESCAPED: readonly Case[] = [
  {
    name: 'a bracket holding an escaped string',
    snippet: "commands['close\\x54ab'](tab.paneKey);",
  },
  { name: 'an identifier escape', snippet: 'commands.close\\u0054ab(tab.paneKey);' },
  { name: 'a code point escape', snippet: 'commands.close\\u{54}ab(tab.paneKey);' },
];

/** Spellings that put `closeTab` in one identifier or one string literal. Each must trip. */
const SPELLED: readonly Case[] = [
  { name: 'a direct call', snippet: 'commands.closeTab(tab.paneKey);' },
  { name: 'an alias', snippet: 'const shut = commands.closeTab; shut(tab.paneKey);' },
  {
    name: 'a renaming destructure',
    snippet: 'const { closeTab: shut } = commands; shut(tab.paneKey);',
  },
  {
    name: 'a shorthand destructure',
    snippet: 'const { closeTab } = commands; closeTab(tab.paneKey);',
  },
  {
    name: 'a computed destructure',
    snippet: "const { ['closeTab']: shut } = commands; shut(tab.paneKey);",
  },
  { name: 'an optional call', snippet: 'commands.closeTab?.(tab.paneKey);' },
  { name: '.call', snippet: 'commands.closeTab.call(commands, tab.paneKey);' },
  { name: 'a bracket holding a string', snippet: "commands['closeTab'](tab.paneKey);" },
  { name: 'a bracket holding a plain template', snippet: 'commands[`closeTab`](tab.paneKey);' },
  ...ESCAPED,
  // The three inputs that hid a direct call from a comment stripper blind to strings.
  {
    name: 'a call after a URL',
    snippet: "window.open('https://nysia.dev/help'); commands.closeTab(tab.paneKey);",
  },
  {
    name: 'a call after a glob in a line comment',
    snippet: '// Ctrl+W, the way src/*.ts handles it elsewhere\ncommands.closeTab(tab.paneKey);',
  },
  {
    name: 'a call after a glob in a string',
    snippet: "window.open('/docs/*');\ncommands.closeTab(tab.paneKey);",
  },
  // The two that hid one from a bare scanner, which cannot tell a regex from a division.
  {
    name: 'a call after a regex holding a quote',
    snippet: "if (/'/.test(tab.title)) commands.closeTab(tab.paneKey);",
  },
  {
    name: 'a call after a regex holding a glob',
    snippet: 'const glob = /src\\/*.ts/; commands.closeTab(tab.paneKey);',
  },
];

/** Text that names `closeTab` without being code that reaches it. Each must stay green. */
const NOT_CODE: readonly Case[] = [
  { name: 'a line comment', snippet: '// commands.closeTab(tab.paneKey);' },
  { name: 'a block comment', snippet: '/* commands.closeTab(tab.paneKey); */' },
  { name: 'a string that mentions it', snippet: "const note = 'closeTab goes through the gate';" },
  { name: 'a longer name', snippet: 'commands.closeTabs?.(tab.paneKey);' },
];

/** The documented limit: code that does reach `closeTab`, which the check cannot see. */
const BEYOND: readonly Case[] = [
  {
    name: 'a name joined at runtime',
    snippet: "(Reflect.get(commands, 'close' + 'Tab') as (k: string) => void)(tab.paneKey);",
  },
  {
    name: 'a template with a substitution',
    snippet: "commands[`close${'Tab'}` as const](tab.paneKey);",
  },
  { name: 'a helper from another file', snippet: 'closeWithoutAsking(commands, tab);' },
];

describe('closing a tab', () => {
  it('names closeTab in no identifier and no string literal', () => {
    expect(namesCloseTab(source), 'the strip reaches closeTab without the stop gate').toBe(false);
  });

  it('parses without a syntax error, so the plants below start from the real file', () => {
    expect(syntaxErrors(source)).toEqual([]);
  });

  it.each(SITES)('reads as far as the gate call in $where', ({ callee }) => {
    // The floor. A reader that silently dropped most of the file would find no `closeTab`
    // in what was left and pass the case above by reading nothing.
    expect(read(source).calls).toContain(callee);
  });

  it('fails the floor when the reader drops the middle of the file', () => {
    // Trap 12 for the floor. This is the mutant that sank the comment stripper: its block
    // pattern made greedy, deleting from the first comment opener to the last closer. In
    // front of this reader it takes the Delete branch with it, and the floor has to notice.
    const mangled = source.replace(/\/\*[\s\S]*\*\//g, '');
    expect(read(mangled).calls).not.toContain(DELETE_BRANCH.callee);
  });

  it('fails the floor when the reader stops before the close button', () => {
    // A reader that loses its place partway — a bare scanner does, on this file, at the
    // first template substitution — never reaches the close button's gate call.
    const cut = source.indexOf('function TabButton');
    expect(cut, 'TabButton moved or was renamed').toBeGreaterThan(-1);
    expect(read(source.slice(0, cut)).calls).not.toContain(CLOSE_BUTTON.callee);
  });

  const spellingsAtSites = SITES.flatMap((site) =>
    SPELLED.map((spelled) => ({ ...spelled, site, where: site.where })),
  );
  it.each(spellingsAtSites)('trips on $name in $where', ({ snippet, site }) => {
    expect(namesCloseTab(plant(snippet, site))).toBe(true);
  });

  it.each(ESCAPED)('plants $name as the escape, not the letter', ({ snippet }) => {
    expect(snippet).toContain('\\');
    expect(snippet).not.toContain('closeTab');
  });

  it.each(NOT_CODE)('stays green on $name', ({ snippet }) => {
    expect(namesCloseTab(plant(snippet, DELETE_BRANCH))).toBe(false);
  });

  it.each(BEYOND)('cannot see $name, which is the documented limit', ({ snippet }) => {
    // Green on purpose. If one of these starts tripping, the check got stronger and both doc
    // comments that name the limit are out of date.
    expect(namesCloseTab(plant(snippet, DELETE_BRANCH))).toBe(false);
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
