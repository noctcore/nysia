import type { SessionKind } from '../generated/SessionKind';

/**
 * The Agents pane's preferences (design-spec.md §5, Screen 2c).
 *
 * Client preferences, every one of them: nothing here is validated by the daemon, an SSH or
 * remote session is still checked when it launches, and a project override beats all of it.
 * Persistence is the placeholder `generalPreferences.ts` uses, for the reason it gives —
 * D-12 puts settings in JSON the daemon owns, and a second settings authority standing up
 * now would only have to be reconciled later. Everything read back is validated rather than
 * trusted, because the value is JSON a user can edit.
 */

/**
 * The agents Nysia can run.
 *
 * Exactly one, and the type says so rather than a comment: D-3/D-4 make Claude the only
 * agent in v1, and there is no provider trait. The design artboard draws chips for Claude
 * Agent Teams, Codex, Grok, OpenCode, Gemini and Cursor; none of them exist, so none of
 * them is here — **absent, not disabled and not greyed**, per the v0.2 delivery plan §1.
 * Six agents rendered when one works is the same defect as a document describing code it
 * does not have.
 *
 * The second one lands in **v0.6**, which is also when the provider trait gets extracted
 * from two real implementations rather than guessed at from one.
 */
export const AGENT_IDS = ['claude'] as const;
export type AgentId = (typeof AGENT_IDS)[number];

/**
 * What a new tab opens as by default: an agent, or a blank terminal.
 *
 * `none` is in the design's chip row as *No agent (blank terminal)* and survives the cut
 * because it is the one non-Claude choice that corresponds to something the product has —
 * a shell session is half of `SessionKind` and the `+` menu already launches four of them.
 * `Auto` does not survive: choosing automatically among a set of one is a gesture at a
 * set, and the whole point of §1 is not to gesture at things that are not there.
 */
export const DEFAULT_AGENT_CHOICES = [...AGENT_IDS, 'none'] as const;
export type DefaultAgentChoice = (typeof DEFAULT_AGENT_CHOICES)[number];

export interface AgentChoice {
  readonly id: DefaultAgentChoice;
  readonly label: string;
  /**
   * Which mark the chip carries.
   *
   * `SessionKind` rather than a glyph string, so the chip draws itself with `SessionGlyph`
   * — the accent asterisk with its glow for an agent, `>_` in Fira Code for a shell — and a
   * chip in Settings cannot end up wearing a different mark from the tab it opens.
   */
  readonly kind: SessionKind;
}

export const AGENT_CHOICES: readonly AgentChoice[] = [
  { id: 'claude', label: 'Claude', kind: 'agent' },
  { id: 'none', label: 'No agent (blank terminal)', kind: 'shell' },
];

export const KEEP_AWAKE_MODES = ['On', 'Agent', 'Off'] as const;
export const AGENT_PERMISSIONS = ['Yolo', 'Manual'] as const;

export type KeepAwakeMode = (typeof KEEP_AWAKE_MODES)[number];
export type AgentPermission = (typeof AGENT_PERMISSIONS)[number];

export interface AgentPreferences {
  readonly defaultAgent: DefaultAgentChoice;
  /**
   * Whether Nysia manages the agent's status hooks.
   *
   * The one preference here that is meant to reach outside the webview: it maps 1:1 to the
   * install/uninstall in `crates/nysia-core/src/agent/**` and to Orca's
   * `agent hooks on|off|status`. The command surface that carries it there is **v0.2 wave
   * C**; until then this records the choice and removes nothing, which the row's own
   * description says out loud rather than leaving to a release note.
   */
  readonly statusHooks: boolean;
  readonly autoTabTitles: boolean;
  readonly keepAwake: KeepAwakeMode;
  readonly promptCacheTimer: boolean;
  /**
   * Fewer permission prompts, or manual checks.
   *
   * Names the same idea as `GeneralPreferences.permissionMode` (`Bypass | Manual`) and is
   * deliberately not merged with it: the design spec puts a control in both panes, and one
   * of them has to become the other's alias when D-12 moves settings into the daemon's
   * JSON. That reconciliation is a settings-authority decision, not a pane's to make
   * unilaterally, so both are stored and this comment is the marker for whoever makes it.
   */
  readonly agentPermissions: AgentPermission;
}

/**
 * The defaults the artboard shows.
 *
 * Two are worth a word. `keepAwake` is `Agent` because that is the mode the row's own
 * description explains — awake only while agents are working — and defaulting to `On`
 * would hold a laptop awake for a pane nobody is watching. `agentPermissions` is `Yolo`
 * to match §3's session chip row, which draws `bypass permissions on` as the running state.
 */
export const DEFAULT_AGENTS: AgentPreferences = {
  defaultAgent: 'claude',
  statusHooks: true,
  autoTabTitles: true,
  keepAwake: 'Agent',
  promptCacheTimer: false,
  agentPermissions: 'Yolo',
};

const STORAGE_KEY = 'nysia.agents';

/** Reads one preference set, substituting the default for any field it cannot trust. */
export function parseAgents(raw: string | null): AgentPreferences {
  if (raw === null) {
    return DEFAULT_AGENTS;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return DEFAULT_AGENTS;
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return DEFAULT_AGENTS;
  }
  const record: Record<string, unknown> = { ...parsed };
  return {
    defaultAgent: oneOf(
      DEFAULT_AGENT_CHOICES,
      record.defaultAgent,
      DEFAULT_AGENTS.defaultAgent,
    ),
    statusHooks: boolOr(record.statusHooks, DEFAULT_AGENTS.statusHooks),
    autoTabTitles: boolOr(record.autoTabTitles, DEFAULT_AGENTS.autoTabTitles),
    keepAwake: oneOf(KEEP_AWAKE_MODES, record.keepAwake, DEFAULT_AGENTS.keepAwake),
    promptCacheTimer: boolOr(record.promptCacheTimer, DEFAULT_AGENTS.promptCacheTimer),
    agentPermissions: oneOf(
      AGENT_PERMISSIONS,
      record.agentPermissions,
      DEFAULT_AGENTS.agentPermissions,
    ),
  };
}

export function loadAgents(storage: Storage | undefined): AgentPreferences {
  if (!storage) {
    return DEFAULT_AGENTS;
  }
  try {
    return parseAgents(storage.getItem(STORAGE_KEY));
  } catch {
    return DEFAULT_AGENTS;
  }
}

export function saveAgents(
  storage: Storage | undefined,
  preferences: AgentPreferences,
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

function oneOf<T extends string>(allowed: readonly T[], value: unknown, fallback: T): T {
  return typeof value === 'string' && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback;
}

function boolOr(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback;
}
