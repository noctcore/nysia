import { useCallback, useLayoutEffect, useMemo, useState, type ReactNode } from 'react';

import { isValidAccent, normalizeAccent } from './accent';
import { ThemeContext, type ThemeContextValue } from './ThemeContext';
import { loadAppearance, saveAppearance, type AppearancePreference } from './preference';
import { ACCENT_PRESETS, type ThemeName } from './themes';
import { applyThemeVariables } from './tokens';

/**
 * Applies the live theme to `<html>` as inline custom properties.
 *
 * Inline properties outrank the `:root` block Tailwind emits from `index.css`, so writing
 * all twelve is the whole switch — every utility in the window repaints in the same frame,
 * including surfaces inside portals and popovers, because they all read the same
 * variables. `useLayoutEffect` rather than `useEffect`: a paint with the old palette before
 * the new one lands is a visible flash on a window this dark.
 */
export function ThemeProvider({ children }: { readonly children: ReactNode }) {
  const [appearance, setAppearance] = useState<AppearancePreference>(() =>
    loadAppearance(typeof localStorage === 'undefined' ? undefined : localStorage),
  );

  useLayoutEffect(() => {
    applyThemeVariables(
      document.documentElement.style,
      appearance.theme,
      appearance.accent,
    );
    saveAppearance(typeof localStorage === 'undefined' ? undefined : localStorage, appearance);
  }, [appearance]);

  const setTheme = useCallback((theme: ThemeName) => {
    setAppearance((current) => (current.theme === theme ? current : { ...current, theme }));
  }, []);

  const setAccent = useCallback((accent: string) => {
    if (!isValidAccent(accent)) {
      return;
    }
    const normalized = normalizeAccent(accent) ?? accent;
    setAppearance((current) =>
      current.accent === normalized ? current : { ...current, accent: normalized },
    );
  }, []);

  const value = useMemo<ThemeContextValue>(
    () => ({
      theme: appearance.theme,
      accent: appearance.accent,
      presets: ACCENT_PRESETS,
      setTheme,
      setAccent,
    }),
    [appearance.theme, appearance.accent, setTheme, setAccent],
  );

  return <ThemeContext value={value}>{children}</ThemeContext>;
}
