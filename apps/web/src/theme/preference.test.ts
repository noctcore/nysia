import { describe, expect, it } from 'vitest';

import { DEFAULT_APPEARANCE, loadAppearance, saveAppearance } from './preference';

/** The two methods `preference.ts` touches, plus a switch for the failure modes. */
function fakeStorage(seed: Record<string, string> = {}, throws = false): Storage {
  const values = new Map(Object.entries(seed));
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    key: (index: number) => [...values.keys()][index] ?? null,
    getItem: (key: string) => {
      if (throws) {
        throw new Error('storage disabled');
      }
      return values.get(key) ?? null;
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
    setItem: (key: string, value: string) => {
      if (throws) {
        throw new Error('quota exceeded');
      }
      values.set(key, value);
    },
  };
}

describe('appearance preference', () => {
  it('round-trips a theme and accent', () => {
    const storage = fakeStorage();
    saveAppearance(storage, { theme: 'Graphite', accent: '#6fd6c8' });
    expect(loadAppearance(storage)).toEqual({ theme: 'Graphite', accent: '#6fd6c8' });
  });

  it('normalises an accent on the way back in', () => {
    const storage = fakeStorage({ 'nysia.appearance': '{"theme":"Ember","accent":"#ABC"}' });
    expect(loadAppearance(storage).accent).toBe('#aabbcc');
  });

  it('falls back to the default for anything it cannot trust', () => {
    for (const raw of [
      'not json',
      'null',
      '[]',
      '{"theme":"Midnight","accent":"rebeccapurple"}',
      '{"theme":42}',
    ]) {
      expect(loadAppearance(fakeStorage({ 'nysia.appearance': raw })), raw).toEqual(
        DEFAULT_APPEARANCE,
      );
    }
  });

  it('loses the preference rather than the window when storage is unavailable', () => {
    expect(loadAppearance(undefined)).toEqual(DEFAULT_APPEARANCE);
    expect(loadAppearance(fakeStorage({}, true))).toEqual(DEFAULT_APPEARANCE);
    expect(() =>
      saveAppearance(fakeStorage({}, true), { theme: 'Ember', accent: '#f2b35b' }),
    ).not.toThrow();
    expect(() => saveAppearance(undefined, DEFAULT_APPEARANCE)).not.toThrow();
  });
});
