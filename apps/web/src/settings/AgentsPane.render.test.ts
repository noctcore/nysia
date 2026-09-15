import { createElement, type ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AgentsPane } from './AgentsPane';
import type { InstalledAgent } from './installedAgents';

/*
 * The Agents pane, rendered node-only (D-18).
 *
 * `renderToStaticMarkup` needs no DOM and runs no effects, so this is still the shared
 * vitest project — and it buys the two things the unit tests beside it cannot see: that the
 * pane does not throw on first paint, and that what it draws is the Claude slice rather
 * than the artboard's nine chips and four invented versions. The absence of something is
 * exactly the kind of decision that regresses silently when the next provider lands, so it
 * is asserted rather than trusted to a comment.
 *
 * Written with `createElement` rather than JSX so the file stays `.test.ts` and inside the
 * `src/**\/*.test.ts` glob the shared config defines — the same reason `App.render.test.ts`
 * is written that way.
 */
function render(node: ReactElement): string {
  return renderToStaticMarkup(node);
}

/** The slice between a marker and the first following terminator. */
function between(source: string, from: string, to: string): string {
  const start = source.indexOf(from);
  const end = source.indexOf(to, start);
  return source.slice(start, end === -1 ? undefined : end);
}

/**
 * The opening tag of the button that *encloses* `marker`.
 *
 * Not the nearest preceding tag: every label here sits inside its button rather than in an
 * attribute, so a nearest-tag walk finds the `</span>` in front of the text and reports a
 * button with no attributes at all — which is an assertion that passes for the wrong reason
 * in one direction and fails for the wrong reason in the other.
 */
function enclosingButton(source: string, marker: string): string {
  const at = source.indexOf(marker);
  const open = source.lastIndexOf('<button', at);
  return source.slice(open, source.indexOf('>', open) + 1);
}

/**
 * A `Storage` the node test can hand the pane.
 *
 * An object literal rather than a class: `lib.dom`'s `Storage` carries an index signature
 * typed `any`, which `implements` would force this file to spell, and `any` is banned
 * repo-wide. A literal satisfies the named members and the index signature costs nothing.
 */
