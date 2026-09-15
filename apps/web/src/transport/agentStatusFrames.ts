import type { AgentState } from '../generated/AgentState';
import type { AgentStatus } from '../generated/AgentStatus';
import type { AgentStatusChange } from '../generated/AgentStatusChange';
import type { AgentStatusRow } from '../generated/AgentStatusRow';
import type { Notify } from '../generated/Notify';
import type { StatusTarget } from '../generated/StatusTarget';

/**
 * Reading an `agent_status` frame.
 *
 * `FrameKind::AgentStatus` carries one serialised `AgentStatusChange` as JSON — the pane's
 * whole status rather than a diff, which entry moved, and whether the change may raise a
 * notification. It arrives on the stream id `AgentStatusSubscribe` handed out and on no
 * other, so a frame of this kind on a session's stream is a daemon bug and not a shape to
 * accommodate.
 *
 * **Why this validates rather than casts.** `JSON.parse()` returns `any`, and a cast to the
 * generated type would be the window asserting something about bytes it has not looked at.
 * The cost of being wrong is specific: a change whose `notify` did not survive the trip
 * would arrive as `undefined`, every `switch` on the arm would fall through to nothing, and
 * the suppression that exists to stop a toast on every session start would silently not
 * apply. That is a failure with no error message, which is the kind worth paying a few
 * predicates to convert into one.
 *
 * It checks structure and not values: `state` must be one of the four, because a fifth
 * would index off the end of the palette table, but `question` stays `unknown` because the
 * wire says `unknown` and inventing a shape for Claude's `tool_input` here would be a
 * second authority over a field D-13 gives Rust.
 *
 * Nothing in this module decides whether a change notifies. It hands back the arm the
 * daemon sent; `store/agentNotifications.ts` is the one place that reads it.
 */

/**
 * Why a frame could not be read.
 *
 * Its own class, like `FrameError`, so a caller can tell "the daemon sent a document I
 * cannot parse" from "the socket died". Both mean resubscribe; only one means there is a
 * bug to go and find.
 */
export class AgentStatusFrameError extends Error {
  constructor(message: string) {
    super(`agent_status frame: ${message}`);
    this.name = 'AgentStatusFrameError';
  }
}

const STATES: readonly AgentState[] = ['working', 'waiting', 'done', 'interrupted'];

/**
 * Decode one frame payload.
 *
 * @throws {AgentStatusFrameError} if the payload is not a well-formed `AgentStatusChange`.
 */
export function decodeAgentStatusChange(payload: Uint8Array): AgentStatusChange {
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(payload));
  } catch (cause) {
    throw new AgentStatusFrameError(
      `payload is not JSON (${cause instanceof Error ? cause.message : 'unknown'})`,
    );
  }
  return parseAgentStatusChange(parsed);
}

/**
 * The same checks, over an already-parsed value.
 *
 * Split out because a status also arrives as a *response* — `agent_status_get` and
 * `agent_status_list` answer over the control connection, where the Tauri bridge has done
 * the parsing — and validating one shape in two places is how the two drift.
 */
export function parseAgentStatusChange(value: unknown): AgentStatusChange {
  const change = asObject(value, 'change');
  return {
    status: parseAgentStatus(change['status']),
    changed: parseTarget(change['changed']),
    notify: parseNotify(change['notify']),
  };
}

export function parseAgentStatus(value: unknown): AgentStatus {
  const status = asObject(value, 'status');
  const subagents = status['subagents'];
  if (!Array.isArray(subagents)) {
    throw new AgentStatusFrameError('status.subagents is not an array');
  }
  return {
    lead: parseRow(status['lead'], 'lead'),
    subagents: subagents.map((row, index) => parseRow(row, `subagents[${index}]`)),
  };
}

function parseRow(value: unknown, where: string): AgentStatusRow {
  const row = asObject(value, where);
  const state = row['state'];
  if (typeof state !== 'string' || !isState(state)) {
    throw new AgentStatusFrameError(`${where}.state is not one of the four states`);
  }
  return {
    pane: asString(row['pane'], `${where}.pane`),
    state,
    // Deliberately untouched. `unknown` is what the wire says and what the row says.
    question: row['question'],
    isInterrupt: asBoolean(row['isInterrupt'], `${where}.isInterrupt`),
    sessionBoundary: asBoolean(row['sessionBoundary'], `${where}.sessionBoundary`),
    agentId: asNullableString(row['agentId'], `${where}.agentId`),
    observedAt: asNumber(row['observedAt'], `${where}.observedAt`),
    restoredUnconfirmed: asBoolean(
      row['restoredUnconfirmed'],
      `${where}.restoredUnconfirmed`,
    ),
  };
}

function parseTarget(value: unknown): StatusTarget {
  const target = asObject(value, 'changed');
  switch (target['target']) {
    case 'lead':
      return { target: 'lead' };
    case 'subagent':
      return {
        target: 'subagent',
        agentId: asString(target['agentId'], 'changed.agentId'),
      };
    default:
      throw new AgentStatusFrameError('changed.target is neither lead nor subagent');
  }
}

/**
 * The decision, read as an arm.
 *
 * The `suppressed` reason is checked against the two the wire defines rather than let
 * through as any string: a reason nobody recognises means this build and the daemon
 * disagree about the contract, and reading it as suppressed-for-some-reason would be
 * guessing on exactly the axis that decides whether the user is interrupted.
 */
function parseNotify(value: unknown): Notify {
  const notify = asObject(value, 'notify');
  switch (notify['decision']) {
    case 'permitted':
      return { decision: 'permitted' };
    case 'suppressed': {
      const reason = notify['reason'];
      if (reason !== 'session_boundary' && reason !== 'restored_unconfirmed') {
        throw new AgentStatusFrameError(
          `notify.reason ${JSON.stringify(reason)} is not a suppression this build knows`,
        );
      }
      return { decision: 'suppressed', reason };
    }
    default:
      throw new AgentStatusFrameError(
        'notify.decision is neither permitted nor suppressed',
      );
  }
}

function isState(value: string): value is AgentState {
  return (STATES as readonly string[]).includes(value);
}

function asObject(value: unknown, where: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new AgentStatusFrameError(`${where} is not an object`);
  }
  return value as Record<string, unknown>;
}

function asString(value: unknown, where: string): string {
  if (typeof value !== 'string') {
    throw new AgentStatusFrameError(`${where} is not a string`);
  }
  return value;
}

function asNullableString(value: unknown, where: string): string | null {
  if (value === null) {
    return null;
  }
  return asString(value, where);
}

function asBoolean(value: unknown, where: string): boolean {
  if (typeof value !== 'boolean') {
    throw new AgentStatusFrameError(`${where} is not a boolean`);
  }
  return value;
}

function asNumber(value: unknown, where: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new AgentStatusFrameError(`${where} is not a finite number`);
  }
  return value;
}
