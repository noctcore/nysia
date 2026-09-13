import { describe, expect, it } from 'vitest';

import {
  ACC14_ALPHA_BYTE,
  ACC35_ALPHA_BYTE,
  accentRamp,
  isValidAccent,
  normalizeAccent,
} from './accent';
import { ACCENT_PRESETS, DEFAULT_ACCENT } from './themes';

describe('normalizeAccent', () => {
  it('expands three-digit hex and lower-cases the result', () => {
    expect(normalizeAccent('#ABC')).toBe('#aabbcc');
    expect(normalizeAccent('#F2B35B')).toBe('#f2b35b');
  });

  it('tolerates surrounding whitespace from a hand-edited settings file', () => {
    expect(normalizeAccent('  #6fd6c8 ')).toBe('#6fd6c8');
  });

  it('rejects anything that is not a three- or six-digit hex colour', () => {
    for (const bad of ['', 'f2b35b', '#f2b35', '#f2b35bff', '#gggggg', 'rebeccapurple']) {
      expect(normalizeAccent(bad), bad).toBeNull();
      expect(isValidAccent(bad), bad).toBe(false);
    }
  });
});

describe('accentRamp', () => {
  it('derives --acc14 at alpha 24 and --acc35 at alpha 59', () => {
    expect(accentRamp('#f2b35b')).toEqual({
      acc: '#f2b35b',
      acc14: '#f2b35b24',
      acc35: '#f2b35b59',
    });
  });

  it('names the alphas after their rounded percentage', () => {
    // 0x24 = 36/255 ≈ 14%, 0x59 = 89/255 ≈ 35%. If either byte is ever retuned, the token
    // name has to move with it.
    expect(Math.round((Number.parseInt(ACC14_ALPHA_BYTE, 16) / 255) * 100)).toBe(14);
    expect(Math.round((Number.parseInt(ACC35_ALPHA_BYTE, 16) / 255) * 100)).toBe(35);
  });

  it('derives both alphas for every preset the Appearance pane offers', () => {
    for (const preset of ACCENT_PRESETS) {
      const ramp = accentRamp(preset.value);
      expect(ramp.acc).toBe(preset.value);
      expect(ramp.acc14).toBe(`${preset.value}${ACC14_ALPHA_BYTE}`);
      expect(ramp.acc35).toBe(`${preset.value}${ACC35_ALPHA_BYTE}`);
    }
  });

  it('normalises before deriving, so a short accent still yields eight-digit tokens', () => {
    const ramp = accentRamp('#ABC');
    expect(ramp).toEqual({ acc: '#aabbcc', acc14: '#aabbcc24', acc35: '#aabbcc59' });
    expect(ramp.acc14).toHaveLength(9);
  });

  it('falls back to the default accent rather than emitting a broken token', () => {
    expect(accentRamp('not a colour').acc).toBe(DEFAULT_ACCENT);
  });
});
