import { describe, expect, it } from 'vitest';

import { ACCENT_PRESETS, STATUS_TOKENS, THEMES, THEME_NAMES } from './themes';
import {
  THEMED_VARIABLES,
  applyThemeVariables,
  cssVariablesFor,
  type StyleTarget,
} from './tokens';

function recordingTarget(): { target: StyleTarget; written: Map<string, string> } {
  const written = new Map<string, string>();
  return {
    written,
    target: {
      setProperty(property, value) {
        written.set(property, value);
      },
    },
  };
}

describe('cssVariablesFor', () => {
  it('emits every themed variable for every theme', () => {
    for (const theme of THEME_NAMES) {
      const variables = cssVariablesFor(theme, '#f2b35b');
      expect(Object.keys(variables).sort()).toEqual([...THEMED_VARIABLES].sort());
      for (const name of THEMED_VARIABLES) {
        expect(variables[name], `${theme} ${name}`).not.toBe('');
      }
    }
  });

  it('swaps every surface token when the theme changes', () => {
    const ember = cssVariablesFor('Ember', '#f2b35b');
    const graphite = cssVariablesFor('Graphite', '#f2b35b');

    // Not one surface value survives the switch — that is what makes Graphite a theme
    // rather than a tint.
    for (const name of THEMED_VARIABLES) {
      if (name.startsWith('--color-acc')) {
        expect(graphite[name], name).toBe(ember[name]);
      } else {
        expect(graphite[name], name).not.toBe(ember[name]);
      }
    }
    expect(ember['--color-bg0']).toBe(THEMES.Ember.bg0);
    expect(graphite['--color-fg3']).toBe(THEMES.Graphite.fg3);
  });

  it('swaps only the accent chain when the accent changes', () => {
    const amber = cssVariablesFor('Ember', '#f2b35b');
    const violet = cssVariablesFor('Ember', '#b79cf2');
    for (const name of THEMED_VARIABLES) {
      if (name.startsWith('--color-acc')) {
        expect(violet[name], name).not.toBe(amber[name]);
      } else {
        expect(violet[name], name).toBe(amber[name]);
      }
    }
  });

  it('never emits a status variable, so the accent picker cannot recolour semantics', () => {
    // The switcher writes twelve names and none of them is a status token: there is no
    // code path from an accent to agent lifecycle colour. (A *value* collision is fine and
    // does happen — Graphite's fg3 is the same grey as the queued status — which is why
    // this asserts on names, and on the accent chain never landing on a status colour.)
    const statuses = new Set<string>(Object.values(STATUS_TOKENS));
    for (const theme of THEME_NAMES) {
      for (const preset of ACCENT_PRESETS) {
        const emitted = cssVariablesFor(theme, preset.value);
        expect(Object.keys(emitted).some((name) => name.includes('status'))).toBe(false);
        for (const name of THEMED_VARIABLES) {
          if (name.startsWith('--color-acc')) {
            expect(statuses.has(emitted[name]), `${preset.value} landed on a status colour`).toBe(
              false,
            );
          }
        }
      }
    }
  });
});

describe('applyThemeVariables', () => {
  it('writes the complete set, so no token survives the previous theme', () => {
    const { target, written } = recordingTarget();

    applyThemeVariables(target, 'Ember', '#f2b35b');
    expect([...written.keys()].sort()).toEqual([...THEMED_VARIABLES].sort());
    const ember = new Map(written);

    applyThemeVariables(target, 'Graphite', '#6fd6c8');
    for (const name of THEMED_VARIABLES) {
      expect(written.get(name), name).not.toBe(ember.get(name));
    }
    expect(written.get('--color-acc14')).toBe('#6fd6c824');
  });
});
