import { describe, expect, it } from 'vitest';

import {
  AGENT_CHOICES,
  AGENT_IDS,
  DEFAULT_AGENTS,
  DEFAULT_AGENT_CHOICES,
  parseAgents,
} from './agentPreferences';

describe('parseAgents', () => {
  it('round-trips a full preference set', () => {
    const stored = {
      ...DEFAULT_AGENTS,
      defaultAgent: 'none',
      statusHooks: false,
      keepAwake: 'Off',
      promptCacheTimer: true,
      agentPermissions: 'Manual',
    };
    expect(parseAgents(JSON.stringify(stored))).toEqual(stored);
  });

  it('falls back to the default when nothing is stored', () => {
    expect(parseAgents(null)).toEqual(DEFAULT_AGENTS);
  });

  it('survives anything that is not a preference object', () => {
    for (const bad of ['', 'not json', 'null', '42', '"a string"', '[]']) {
      expect(parseAgents(bad), bad).toEqual(DEFAULT_AGENTS);
    }
  });

  it('replaces only the fields it cannot trust', () => {
    // A settings file edited by hand, or written by a build that spelled an option
    // differently. A stray string reaching a segmented control renders a group with
    // nothing selected, which looks like a bug in the control rather than in the data.
    const parsed = parseAgents(
      JSON.stringify({
        defaultAgent: 'codex',
        statusHooks: 'yes',
        autoTabTitles: false,
        keepAwake: 'Agent',
        agentPermissions: 7,
      }),
    );
    expect(parsed.defaultAgent).toBe(DEFAULT_AGENTS.defaultAgent);
    expect(parsed.statusHooks).toBe(DEFAULT_AGENTS.statusHooks);
    expect(parsed.autoTabTitles).toBe(false);
    expect(parsed.keepAwake).toBe('Agent');
    expect(parsed.agentPermissions).toBe(DEFAULT_AGENTS.agentPermissions);
  });

  it('refuses an agent this build does not have', () => {
    // The case the delivery plan §1 is about. A preference file carried back from a build
    // that shipped Codex — or forward from v0.6 — must not leave the pane defaulting new
    // tabs to an agent that cannot launch. It resolves to Claude, which can.
    for (const absent of ['auto', 'codex', 'gemini', 'grok', 'opencode', 'cursor', 'teams']) {
      expect(parseAgents(JSON.stringify({ defaultAgent: absent })).defaultAgent, absent).toBe(
        'claude',
      );
    }
  });
});

describe('the agent roster', () => {
  it('offers exactly one agent, per D-3', () => {
    // Not a style assertion. The artboard draws nine chips; this is the check that fails
    // if someone restores the eight that correspond to nothing, rather than waiting for
    // v0.6 when the second provider — and the trait extracted from two real ones — lands.
    expect(AGENT_IDS).toEqual(['claude']);
  });

  it('gives every stored choice a chip, and every chip a stored choice', () => {
    // The two lists are what a preference is validated against and what the pane renders.
    // Either one growing alone is a choice that cannot be picked, or a chip that cannot
    // be saved.
    expect(AGENT_CHOICES.map((choice) => choice.id)).toEqual([...DEFAULT_AGENT_CHOICES]);
  });
});
