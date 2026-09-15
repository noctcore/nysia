/**
 * The settings information architecture from design-spec.md §5.
 *
 * The whole tree is rendered, not just the entries a pane exists for. That is deliberate:
 * the nav is the clearest statement of what Nysia intends to be — Orchestration,
 * Automations, Task sources, per-project overrides — and hiding the unbuilt entries would
 * make each one look like a surprise when it lands. Every unimplemented entry opens a pane
 * that says which release it is due in, so the tree promises rather than pretends.
 *
 * Which is exactly why `Voice` is not here. The mock draws it and design-spec.md §5 still
 * lists it, but D-10 settled it — "No Voice. Design mock only." A tree that promises is a
 * tree that cannot carry an entry nothing will ever deliver: the reasoning above turns an
 * unbuilt entry into a commitment, and a commitment to something cancelled is the one thing
 * worse than hiding it.
 *
 * The `Projects` group is not here: it is built from the store's project list at render
 * time, because per-project settings follow the projects the user actually has.
 *
 * ## The glyph column
 *
 * `glyph` is required and nullable rather than optional, which is the difference between a
 * decision and an oversight: a new entry cannot be added without someone writing down what
 * it looks like, and `null` is them saying there is nothing honest to put there. The three
 * groups read as three undifferentiated lists without it (#72), and the icon rail two
 * inches to the left has had marks since v0.1.
 *
 * `Nysia account` is the one `null`. The mark it wants is a person, and the only
 * text-presentation characters that read as one are smileys — tonally wrong for this app,
 * and worse than that, a lookalike. A nav is navigated by its icons before it is read, so a
 * reader trusts them; a mark that means nothing is followed anyway, which makes a wrong
 * icon worse than none. The slot keeps its width so the column stays aligned and the gap
 * reads as deliberate. Do not fill it with the first character that fits.
 *
 * The `Projects` rows are blank for the same reason and a different one: a project is
 * identified by its name, and one mark repeated down every row would be exactly the
 * failure #73 describes on the other side of the window.
 */

import { GLYPH } from '../ui/glyphs';

export type SettingsEntryId =
  | 'agents'
  | 'accounts'
  | 'orchestration'
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
  /** The mark in the nav's glyph column, or `null` where no honest one exists. */
  readonly glyph: string | null;
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
      { id: 'agents', label: 'Agents', glyph: GLYPH.agent },
      {
        id: 'accounts',
        label: 'AI provider accounts',
        glyph: GLYPH.credentials,
        tag: 'optional',
        version: 'v0.4',
        detail: 'Arrives with the multi-provider usage surface.',
      },
      {
        id: 'orchestration',
        label: 'Orchestration',
        glyph: GLYPH.orchestration,
        version: 'v0.5',
        detail: 'The verb surface that lets one agent dispatch another.',
      },
    ],
  },
  {
    label: 'Set up',
    entries: [
      // The honest gap. See the glyph-column note above before filling it in.
      { id: 'nysia-account', label: 'Nysia account', glyph: null, version: 'a later version' },
      { id: 'general', label: 'General', glyph: GLYPH.settings },
      { id: 'appearance', label: 'Appearance', glyph: GLYPH.contrast },
      {
        id: 'integrations',
        label: 'Integrations',
        glyph: GLYPH.exchange,
        version: 'a later version',
      },
    ],
  },
  {
    label: 'Workflows',
    entries: [
      {
        id: 'automations',
        label: 'Automations',
        glyph: GLYPH.run,
        version: 'a later version',
      },
      {
        id: 'git',
        label: 'Git & source control',
        glyph: GLYPH.branch,
        version: 'v0.4',
        detail: 'Arrives with the worktree manager.',
      },
      {
        id: 'task-sources',
        label: 'Task sources',
        glyph: GLYPH.tasks,
        version: 'v0.3',
        detail: 'Tasks are GitHub Issues, queried live — there is no local task model (D-5).',
      },
      {
        id: 'terminal',
        label: 'Terminal',
        glyph: GLYPH.shell,
        version: 'v0.2',
        detail: 'Shell profiles, fonts and scrollback limits, once the PTY layer is wired.',
      },
      {
        id: 'quick-commands',
        label: 'Quick commands',
        glyph: GLYPH.command,
        version: 'a later version',
      },
    ],
  },
];

/** The pane the settings screen opens on. */
export const DEFAULT_SETTINGS_ENTRY: SettingsEntryId = 'general';
