import { normalizeAccent } from './accent';
import { DEFAULT_ACCENT, DEFAULT_THEME, isThemeName, type ThemeName } from './themes';

/**
 * Where the appearance preference lives until the daemon owns it.
 *
 * D-12 puts settings in JSON written by the daemon, so this is a placeholder: it keeps the
 * choice across a reload without inventing a second settings authority. Both functions
 * swallow storage failures — a webview with storage disabled should lose the preference,
 * not the window.
 */

export interface AppearancePreference {
  readonly theme: ThemeName;
  readonly accent: string;
}

const STORAGE_KEY = 'nysia.appearance';

export const DEFAULT_APPEARANCE: AppearancePreference = {
  theme: DEFAULT_THEME,
  accent: DEFAULT_ACCENT,
};

/**
 * Read the stored preference, falling back to the default for anything unrecognised.
 *
 * Every field is validated rather than trusted: the value is JSON a user can edit, and a
 * bad accent would otherwise reach `accentRamp` and silently paint nothing.
 */
export function loadAppearance(storage: Storage | undefined): AppearancePreference {
  const raw = readRaw(storage);
  if (raw === null) {
    return DEFAULT_APPEARANCE;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return DEFAULT_APPEARANCE;
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return DEFAULT_APPEARANCE;
  }
  const record: Record<string, unknown> = { ...parsed };
  const theme = record.theme;
  const accent = record.accent;
  return {
    theme: typeof theme === 'string' && isThemeName(theme) ? theme : DEFAULT_THEME,
    accent:
      typeof accent === 'string' ? (normalizeAccent(accent) ?? DEFAULT_ACCENT) : DEFAULT_ACCENT,
  };
}

export function saveAppearance(
  storage: Storage | undefined,
  preference: AppearancePreference,
): void {
  if (!storage) {
    return;
  }
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(preference));
  } catch {
    // Storage can be unavailable or full. An unremembered theme is not worth a crash.
  }
}

function readRaw(storage: Storage | undefined): string | null {
  if (!storage) {
    return null;
  }
  try {
    return storage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}
