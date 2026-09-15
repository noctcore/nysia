import { describe, expect, it } from 'vitest';

import { SEED_LAUNCHERS } from '../store/mock/seed';
import { GLYPH, launcherGlyph } from './glyphs';

/*
 * The vocabulary's one mechanical rule, and the `+` menu's lookup.
 *
 * A glyph inherits the token colour, which is the entire reason design-spec.md §6.8 is
 * satisfied by a character — except for one class of character that brings its own.
 * A default-emoji code point renders as a colour bitmap: `theme/colourGuard.ts` walks the
 * syntax tree for colour literals and there is no literal to find, so this is the one way
 * to put an unswitchable colour in the chrome and keep every existing gate green.
 *
 * `Emoji_Presentation` is a Unicode property JavaScript's regex engine already knows, so
 * the check needs nothing installed and no table of our own to go stale.
 */
const EMOJI_BY_DEFAULT = /\p{Emoji_Presentation}/u;

/**
 * Characters whose default presentation is a colour bitmap, kept as the proof that the
 * rule above trips (CLAUDE.md §5, trap 12).
 *
 * Both were real candidates while this was being written — a bolt for Automations, a
 * penguin for WSL — which is what makes them the right fixtures rather than invented ones.
 */
const REJECTED = { '⚡': 'U+26A1, a bolt for Automations', '🐧': 'U+1F427, a penguin for WSL' };

describe('the glyph vocabulary', () => {
  it('holds no character that renders as colour emoji', () => {
    for (const [name, glyph] of Object.entries(GLYPH)) {
      expect(
        EMOJI_BY_DEFAULT.test(glyph),
        `GLYPH.${name} is ${JSON.stringify(glyph)}, which paints its own colour and ` +
          'will not follow the theme switcher',
      ).toBe(false);
    }
  });

  it('is checked by a rule that actually rejects one', () => {
    // Without this the case above passes just as happily against a regex that matches
    // nothing, and the gate would be decoration.
    for (const [glyph, what] of Object.entries(REJECTED)) {
      expect(EMOJI_BY_DEFAULT.test(glyph), what).toBe(true);
    }
  });

  it('is big enough to be the whole vocabulary, not a remnant', () => {
    expect(Object.keys(GLYPH).length).toBeGreaterThan(20);
  });
});

describe('the mark on a `+` menu entry', () => {
  /** The four ids `DaemonStore.profileFor` switches on, which is what this mirrors. */
  const SHELLS = ['shell.pwsh', 'shell.cmd', 'shell.git_bash', 'shell.wsl'] as const;

  it('gives each of the four shells a different one', () => {
    // #73: all four wore `>_` and were told apart only by the hint at the far end of the
    // row, so the glyph column did no work at all.
    const marks = SHELLS.map((id) => launcherGlyph(id, 'shell'));
    expect(new Set(marks).size, `${marks.join(' ')} — two shells share a mark`).toBe(
      SHELLS.length,
    );
  });

  it('keeps each of them distinct from the generic shell mark too', () => {
    for (const id of SHELLS) {
      expect(launcherGlyph(id, 'shell'), id).not.toBe(GLYPH.shell);
    }
  });

  it('falls back to `>_` for a shell the daemon grows later', () => {
    // A shape it has no mark for should still read as a shell rather than as nothing.
    expect(launcherGlyph('shell.nushell', 'shell')).toBe(GLYPH.shell);
    expect(launcherGlyph('', 'shell')).toBe(GLYPH.shell);
  });

  it('marks every agent with the agent star, whatever its id', () => {
    // Claude is the only agent and there is no provider trait (D-3, D-4): a second agent
    // mark would be a shape with nothing behind it.
    expect(launcherGlyph('agent.claude', 'agent')).toBe(GLYPH.agent);
    expect(launcherGlyph('shell.pwsh', 'agent')).toBe(GLYPH.agent);
  });

  it('recognises every shell the seeded menu actually offers', () => {
    /*
     * The one case that reads real launcher data rather than ids written here.
     *
     * The lookup keys off the id, so a fixture spelling an id its own way does not fail —
     * it silently takes the `>_` fallback and the menu looks exactly as it did before #73
     * was fixed. That had already happened: the seed said `shell.gitbash` where the daemon
     * says `shell.git_bash`. Tolerating both spellings would have hidden it for good, so
     * the seed was corrected instead and this is what holds it there.
     */
    const shells = SEED_LAUNCHERS.flatMap((group) => group.items).filter(
      (item) => item.kind === 'shell',
    );
    expect(shells.length, 'the fixture needs shells to check').toBeGreaterThan(1);
    for (const shell of shells) {
      expect(
        launcherGlyph(shell.id, shell.kind),
        `${shell.id} (${shell.label}) fell back to the generic shell mark — the id is ` +
          'spelled differently here than in DaemonStore.profileFor',
      ).not.toBe(GLYPH.shell);
    }
  });
});