function memoryStorage(entries: Record<string, string>): Storage {
  const held = new Map(Object.entries(entries));
  return {
    get length() {
      return held.size;
    },
    clear: () => held.clear(),
    getItem: (key: string) => held.get(key) ?? null,
    key: (index: number) => [...held.keys()][index] ?? null,
    removeItem: (key: string) => {
      held.delete(key);
    },
    setItem: (key: string, value: string) => {
      held.set(key, value);
    },
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('the Agents pane', () => {
  const pane = render(createElement(AgentsPane));

  it('offers only agents this build can launch', () => {
    // The whole point of the v0.2 delivery plan §1. The artboard draws nine chips; two
    // correspond to something that exists — Claude, and the blank terminal that is half of
    // `SessionKind`. The other seven are absent, not disabled and not greyed, until v0.6.
    const chips = between(pane, 'aria-label="Default agent"', '</div>');
    expect(chips.match(/role="radio"/g)).toHaveLength(2);
    expect(chips).toContain('Claude');
    expect(chips).toContain('No agent (blank terminal)');
    for (const absent of ['Auto', 'Teams', 'Codex', 'Grok', 'OpenCode', 'Gemini', 'Cursor']) {
      expect(chips, absent).not.toContain(absent);
    }
    // And nowhere else on the pane either — an absent agent must not come back as a row.
    for (const absent of ['Codex', 'Grok', 'OpenCode', 'Gemini', 'Cursor']) {
      expect(pane, absent).not.toContain(absent);
    }
  });

  it('gives the chip group one tab stop, like the segmented controls beside it', () => {
    // A radiogroup where every radio is tabbable is not the APG pattern, and it is the
    // half of it that is easy to leave out — `ui/roving.ts` carries the rule, and this is
    // the check that the chip group is actually using it.
    const chips = between(pane, 'aria-label="Default agent"', '</div>');
    const radios = [...chips.matchAll(/<button[^>]*>/g)].map((match) => match[0]);
    expect(radios).toHaveLength(2);
    expect(radios.filter((tag) => tag.includes('tabindex="0"'))).toHaveLength(1);
  });

  it('selects Claude, and says the choice is a client preference', () => {
    const claude = enclosingButton(pane, 'Claude');
    expect(claude).toContain('aria-checked="true"');
    expect(pane).toContain('still validated when it launches');
  });

  it('draws the five setting rows §5 specifies, with their controls', () => {
    for (const label of [
      'Agent status hooks',
      'Auto-generate tab titles',
      'Keep computer awake',
      'Prompt cache timer',
      'Agent permissions',
    ]) {
      expect(pane, label).toContain(label);
    }
    // Three toggles and two segmented groups, plus the chip group.
    expect(pane.match(/role="switch"/g)).toHaveLength(3);
    expect(pane.match(/role="radiogroup"/g)).toHaveLength(3);
    for (const segment of ['On', 'Agent', 'Off', 'Yolo', 'Manual']) {
      expect(pane, segment).toContain(`>${segment}</button>`);
    }
  });

  it('ships the artboard default states, on and off', () => {
    // Two on (status hooks, tab titles), one off (prompt cache timer). A pane that stored
    // the wrong default would still render, so the count is the assertion.
    expect(pane.match(/role="switch"[^>]*aria-checked="true"/g)).toHaveLength(2);
    expect(pane.match(/role="switch"[^>]*aria-checked="false"/g)).toHaveLength(1);
  });

  it('says out loud that the status-hooks toggle removes nothing yet', () => {
    // The row's own copy promises "turn off to remove managed hooks", and the command
    // surface that would do the removing is wave C. A control that looks like it works and
    // does not is worse than one that says what it is, so the caveat is on screen rather
    // than in a release note — and here, so it cannot quietly disappear.
    expect(pane).toContain('records the choice today');
    expect(pane).toContain('v0.2 wave C');
  });

  it('detects nothing, and neither invents a version nor pretends to refresh', () => {
    expect(pane).toContain('0 detected');
    expect(pane).toContain('Nothing detected');
    // No `2.1.263`-shaped string anywhere: the artboard's four versions were measurements
    // taken on someone else's machine, and shipping one as detected is a fabricated record.
    expect(pane).not.toMatch(/\d+\.\d+\.\d+/);

    const refresh = enclosingButton(pane, 'Refresh');
    expect(refresh).toContain('disabled');
    expect(refresh).toMatch(/title="[^"]*wave C[^"]*"/);
  });

  it('paints no colour the theme switcher cannot reach', () => {
    expect([...pane.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});

describe('the Agents pane, once something is detected', () => {
  // The populated path, exercised now rather than first run on the day the daemon starts
  // answering: when detection lands, `detectInstalledAgents` is the only thing that changes.
  const claude: InstalledAgent = { id: 'claude', name: 'Claude', version: '2.1.263' };
  const pane = render(createElement(AgentsPane, { installed: [claude] }));

  it('states a total that is a fact about the list beneath it', () => {
    expect(pane).toContain('1 detected');
    expect(pane).not.toContain('Nothing detected');
  });

  it('shows the name and the version the agent reported', () => {
    expect(pane).toContain('Claude');
    expect(pane).toContain('2.1.263');
    expect(pane).toContain('font-mono');
  });

  it('marks the row that is already the default rather than offering to set it', () => {
    expect(pane).toContain('Default');
    expect(pane).not.toContain('Set default');
  });
});

describe('the Agents pane, reading a stored preference', () => {
  it('opens on the choice that was saved, not on the default', () => {
    // Persistence is load-bearing here: it is the only thing the status-hooks toggle does
    // in v0.2. Reading it back through the pane proves the whole path — `settingsStorage`,
    // `loadAgents`, the control's checked state — rather than just `parseAgents`.
    vi.stubGlobal(
      'localStorage',
      memoryStorage({ 'nysia.agents': JSON.stringify({ defaultAgent: 'none' }) }),
    );
    const pane = render(createElement(AgentsPane, { installed: [] }));

    const blank = enclosingButton(pane, 'No agent (blank terminal)');
    expect(blank).toContain('aria-checked="true"');
    expect(enclosingButton(pane, 'Claude')).toContain('aria-checked="false"');
  });

  it('offers Set default on a detected agent that is not the current choice', () => {
    vi.stubGlobal(
      'localStorage',
      memoryStorage({ 'nysia.agents': JSON.stringify({ defaultAgent: 'none' }) }),
    );
    const pane = render(
      createElement(AgentsPane, {
        installed: [{ id: 'claude', name: 'Claude', version: '2.1.263' }],
      }),
    );

    expect(pane).toContain('Set default');
  });
});
