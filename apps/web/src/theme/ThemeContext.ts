import { createContext } from 'react';

import type { AccentPreset, ThemeName } from './themes';

/**
 * The live appearance, and the two setters the Appearance pane drives.
 *
 * Kept in its own module so `ThemeProvider.tsx` exports nothing but a component:
 * `react-refresh/only-export-components` is a warning, and `pnpm lint` runs with
 * `--max-warnings 0`.
 */
export interface ThemeContextValue {
  readonly theme: ThemeName;
  readonly accent: string;
  readonly presets: readonly AccentPreset[];
  setTheme(theme: ThemeName): void;
  /** Ignores an unparseable value rather than blanking the accent mid-keystroke. */
  setAccent(accent: string): void;
}

export const ThemeContext = createContext<ThemeContextValue | null>(null);
