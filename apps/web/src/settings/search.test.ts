import { describe, expect, it } from 'vitest';

import { SETTINGS_TREE } from './nav';
import { matches, matchingGroups } from './search';

describe('matches', () => {
  it('ignores case and surrounding space', () => {
    expect(matches('Appearance', 'appear')).toBe(true);
    expect(matches('Appearance', '  APPEAR ')).toBe(true);
  });

  it('treats an empty query as matching everything', () => {
    expect(matches('Anything', '')).toBe(true);
    expect(matches('Anything', '   ')).toBe(true);
  });

  it('does not match on something absent', () => {
    expect(matches('Appearance', 'terminal')).toBe(false);
  });
});

describe('matchingGroups', () => {
  it('returns the whole tree for an empty query', () => {
    expect(matchingGroups('')).toBe(SETTINGS_TREE);
  });

  it('keeps only the entries that matched, and drops the groups left empty', () => {
    const groups = matchingGroups('appearance');
    expect(groups).toHaveLength(1);
    expect(groups[0]?.entries.map((entry) => entry.label)).toEqual(['Appearance']);
  });

  it('keeps a whole group when the group name itself matches', () => {
    // Someone typing a section name wants the section, not nothing: no entry under
    // "Workflows" contains the word.
    const groups = matchingGroups('workflows');
    expect(groups).toHaveLength(1);
    expect(groups[0]?.entries.length).toBeGreaterThan(1);
  });

  it('spans groups when a query matches in more than one', () => {
    const groups = matchingGroups('a');
    expect(groups.length).toBeGreaterThan(1);
  });

  it('returns nothing rather than an empty heading when nothing matches', () => {
    expect(matchingGroups('zzzznotasetting')).toEqual([]);
  });
});
