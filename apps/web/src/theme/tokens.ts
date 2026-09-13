import { accentRamp } from './accent';
import { THEMES, type ThemeName } from './themes';

/**
 * Turning a (theme, accent) pair into the CSS custom properties the window runs on.
 *
 * Tailwind 4 emits the `@theme` block in `index.css` as `:root` custom properties, so
 * writing the same names as an inline style on `<html>` overrides them wholesale — that is
 * the whole theme switch. Keeping the mapping a pure function is what lets a node-only
 * vitest run (D-18) prove that switching theme replaces every token, with no DOM.
 */

/**
 * Every variable the switcher owns, in the order it writes them.
 *
 * The status palette is deliberately absent. It is defined once in `index.css` and never
 * re-emitted, which is the mechanical reason the accent picker cannot recolour it: there
 * is no code path from an accent to a status token.
 */
export const THEMED_VARIABLES = [
  '--color-bg0',
  '--color-bg1',
  '--color-bg2',
  '--color-bg3',
  '--color-line',
  '--color-line2',
  '--color-fg',
  '--color-fg2',
  '--color-fg3',
  '--color-acc',
  '--color-acc14',
  '--color-acc35',
] as const;

export type ThemedVariable = (typeof THEMED_VARIABLES)[number];

export type ThemeVariables = Readonly<Record<ThemedVariable, string>>;

/** The complete variable set for a theme and accent. */
export function cssVariablesFor(theme: ThemeName, accent: string): ThemeVariables {
  const surface = THEMES[theme];
  const { acc, acc14, acc35 } = accentRamp(accent);
  return {
    '--color-bg0': surface.bg0,
    '--color-bg1': surface.bg1,
    '--color-bg2': surface.bg2,
    '--color-bg3': surface.bg3,
    '--color-line': surface.line,
    '--color-line2': surface.line2,
    '--color-fg': surface.fg,
    '--color-fg2': surface.fg2,
    '--color-fg3': surface.fg3,
    '--color-acc': acc,
    '--color-acc14': acc14,
    '--color-acc35': acc35,
  };
}

/**
 * The narrow slice of `CSSStyleDeclaration` the switcher needs.
 *
 * Declaring it rather than taking an `HTMLElement` keeps `applyThemeVariables` — the code
 * path the running app actually uses — reachable from a node-only test.
 */
export interface StyleTarget {
  setProperty(property: string, value: string): void;
}

/**
 * Write a theme onto a style target. Every variable is written every time, so no token can
 * survive from the previous theme.
 */
export function applyThemeVariables(
  target: StyleTarget,
  theme: ThemeName,
  accent: string,
): void {
  const variables = cssVariablesFor(theme, accent);
  for (const name of THEMED_VARIABLES) {
    target.setProperty(name, variables[name]);
  }
}
