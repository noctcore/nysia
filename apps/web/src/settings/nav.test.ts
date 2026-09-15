import { describe, expect, it } from 'vitest';

import { SETTINGS_TREE, type SettingsEntry } from './nav';

/*
 * What the settings tree promises, asserted rather than trusted to a comment.
 *
 * The tree renders entries no pane exists for yet, on the reasoning `nav.ts` states: an
 * unbuilt entry is a commitment to build it. That makes two things decisions rather than
 * details — which entries are in it, and what each one looks like — and both are the kind
 * that regress quietly, because nothing about re-adding a row or leaving a glyph off fails
 * to compile.
 */
const entries: readonly SettingsEntry[] = SETTINGS_TREE.flatMap((group) => group.entries);

/** The same rule `ui/glyphs.test.ts` holds the vocabulary to, applied where it is used. */
const EMOJI_BY_DEFAULT = /\p{Emoji_Presentation}/u;

describe('the settings tree', () => {
  it('is the three groups design-spec.md §5 names', () => {
    // Guards everything below from passing vacuously against a tree that lost its rows.
    expect(SETTINGS_TREE.map((group) => group.label)).toEqual([
      'AI capabilities',
      'Set up',
      'Workflows',
    ]);
    expect(entries.length).toBeGreaterThan(10);
  });

  it('does not carry Voice', () => {
    // D-10: "No Voice. Design mock only." The mock draws it and design-spec.md §5 still
    // lists it, so it has a way back in; a tree whose unbuilt entries are promises cannot
    // hold one for something cancelled (#72).
    expect(entries.map((entry) => entry.id)).not.toContain('voice');
    for (const entry of entries) {
      expect(entry.label, entry.id).not.toMatch(/voice/i);
    }
  });

  it('gives no two entries the same mark', () => {
    // The failure #73 describes, on the surface #72 is about: a glyph column where one
    // mark appears twice is a column that does not distinguish.
    const glyphs = entries.map((entry) => entry.glyph).filter((glyph) => glyph !== null);
    expect(new Set(glyphs).size, `${glyphs.join(' ')} — a mark is used twice`).toBe(
      glyphs.length,
    );
  });

  it('leaves exactly one slot empty, and says which', () => {
    // An honest gap beats a lookalike, but only while it stays one considered gap. This
    // fails both ways on purpose: filling `Nysia account` with the first character that
    // fits trips it, and so does a new entry that quietly skips the decision.
    const blank = entries.filter((entry) => entry.glyph === null).map((entry) => entry.id);
    expect(blank).toEqual(['nysia-account']);
  });

  it('marks every entry with a character that follows the theme', () => {
    for (const entry of entries) {
      if (entry.glyph === null) {
        continue;
      }
      expect(
        EMOJI_BY_DEFAULT.test(entry.glyph),
        `${entry.id} is marked ${JSON.stringify(entry.glyph)}, which paints its own ` +
          'colour and will not follow the theme switcher',
      ).toBe(false);
    }
  });

  it('names a release for every entry that has no pane', () => {
    // Pre-existing behaviour, asserted here because the glyph column is a second thing a
    // row now carries and a copy/paste of a built row would bring its empty `version`.
    const built = ['agents', 'general', 'appearance'];
    for (const entry of entries) {
      if (built.includes(entry.id)) {
        expect(entry.version, `${entry.id} has a pane but promises a release`).toBeUndefined();
      } else {
        expect(entry.version, `${entry.id} has no pane and names no release`).toBeTruthy();
      }
    }
  });
});
