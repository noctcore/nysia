import { DEFAULT_ACCENT } from './themes';

/**
 * The accent ramp.
 *
 * design-spec.md §1 derives two translucent variants from the single accent hue:
 * `--acc14` at alpha `24` (active rail items, chip fills, the focus ring) and `--acc35` at
 * alpha `59` (pill borders). The names are the rounded percentages — `0x24` is 36/255 ≈
 * 14%, `0x59` is 89/255 ≈ 35% — and the design mock produces them by string-appending the
 * alpha byte to the accent, which is what this reproduces.
 *
 * Appending an alpha byte rather than mixing two colours matters: the result composites against
 * whatever is behind it, so the same token reads correctly over `bg0` chrome and over a
 * raised `bg2` popover without a second variant per surface.
 */

/** Alpha byte for `--acc14`: 36/255 ≈ 14%. */
export const ACC14_ALPHA_BYTE = '24';

/** Alpha byte for `--acc35`: 89/255 ≈ 35%. */
export const ACC35_ALPHA_BYTE = '59';

const SHORT_HEX = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i;
const LONG_HEX = /^#[0-9a-f]{6}$/i;

/**
 * Expand a three-digit hex colour to its six-digit form and lower-case the result, or
 * return `null` if the input is not a three- or six-digit hex colour.
 *
 * An accent can arrive from a settings file written by an older build or edited by hand,
 * so this is a boundary check, not a formality — an unparsed value appended with an alpha
 * byte produces a token that silently paints nothing.
 */
export function normalizeAccent(value: string): string | null {
  const trimmed = value.trim();
  const short = SHORT_HEX.exec(trimmed);
  if (short) {
    const [, r, g, b] = short;
    return `#${r}${r}${g}${g}${b}${b}`.toLowerCase();
  }
  return LONG_HEX.test(trimmed) ? trimmed.toLowerCase() : null;
}

/** The same check, for callers that only need the answer (the Appearance colour input). */
export function isValidAccent(value: string): boolean {
  return normalizeAccent(value) !== null;
}

/** The three accent tokens, ready to be written as CSS custom properties. */
export interface AccentRamp {
  readonly acc: string;
  readonly acc14: string;
  readonly acc35: string;
}

/**
 * Derive the ramp, falling back to the default amber when `accent` is unparseable.
 *
 * Falling back rather than throwing is deliberate: a bad accent in a settings file should
 * cost the user their accent, not their window.
 */
export function accentRamp(accent: string): AccentRamp {
  const acc = normalizeAccent(accent) ?? DEFAULT_ACCENT;
  return {
    acc,
    acc14: `${acc}${ACC14_ALPHA_BYTE}`,
    acc35: `${acc}${ACC35_ALPHA_BYTE}`,
  };
}
