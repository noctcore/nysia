import type { AgentId } from './agentPreferences';

/**
 * The *Installed* list on design-spec.md §5's Agents screen, and the one honest answer the
 * webview can give for it today.
 *
 * The artboard draws four rows with versions — Claude `2.1.263`, Codex `0.48.0`, Gemini
 * `0.9.2`, OpenCode `1.2.7`. Three of those agents do not exist in v1 (D-3), so they are
 * absent for the reason the v0.2 delivery plan §1 gives. The fourth is the interesting one:
 * **a version string is a measurement, not a design decision.** `2.1.263` in the mock is
 * the number the designer's machine happened to have, and shipping it as a detected version
 * would be a fabricated record — worse than an empty list, because a fabricated record is
 * indistinguishable from a working feature until someone upgrades and it does not move.
 *
 * Nothing in the webview can take that measurement:
 *
 *  - `RequestPayload` (`src/generated`) carries no detect verb — sessions, terminal I/O,
 *    streams and agent status, and that is all;
 *  - `Store` carries no installed-agent field, and the store contract is not this pane's
 *    to extend;
 *  - `apps/web` may not import `@tauri-apps` outside `src/transport` (D-1, D-2, and the
 *    ESLint ban that enforces it), so there is no local process to ask either.
 *
 * So this returns nothing, and the pane renders an empty state that says why. When the
 * daemon can answer — the agent module is `crates/nysia-core/src/agent/**`, and the verb
 * that would carry its answer to a client belongs to v0.2 wave C — **this function is the
 * only thing that changes.** The row component, the count and the pane are already written
 * against a populated list, and the render test exercises them with a fixture so that path
 * is not first run on the day the verb lands.
 */
export interface InstalledAgent {
  /** Which default-agent choice the row's `Set default` selects. */
  readonly id: AgentId;
  readonly name: string;
  /** Exactly what the agent reported. Never a guess, never a fallback string. */
  readonly version: string;
}

/**
 * Stable identity, so the pane's default argument does not allocate a new array per render.
 */
const NONE_DETECTED: readonly InstalledAgent[] = [];

export function detectInstalledAgents(): readonly InstalledAgent[] {
  return NONE_DETECTED;
}

/**
 * The count pill beside the *Installed* heading.
 *
 * Derived from the list rather than written beside it, because a stated total is a fact
 * about the list beneath it (v0.2 delivery plan §5) — and the artboard's `4 detected` over
 * four hardcoded rows is exactly the shape that can drift. `installedAgents.test.ts` holds
 * the two together.
 */
export function detectedLabel(agents: readonly InstalledAgent[]): string {
  return `${agents.length} detected`;
}
