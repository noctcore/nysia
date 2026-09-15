import { useEffect, useRef, useState, type KeyboardEvent } from 'react';

import { SessionGlyph } from '../chrome/SessionGlyph';
import { GLYPH } from '../ui/glyphs';
import { isArrowKey, nextOption, tabbableIndex } from '../ui/roving';
import { Segmented } from '../ui/Segmented';
import { SettingRow } from '../ui/SettingRow';
import { Toggle } from '../ui/Toggle';
import {
  AGENT_CHOICES,
  AGENT_PERMISSIONS,
  DEFAULT_AGENT_CHOICES,
  KEEP_AWAKE_MODES,
  loadAgents,
  saveAgents,
  type AgentPreferences,
  type DefaultAgentChoice,
} from './agentPreferences';
import {
  detectInstalledAgents,
  detectedLabel,
  type InstalledAgent,
} from './installedAgents';
import { SettingsCard, SettingsHeader } from './layout';
import { settingsStorage } from './storage';

/**
 * Settings › Agents — design-spec.md §5, Screen 2c, the Claude slice.
 *
 * Two things this pane deliberately does not draw, both from the v0.2 delivery plan §1:
 *
 *  - **Seven of the nine chips.** Auto, Claude Agent Teams, Codex, Grok, OpenCode, Gemini
 *    and Cursor correspond to nothing this build can launch (D-3), so they are absent —
 *    not disabled, not greyed. `agentPreferences.ts` holds the roster and the reasoning;
 *    the second agent, and the provider trait extracted from two real implementations
 *    rather than guessed from one, are **v0.6**.
 *  - **All four Installed rows, and every version on them.** See `installedAgents.ts`.
 *
 * Everything here is a client preference. The note under the heading says so, because the
 * distinction matters the first time someone points Nysia at a remote host: the default
 * agent is what this window *asks* for, and the run-time check is what decides.
 */
export function AgentsPane({
  installed = detectInstalledAgents(),
}: {
  /**
   * Injected by the render test, which exercises the populated path with a fixture so the
   * rows are not first run on the day detection starts answering.
   */
  readonly installed?: readonly InstalledAgent[];
}) {
  const [preferences, setPreferences] = useState<AgentPreferences>(() =>
    loadAgents(settingsStorage()),
  );

  useEffect(() => {
    saveAgents(settingsStorage(), preferences);
  }, [preferences]);

  function update<K extends keyof AgentPreferences>(key: K, value: AgentPreferences[K]) {
    setPreferences((current) => ({ ...current, [key]: value }));
  }

  return (
    <>
      <SettingsHeader title="Agents" description="Manage AI agents and set a default." />

      <SettingsCard title="Default agent">
        <p className="text-fg2 text-term m-0 leading-normal">
          Client preferences. They say what a new tab asks for; an SSH or remote session is
          still validated when it launches, and a project override beats all of it.
        </p>

        <AgentChips
          value={preferences.defaultAgent}
          onChange={(value) => update('defaultAgent', value)}
        />

        <SettingRow
          label="Agent status hooks"
          description="Shows working, waiting and done states in Nysia. Turn off to remove managed hooks. Nysia records the choice today and writes nothing — installing and removing the hooks arrives in v0.2 wave C."
          control={
            <Toggle
              label="Agent status hooks"
              checked={preferences.statusHooks}
              onChange={(value) => update('statusHooks', value)}
            />
          }
        />
        <SettingRow
          label="Auto-generate tab titles"
          description="Derive short stable tab names from the first agent prompt. Manual renames always win."
          control={
            <Toggle
              label="Auto-generate tab titles"
              checked={preferences.autoTabTitles}
              onChange={(value) => update('autoTabTitles', value)}
            />
          }
        />
        <SettingRow
          label="Keep computer awake"
          description="Agent mode stays awake only while agents are working."
          control={
            <Segmented
              label="Keep computer awake"
              options={KEEP_AWAKE_MODES}
              value={preferences.keepAwake}
              onChange={(value) => update('keepAwake', value)}
            />
          }
        />
        <SettingRow
          label="Prompt cache timer"
          description="Countdown in the sidebar after a Claude agent becomes idle, so you know when the cache expires."
          control={
            <Toggle
              label="Prompt cache timer"
              checked={preferences.promptCacheTimer}
              onChange={(value) => update('promptCacheTimer', value)}
            />
          }
        />
        <SettingRow
          label="Agent permissions"
          description="Launch agents with fewer permission prompts or with manual checks."
          control={
            <Segmented
              label="Agent permissions"
              options={AGENT_PERMISSIONS}
              value={preferences.agentPermissions}
              onChange={(value) => update('agentPermissions', value)}
            />
          }
        />
      </SettingsCard>

      <SettingsCard>
        <div className="flex items-center gap-2.5">
          <h2 className="text-row m-0 font-semibold">Installed</h2>
          <span className="border-line bg-bg2 text-fg2 text-chip rounded-pill border px-2 py-px">
            {detectedLabel(installed)}
          </span>
          {/*
            Held to the same standard as the rail's Help button: an affordance that cannot
            work yet is disabled and its tooltip names the release, rather than being live
            and doing nothing. It is not removed, because unlike the seven absent chips
            this one has a target — it re-asks a question nothing can answer yet.
          */}
          <button
            type="button"
            disabled
            title="Nysia cannot ask which agents are installed yet. Detection arrives in v0.2 wave C."
            className="border-line2 bg-bg3 text-fg3 ml-auto flex cursor-default items-center gap-1.5 rounded-control border px-2.5 py-1"
          >
            <span aria-hidden="true">{GLYPH.refresh}</span>
            Refresh
          </button>
        </div>

        {installed.length === 0 ? (
          <p className="text-fg2 text-term m-0 leading-normal">
            Nothing detected. A version Nysia has not read from the agent itself would be a
            guess, and a guessed version looks exactly like a working one until it fails to
            move — so this list stays empty until the daemon can be asked. Detection arrives
            with the agent module in v0.2 wave C.
          </p>
        ) : (
          installed.map((agent) => (
            <InstalledRow
              key={agent.id}
              agent={agent}
              isDefault={agent.id === preferences.defaultAgent}
              onSetDefault={() => update('defaultAgent', agent.id)}
            />
          ))
        )}
      </SettingsCard>
    </>
  );
}

