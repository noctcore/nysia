import { formatAge } from '../format';
import type { AgentState } from '../generated/AgentState';
import type { AgentStatus } from '../generated/AgentStatus';
import type { AgentStatusChange } from '../generated/AgentStatusChange';
import type { AgentStatusRow } from '../generated/AgentStatusRow';
import type { PaneKey } from '../generated/PaneKey';
import { AGENT_STATUS_STALE_AFTER_MS } from '../generated/wireConstants';
import type { StatusTokens } from '../theme/themes';

/**
 * Turning a status row into a dot.
 *
 * **The two vocabularies do not match, and this module is where that is admitted.** The
 * design spec's §1 status palette has four entries — Running, Needs input, Queued,
 * Failed — and the wire's [`AgentState`] has four — `working`, `waiting`, `done`,
 * `interrupted`. They are the same size and they are not the same list, so the join is a
 * decision rather than a rename, and it is written down here rather than spread across two
 * components:
 *
 * | Row | Palette | Why |
 * |---|---|---|
 * | *absent* | the **accent**, not a status colour | A pane with no row yet is not idle — nothing is known about it. The accent dot is v0.1's "an agent lives here", and it stays that until a hook says otherwise. Painting it grey would be a claim. |
 * | `working`, fresh | Running (teal) | The one exact match. |
 * | `working`, stale | Running, decayed | §2.3: a `working` row past thirty minutes decays to *active*. Same token at half weight — **not a fifth colour**, because staleness is not a fifth state. |
 * | `waiting` | Needs input (amber) | The palette entry is named after this exact situation. |
 * | `done` | Queued (grey) | Grey is the palette's "nothing is happening". An agent that reached `Stop` is idle and holding, which is what the Tasks table's Queued pill paints. There is no Done entry and inventing one would make the palette five. |
 * | `interrupted` | Failed (red) | `is_interrupt` means the turn ended because something cut it off. Red is the palette's "this did not finish". |
 *
 * The *colour* comes from the palette; the *wording* comes from the wire state, which is
 * why {@link STATE_TONE} and {@link STATE_LABEL} are two tables rather than one. Calling a
 * finished agent "Queued" in a tooltip would be the join leaking into prose.
 *
 * Nothing here reads `AgentStatusChange.notify`. That decision belongs to
 * `./agentNotifications`, and it belongs there alone.
 */

/**
 * One entry in the design spec's §1 status palette.
 *
 * Taken from `theme/themes.ts` rather than spelled again: the palette has one authority and
 * it is the module that carries the four colour values.
 */
export type StatusTone = keyof StatusTokens;

/**
 * §2.1's four states, mapped onto §1's four palette entries.
 *
 * `satisfies` is load-bearing in both directions. A fifth `AgentState` on the wire fails
 * the typecheck here rather than falling through to an undefined colour, and a tone that is
 * not a palette entry fails too.
 */
export const STATE_TONE = {
  working: 'running',
  waiting: 'needsInput',
  done: 'queued',
  interrupted: 'failed',
} as const satisfies Record<AgentState, StatusTone>;

/**
 * How each palette entry is painted.
 *
 * A Tailwind utility over the token, never a colour literal — `index.css` defines the four
 * `--color-status-*` properties once and the runtime theme switcher never re-emits them, so
 * this is the whole reason the accent picker cannot recolour semantics
 * (`theme/tokens.test.ts` holds the other half).
 */
export const TONE_CLASS = {
  running: 'bg-status-running',
  needsInput: 'bg-status-needs-input',
  queued: 'bg-status-queued',
  failed: 'bg-status-failed',
} as const satisfies Record<StatusTone, string>;

/**
 * What a pane with no status row gets.
 *
 * The accent, deliberately: design-spec.md §3's session dot says "an agent lives here", and
 * that is the only true thing to say about an agent whose first hook has not arrived. Every
 * status colour would be a claim about a lifecycle nobody has reported.
 */
export const UNKNOWN_CLASS = 'bg-acc';

/**
 * A stale `working` dot: the same teal, at half weight.
 *
 * `opacity-*` rather than Tailwind's `/50` colour modifier. The modifier compiles to a CSS
 * colour-mixing function, which is one of the four shapes `theme/colourGuard.ts` hunts for,
 * and a decayed dot is not worth arguing with a guard when a plain opacity utility paints
 * the same pixel.
 */
export const STALE_CLASS = `${TONE_CLASS.running} opacity-50`;

/** The tooltip's wording, from the wire state rather than from the palette entry. */
export const STATE_LABEL = {
  working: 'Working',
  waiting: 'Needs input',
  done: 'Done',
  interrupted: 'Interrupted',
} as const satisfies Record<AgentState, string>;

