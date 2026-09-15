/**
 * The settings information architecture from design-spec.md §5.
 *
 * The whole tree is rendered, not just the two panes v0.1 implements. That is deliberate:
 * the nav is the clearest statement of what Nysia intends to be — Orchestration, Voice,
 * Automations, Task sources, per-project overrides — and hiding the unbuilt entries would
 * make each one look like a surprise when it lands. Every unimplemented entry opens a pane
 * that says which release it is due in, so the tree promises rather than pretends.
 *
 * The `Projects` group is not here: it is built from the store's project list at render
 * time, because per-project settings follow the projects the user actually has.
 */

export type SettingsEntryId =
  | 'agents'
  | 'accounts'
  | 'orchestration'
  | 'voice'
  | 'nysia-account'
  | 'general'
  | 'appearance'
  | 'integrations'
  | 'automations'
  | 'git'
  | 'task-sources'
  | 'terminal'
  | 'quick-commands';

export interface SettingsEntry {
  readonly id: SettingsEntryId;
  readonly label: string;
  /** The small uppercase pill the mock puts beside `AI provider accounts`. */
  readonly tag?: string;
  /** Which release delivers it. Absent once the pane is built. */
  readonly version?: string;
  readonly detail?: string;
}

export interface SettingsGroup {
  readonly label: string;
  readonly entries: readonly SettingsEntry[];
}

export const SETTINGS_TREE: readonly SettingsGroup[] = [
  {
    label: 'AI capabilities',
    entries: [
      // Built in v0.2, hence no `version`: carrying one on a pane that exists would put a
      // "coming in v0.2" placeholder in front of the pane it promises.
      { id: 'agents', label: 'Agents' },
      {
        id: 'accounts',
        label: 'AI provider accounts',
        tag: 'optional',
        version: 'v0.4',
        detail: 'Arrives with the multi-provider usage surface.',
      },
      {
        id: 'orchestration',
        label: 'Orchestration',
        version: 'v0.5',
        detail: 'The verb surface that lets one agent dispatch another.',
      },
      { id: 'voice', label: 'Voice', version: 'a later version' },
    ],
  },
  {
    label: 'Set up',
    entries: [
      { id: 'nysia-account', label: 'Nysia account', version: 'a later version' },
      { id: 'general', label: 'General' },
      { id: 'appearance', label: 'Appearance' },
      { id: 'integrations', label: 'Integrations', version: 'a later version' },
    ],
  },
  {
    label: 'Workflows',
    entries: [
      { id: 'automations', label: 'Automations', version: 'a later version' },
      {
        id: 'git',
        label: 'Git & source control',
        version: 'v0.4',
        detail: 'Arrives with the worktree manager.',
      },
      {
        id: 'task-sources',
        label: 'Task sources',
        version: 'v0.3',
        detail: 'Tasks are GitHub Issues, queried live — there is no local task model (D-5).',
      },
      {
        id: 'terminal',
        label: 'Terminal',
        version: 'v0.2',
        detail: 'Shell profiles, fonts and scrollback limits, once the PTY layer is wired.',
      },
      { id: 'quick-commands', label: 'Quick commands', version: 'a later version' },
    ],
  },
];

/** The pane the settings screen opens on. */
export const DEFAULT_SETTINGS_ENTRY: SettingsEntryId = 'general';
