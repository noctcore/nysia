import { useContext } from 'react';

import { ThemeContext, type ThemeContextValue } from './ThemeContext';

/** Read the live theme. Throws outside `ThemeProvider`, which is always a wiring bug. */
export function useTheme(): ThemeContextValue {
  const value = useContext(ThemeContext);
  if (value === null) {
    throw new Error('useTheme must be used inside <ThemeProvider>');
  }
  return value;
}