/**
 * The selected-chip tick from design-spec.md §5.
 *
 * `ui/glyphs.ts` is where a glyph belongs, and it says why: the tick and its heavier
 * cousin are one merge apart and a substitution between them is impossible to spot inline.
 * That file is outside this pane's owned paths for this wave, so the character is spelled
 * here with this note rather than dropped from the design; it moves to the catalogue next
 * time someone opens that file.
 */
const TICK = '✓';

/**
 * The *Default agent* chips: 6px radius, and the selected one gets a 1px accent border over
 * an `acc14` fill — the same treatment `AppearancePane`'s theme swatch uses, because it is
 * the same decision being made.
 *
 * The keyboard behaviour is `ui/roving.ts`, the module `Segmented` uses, rather than a
 * second reading of the same APG pattern: one tab stop for the group, arrow keys moving the
 * selection with focus following the check, and a stored value outside the list entering at
 * the first chip instead of leaving the group unreachable. `Segmented` explains at length
 * why each of those matters; the point here is that the two controls on this pane answer
 * the arrow keys the same way, which is the part a user notices.
 *
 * The chip order is `AGENT_CHOICES`; the arrow order is `DEFAULT_AGENT_CHOICES`, which is
 * also what a stored preference is validated against. `agentPreferences.test.ts` holds the
 * two lists together, so an agent cannot be rendered in one order and stepped in another.
 */
function AgentChips({
  value,
  onChange,
}: {
  readonly value: DefaultAgentChoice;
  readonly onChange: (next: DefaultAgentChoice) => void;
}) {
  const buttons = useRef(new Map<DefaultAgentChoice, HTMLButtonElement>());
  const pendingFocus = useRef<DefaultAgentChoice | null>(null);

  useEffect(() => {
    const target = pendingFocus.current;
    if (target === null) {
      return;
    }
    pendingFocus.current = null;
    // Only if the parent took the change, so a request cannot sit pending and steal focus
    // on some later, unrelated render.
    if (target === value) {
      buttons.current.get(target)?.focus();
    }
  });

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!isArrowKey(event.key)) {
      return;
    }
    // Swallowed even when the selection does not move, so an arrow never scrolls the pane
    // out from under the control being operated.
    event.preventDefault();
    const next = nextOption(DEFAULT_AGENT_CHOICES, value, event.key);
    if (next !== undefined) {
      pendingFocus.current = next;
      onChange(next);
    }
  }

  const tabbable = tabbableIndex(DEFAULT_AGENT_CHOICES, value);

  return (
    <div
      role="radiogroup"
      aria-label="Default agent"
      onKeyDown={onKeyDown}
      className="flex flex-wrap gap-2 pb-0.5"
    >
      {AGENT_CHOICES.map((choice, index) => {
        const selected = choice.id === value;
        return (
          <button
            key={choice.id}
            ref={(node) => {
              if (node) {
                buttons.current.set(choice.id, node);
              } else {
                buttons.current.delete(choice.id);
              }
            }}
            type="button"
            role="radio"
            aria-checked={selected}
            tabIndex={index === tabbable ? 0 : -1}
            onClick={() => onChange(choice.id)}
            className={`flex cursor-pointer items-center gap-2 rounded-chip px-3 py-1.5 focus-visible:shadow-focus focus-visible:outline-none ${
              selected ? 'border-acc bg-acc14 text-fg border' : 'border-line2 text-fg2 border'
            }`}
          >
            <SessionGlyph kind={choice.kind} />
            {choice.label}
            {selected ? (
              <span aria-hidden="true" className="text-acc">
                {TICK}
              </span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

/**
 * One *Installed* row: the 30×30 icon tile, the name, the version in mono `fg3`, and the
 * default button.
 *
 * The artboard also puts `Enabled | Disabled`, an open-externally arrow and a chevron on
 * this row. None of the three has anywhere to go — there is no per-agent enable, no
 * external page to open and no row detail in v0.2 — so they are left out on the same terms
 * as the absent chips, rather than drawn as controls that swallow a click.
 */
function InstalledRow({
  agent,
  isDefault,
  onSetDefault,
}: {
  readonly agent: InstalledAgent;
  readonly isDefault: boolean;
  readonly onSetDefault: () => void;
}) {
  return (
    <div className="border-line flex items-center gap-3 border-t pt-3">
      <span
        aria-hidden="true"
        className="border-line bg-bg0 text-acc flex size-[30px] flex-none items-center justify-center rounded-control border"
      >
        {GLYPH.agent}
      </span>
      <span className="text-row font-medium">{agent.name}</span>
      <span className="text-fg3 text-chip font-mono">{agent.version}</span>
      {isDefault ? (
        <span className="border-acc35 text-acc text-chip ml-auto rounded-control border px-2.5 py-1">
          <span aria-hidden="true">{TICK} </span>
          Default
        </span>
      ) : (
        <button
          type="button"
          onClick={onSetDefault}
          className="border-line2 bg-bg3 text-fg2 hover:text-fg text-chip ml-auto cursor-pointer rounded-control border px-2.5 py-1 focus-visible:shadow-focus focus-visible:outline-none"
        >
          Set default
        </button>
      )}
    </div>
  );
}
