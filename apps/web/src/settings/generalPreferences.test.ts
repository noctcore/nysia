import { describe, expect, it } from 'vitest';

import { DEFAULT_GENERAL, parseGeneral } from './generalPreferences';

describe('parseGeneral', () => {
  it('round-trips a full preference set', () => {
    const stored = {
      ...DEFAULT_GENERAL,
      model: 'Sonnet 5',
      thinkingEffort: 'low',
      permissionMode: 'Manual',
      completionSound: true,
    };
    expect(parseGeneral(JSON.stringify(stored))).toEqual(stored);
  });

  it('falls back to the default when nothing is stored', () => {
    expect(parseGeneral(null)).toEqual(DEFAULT_GENERAL);
  });

  it('survives anything that is not a preference object', () => {
    for (const bad of ['', 'not json', 'null', '42', '"a string"', '[]']) {
      expect(parseGeneral(bad), bad).toEqual(DEFAULT_GENERAL);
    }
  });

  it('replaces only the fields it cannot trust', () => {
    // A hand-edited settings file, or one written by a build that spelled an option
    // differently. A stray string reaching a segmented control renders a group with
    // nothing selected, which looks like a bug in the control.
    const parsed = parseGeneral(
      JSON.stringify({
        model: 'Some Retired Model',
        thinkingEffort: 'high',
        permissionMode: 7,
        sessionRecaps: 'yes',
        completionSound: true,
      }),
    );
    expect(parsed.model).toBe(DEFAULT_GENERAL.model);
    expect(parsed.thinkingEffort).toBe('high');
    expect(parsed.permissionMode).toBe(DEFAULT_GENERAL.permissionMode);
    expect(parsed.sessionRecaps).toBe(DEFAULT_GENERAL.sessionRecaps);
    expect(parsed.completionSound).toBe(true);
  });
});
