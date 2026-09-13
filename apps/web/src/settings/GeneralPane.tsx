import { useEffect, useState } from 'react';

import { Segmented } from '../ui/Segmented';
import { SettingRow } from '../ui/SettingRow';
import { Toggle } from '../ui/Toggle';
import {
  MODELS,
  PERMISSION_MODES,
  THINKING_EFFORTS,
  loadGeneral,
  saveGeneral,
  type GeneralPreferences,
} from './generalPreferences';
import { SettingsCard, SettingsHeader } from './layout';

/**
 * Settings › General.
 *
 * The three groups and their copy come from the design mock's `settingsGroups` seed —
 * Defaults, Behaviour, Notifications. They are client preferences: the daemon validates
 * nothing here, and a project override beats any of them.
 *
 * Persistence is the same placeholder the theme uses, and for the same reason: D-12 puts
 * settings in JSON the daemon owns, and standing up a second authority now would only
 * have to be reconciled later.
 */
export function GeneralPane() {
  const [preferences, setPreferences] = useState<GeneralPreferences>(() =>
    loadGeneral(storage()),
  );

  useEffect(() => {
    saveGeneral(storage(), preferences);
  }, [preferences]);

  function update<K extends keyof GeneralPreferences>(
    key: K,
    value: GeneralPreferences[K],
  ) {
    setPreferences((current) => ({ ...current, [key]: value }));
  }

  return (
    <>
      <SettingsHeader
        title="General"
        description="Defaults for new sessions, and how Nysia behaves while they run."
      />

      <SettingsCard title="Defaults">
        <SettingRow
          divider={false}
          label="Default model"
          description="Used when a project has no override."
          control={
            <Segmented
              label="Default model"
              options={MODELS}
              value={preferences.model}
              onChange={(value) => update('model', value)}
            />
          }
        />
        <SettingRow
          label="Thinking effort"
          description="Higher is slower and costs more."
          control={
            <Segmented
              label="Thinking effort"
              options={THINKING_EFFORTS}
              value={preferences.thinkingEffort}
              onChange={(value) => update('thinkingEffort', value)}
            />
          }
        />
        <SettingRow
          label="Permission mode"
          description="Shift+Tab cycles per session."
          control={
            <Segmented
              label="Permission mode"
              options={PERMISSION_MODES}
              value={preferences.permissionMode}
              onChange={(value) => update('permissionMode', value)}
            />
          }
        />
      </SettingsCard>

      <SettingsCard title="Behaviour">
        <SettingRow
          divider={false}
          label="Session recaps"
          description="Summarise goal, outcome and next action when an agent stops."
          control={
            <Toggle
              label="Session recaps"
              checked={preferences.sessionRecaps}
              onChange={(value) => update('sessionRecaps', value)}
            />
          }
        />
        <SettingRow
          label="Auto-clear suggestion"
          description="Suggest /clear when context passes 250k tokens."
          control={
            <Toggle
              label="Auto-clear suggestion"
              checked={preferences.autoClearSuggestion}
              onChange={(value) => update('autoClearSuggestion', value)}
            />
          }
        />
        <SettingRow
          label="Confirm destructive git"
          description="Ask before force-push, reset --hard, branch delete."
          control={
            <Toggle
              label="Confirm destructive git"
              checked={preferences.confirmDestructiveGit}
              onChange={(value) => update('confirmDestructiveGit', value)}
            />
          }
        />
      </SettingsCard>

      <SettingsCard title="Notifications">
        <SettingRow
          divider={false}
          label="Needs-input alerts"
          description="System notification when an agent is blocked on you."
          control={
            <Toggle
              label="Needs-input alerts"
              checked={preferences.needsInputAlerts}
              onChange={(value) => update('needsInputAlerts', value)}
            />
          }
        />
        <SettingRow
          label="Completion sound"
          description="Play a chime when a task finishes."
          control={
            <Toggle
              label="Completion sound"
              checked={preferences.completionSound}
              onChange={(value) => update('completionSound', value)}
            />
          }
        />
      </SettingsCard>
    </>
  );
}

function storage(): Storage | undefined {
  return typeof localStorage === 'undefined' ? undefined : localStorage;
}
