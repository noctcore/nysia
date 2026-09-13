/**
 * The token tables, transcribed from the `themes` object in the design mock's
 * `<script type="text/x-dc">` block and cross-checked against design-spec.md §1.
 *
 * This module and `index.css` are the **only** two files in `apps/web` allowed to contain
 * a colour literal, and `hexGuard.test.ts` fails the build if a third appears. Everything
 * else reads a token, because the theme and the accent are live user tweaks: a hardcoded
 * colour is a pixel that stops following the switcher.
 */

/** The two themes design-spec.md §1 ships. Ember is cool blue-black, Graphite warm grey. */
export type ThemeName = 'Ember' | 'Graphite';

/**
 * The nine surface and text tokens every theme defines.
 *
 * `bg0` is chrome — titlebar, rail, status bar, cards. `bg1` is the app body. `bg2` and
 * `bg3` are raised surfaces. `line` is a plain border, `line2` a raised or interactive one.
 */
export interface SurfaceTokens {
  readonly bg0: string;
  readonly bg1: string;
  readonly bg2: string;
  readonly bg3: string;
  readonly line: string;
  readonly line2: string;
  readonly fg: string;
  readonly fg2: string;
  readonly fg3: string;
}

export const THEMES: Readonly<Record<ThemeName, SurfaceTokens>> = {
  Ember: {
    bg0: '#090b10',
    bg1: '#0c0f15',
    bg2: '#11151d',
    bg3: '#161b25',
    line: '#1f2532',
    line2: '#2a3140',
    fg: '#dfe3ea',
    fg2: '#8b94a6',
    fg3: '#5c6577',
  },
  Graphite: {
    bg0: '#0f0f11',
    bg1: '#141416',
    bg2: '#18181b',
    bg3: '#1c1c20',
    line: '#26262a',
    line2: '#2e2e33',
    fg: '#e6e4e0',
    fg2: '#a8a8b0',
    fg3: '#7c7c84',
  },
};

/** Declaration order is the order the Appearance pane offers them in. */
export const THEME_NAMES: readonly ThemeName[] = ['Ember', 'Graphite'];

export const DEFAULT_THEME: ThemeName = 'Ember';

/** Narrowing for values that crossed a persistence or wire boundary. */
export function isThemeName(value: string): value is ThemeName {
  return Object.prototype.hasOwnProperty.call(THEMES, value);
}

/** One of the swatches the Appearance pane offers; the accent itself is free-form. */
export interface AccentPreset {
  readonly label: string;
  readonly value: string;
}

export const DEFAULT_ACCENT = '#f2b35b';

export const ACCENT_PRESETS: readonly AccentPreset[] = [
  { label: 'Amber', value: DEFAULT_ACCENT },
  { label: 'Teal', value: '#6fd6c8' },
  { label: 'Violet', value: '#b79cf2' },
  { label: 'Pink', value: '#ef8fa2' },
];

/**
 * Agent lifecycle colours.
 *
 * Deliberately **not** derived from the accent, and deliberately not in the same table:
 * accent is identity, status is semantics. A user who picks a teal accent must still be
 * able to tell a running agent from a failed one, so the accent picker never touches
 * these. Three of the four are `oklch` in the design source; they are transcribed as-is.
 */
export interface StatusTokens {
  readonly running: string;
  readonly needsInput: string;
  readonly queued: string;
  readonly failed: string;
}

export const STATUS_TOKENS: StatusTokens = {
  running: 'oklch(78% 0.12 180)',
  needsInput: 'oklch(80% 0.15 70)',
  queued: '#7c7c84',
  failed: 'oklch(70% 0.15 25)',
};