/**
 * Whether a row is older than §2.3's thirty minutes, as of `now`.
 *
 * The threshold is `AGENT_STATUS_STALE_AFTER_MS` out of `generated/wireConstants`, which
 * `nysia-proto`'s `bindings` module writes from the same Rust constant the daemon uses
 * (D-13). It was briefly a `30 * 60 * 1000` here beside a comment naming that constant as
 * its authority; a comment is not a mechanism, and the generated module is the channel this
 * repository already uses for a value a client needs at run time.
 *
 * Strictly older, and a row from the future is not stale — both matching
 * `AgentStatusRow::is_stale`, which is the implementation this one has to agree with. A
 * daemon on a machine that has just synced NTP can legitimately report an `observed_at`
 * ahead of the window's clock, and calling that stale would decay a dot that is live.
 */
export function isStale(row: AgentStatusRow, now: number): boolean {
  return now - row.observedAt > AGENT_STATUS_STALE_AFTER_MS;
}

/** What a status dot paints, and what it says. */
export interface AgentDot {
  /** The palette entry, or `null` when there is no row and the accent is painted instead. */
  readonly tone: StatusTone | null;
  /** True for a `working` row §2.3 calls stale. Not a state — see the table above. */
  readonly stale: boolean;
  /** The Tailwind utility that colours it. Always a token. */
  readonly className: string;
  /** The accessible name and the tooltip. */
  readonly label: string;
}

/**
 * The dot for one pane's **lead** row.
 *
 * Subagents are deliberately not painted this wave. `AgentStatus.subagents` is on the wire
 * and the roster is real data, but the sidebar shows one row per session and the tab strip
 * one dot per tab — a lead whose dot flickered between its own state and its subagents'
 * would say less, not more. The roster is there for the surface that renders it.
 */
export function agentDot(row: AgentStatusRow | undefined, now: number): AgentDot {
  if (!row) {
    return {
      tone: null,
      stale: false,
      className: UNKNOWN_CLASS,
      label: 'Agent · no status yet',
    };
  }

  const tone = STATE_TONE[row.state];
  const stale = row.state === 'working' && isStale(row, now);
  return {
    tone,
    stale,
    className: stale ? STALE_CLASS : TONE_CLASS[tone],
    // A stale dot has to say why it is dim, or it reads as a rendering bug. The age is the
    // whole answer: "working, and nothing has been heard for 4h".
    label: stale
      ? `${STATE_LABEL[row.state]} · no update for ${formatAge(now, row.observedAt)}`
      : STATE_LABEL[row.state],
  };
}

/**
 * One pane's status out of the list, or `undefined` when the daemon has none for it.
 *
 * A list rather than a map because that is the shape the wire uses — `AgentStatusList`
 * answers with `statuses: Array<AgentStatus>` — and because the number of open panes is
 * tens, not thousands. Keying it client-side would be a second index to keep in step with
 * the frames that arrive.
 */
export function findAgentStatus(
  panes: readonly AgentStatus[],
  paneKey: PaneKey,
): AgentStatus | undefined {
  return panes.find((status) => status.lead.pane === paneKey);
}

/**
 * Fold one change into the pane list.
 *
 * `AgentStatusChange.status` is *the pane's whole status after the change, never a diff*,
 * so this replaces rather than merges — a client that merged would be maintaining a second
 * opinion about a shape the daemon already sends complete. `changed` says which entry
 * moved, which matters to a notification's wording and not at all to the fold.
 *
 * Returns a new array every time, which is what `useSyncExternalStore` needs: a provider
 * that mutated the list in place would notify with a snapshot React considers unchanged.
 */
export function applyAgentStatus(
  panes: readonly AgentStatus[],
  change: AgentStatusChange,
): readonly AgentStatus[] {
  const pane = change.status.lead.pane;
  const index = panes.findIndex((status) => status.lead.pane === pane);
  if (index === -1) {
    return [...panes, change.status];
  }
  return panes.map((status, at) => (at === index ? change.status : status));
}

/**
 * Drop every status whose pane is gone.
 *
 * A provider calls this when its session list shrinks. Without it a closed pane's row
 * outlives the tab that showed it, and the next pane to reuse the key — a `PaneKey` is
 * durable, so reuse is a real thing rather than a hypothetical — would inherit a dot
 * describing something else entirely.
 */
export function retainAgentStatus(
  panes: readonly AgentStatus[],
  live: ReadonlySet<PaneKey>,
): readonly AgentStatus[] {
  const kept = panes.filter((status) => live.has(status.lead.pane));
  return kept.length === panes.length ? panes : kept;
}
