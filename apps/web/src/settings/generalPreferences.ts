/**
 * The General pane's preferences, and the same placeholder persistence the theme uses.
 *
 * D-12 puts settings in JSON owned by the daemon, so this is temporary by design: it keeps
 * the choices across a reload without standing up a second settings authority that would
 * then have to be reconciled. Everything read back is validated rather than trusted — the
 * value is JSON a user can edit, and a stray string reaching a segmented control would
 * render a group with nothing selected.
 */

export const MODELS = ['Opus 5', 'Sonnet 5', 'Haiku 4.5'] as const;
export const THINKING_EFFORTS = ['low', 'medium', 'high', 'xhigh'] as const;
export const PERMISSION_MODES = ['Bypass', 'Manual'] as const;

export type Model = (typeof MODELS)[number];
export type ThinkingEffort = (typeof THINKING_EFFORTS)[number];
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export interface GeneralPreferences {
  readonly model: Model;
  readonly thinkingEffort: ThinkingEffort;
  readonly permissionMode: PermissionMode;
  readonly sessionRecaps: boolean;
  readonly autoClearSuggestion: boolean;
  readonly confirmDestructiveGit: boolean;
  readonly needsInputAlerts: boolean;
  readonly completionSound: boolean;
}

export const DEFAULT_GENERAL: GeneralPreferences = {
  model: 'Opus 5',
  thinkingEffort: 'xhigh',
  permissionMode: 'Bypass',
  sessionRecaps: true,
  autoClearSuggestion: true,
  confirmDestructiveGit: true,
  needsInputAlerts: true,
  completionSound: false,
};

const STORAGE_KEY = 'nysia.general';

/** Reads one preference set, substituting the default for any field it cannot trust. */
export function parseGeneral(raw: string | null): GeneralPreferences {
  if (raw === null) {
    return DEFAULT_GENERAL;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return DEFAULT_GENERAL;
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return DEFAULT_GENERAL;
  }
  const record: Record<string, unknown> = { ...parsed };
  return {
    model: oneOf(MODELS, record.model, DEFAULT_GENERAL.model),
    thinkingEffort: oneOf(
      THINKING_EFFORTS,
      record.thinkingEffort,
      DEFAULT_GENERAL.thinkingEffort,
    ),
    permissionMode: oneOf(
      PERMISSION_MODES,
      record.permissionMode,
      DEFAULT_GENERAL.permissionMode,
    ),
    sessionRecaps: boolOr(record.sessionRecaps, DEFAULT_GENERAL.sessionRecaps),
    autoClearSuggestion: boolOr(
      record.autoClearSuggestion,
      DEFAULT_GENERAL.autoClearSuggestion,
    ),
    confirmDestructiveGit: boolOr(
      record.confirmDestructiveGit,
      DEFAULT_GENERAL.confirmDestructiveGit,
    ),
    needsInputAlerts: boolOr(record.needsInputAlerts, DEFAULT_GENERAL.needsInputAlerts),
    completionSound: boolOr(record.completionSound, DEFAULT_GENERAL.completionSound),
  };
}

export function loadGeneral(storage: Storage | undefined): GeneralPreferences {
  if (!storage) {
    return DEFAULT_GENERAL;
  }
  try {
    return parseGeneral(storage.getItem(STORAGE_KEY));
  } catch {
    return DEFAULT_GENERAL;
  }
}

export function saveGeneral(
  storage: Storage | undefined,
  preferences: GeneralPreferences,
): void {
  if (!storage) {
    return;
  }
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
  } catch {
    // Storage can be unavailable or full. A forgotten preference is not worth a crash.
  }
}

function oneOf<T extends string>(
  allowed: readonly T[],
  value: unknown,
  fallback: T,
): T {
  return typeof value === 'string' && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback;
}

function boolOr(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback;
}
