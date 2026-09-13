import { describe, expect, it } from 'vitest';

import { isArrowKey, nextOption, tabbableIndex } from './roving';

const OPTIONS = ['On', 'Agent', 'Off'] as const;

describe('nextOption', () => {
  it('moves both ways along the group', () => {
    expect(nextOption(OPTIONS, 'On', 'ArrowRight')).toBe('Agent');
    expect(nextOption(OPTIONS, 'Agent', 'ArrowLeft')).toBe('On');
  });

  it('treats the vertical arrows as the same axis', () => {
    expect(nextOption(OPTIONS, 'On', 'ArrowDown')).toBe('Agent');
    expect(nextOption(OPTIONS, 'Agent', 'ArrowUp')).toBe('On');
  });

  it('wraps at both ends', () => {
    expect(nextOption(OPTIONS, 'Off', 'ArrowRight')).toBe('On');
    expect(nextOption(OPTIONS, 'On', 'ArrowLeft')).toBe('Off');
  });

  it('ignores keys that are not arrows', () => {
    for (const key of ['Enter', ' ', 'Tab', 'a', 'Escape']) {
      expect(nextOption(OPTIONS, 'On', key), key).toBeUndefined();
      expect(isArrowKey(key), key).toBe(false);
    }
  });

  it('enters at the first option when the current value is not in the list', () => {
    // A stored preference from a build that spelled an option differently.
    expect(nextOption(OPTIONS, 'Retired' as 'On', 'ArrowRight')).toBe('Agent');
  });

  it('stays put rather than reporting a move on a one-option group', () => {
    expect(nextOption(['Only'] as const, 'Only', 'ArrowRight')).toBeUndefined();
    expect(nextOption([] as readonly string[], 'anything', 'ArrowRight')).toBeUndefined();
  });
});

describe('tabbableIndex', () => {
  it('puts the tab stop on the selected option', () => {
    expect(tabbableIndex(OPTIONS, 'On')).toBe(0);
    expect(tabbableIndex(OPTIONS, 'Off')).toBe(2);
  });

  it('keeps the group reachable when the stored value is not in the list', () => {
    // Otherwise every option sits at tabIndex -1 and the control drops out of the tab
    // order entirely, with no keyboard route back in.
    expect(tabbableIndex(OPTIONS, 'Retired' as 'On')).toBe(0);
  });

  it('always names exactly one option', () => {
    for (const value of [...OPTIONS, 'Retired' as 'On']) {
      const index = tabbableIndex(OPTIONS, value);
      expect(index, value).toBeGreaterThanOrEqual(0);
      expect(index, value).toBeLessThan(OPTIONS.length);
    }
  });
});
