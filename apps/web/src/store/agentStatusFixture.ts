import type { AgentState } from '../generated/AgentState';
import type { AgentStatus } from '../generated/AgentStatus';
import type { AgentStatusChange } from '../generated/AgentStatusChange';
import type { AgentStatusRow } from '../generated/AgentStatusRow';
import type { Notify } from '../generated/Notify';
import type { PaneKey } from '../generated/PaneKey';
import type { StatusTarget } from '../generated/StatusTarget';

/**
 * Rows and changes, built the way the daemon builds them.
 *
 * A plain module rather than a `.test.ts`, like `storeContract.ts`: four suites need the
 * same eight-field row and a fifth would otherwise copy it. Copying matters more here than
 * it usually does — every field has a default that makes the row *notifiable*, so a suite
 * that hand-rolled a row and forgot one would be testing a shape the daemon never sends.
 *
 * The defaults are deliberate: no session boundary, not restored, no subagent, `notify`
 * permitted. A case that wants suppression has to ask for it, which is what makes
 * `notify: suppressed(...)` visible at the call site rather than buried in a helper.
 */

export function statusRow(
  pane: PaneKey,
  state: AgentState,
  overrides: Partial<AgentStatusRow> = {},
): AgentStatusRow {
  return {
    pane,
    state,
    question: null,
    isInterrupt: state === 'interrupted',
    sessionBoundary: false,
    agentId: null,
    observedAt: 1_700_000_000_000,
    restoredUnconfirmed: false,
    ...overrides,
  };
}

export function statusOf(
  pane: PaneKey,
  state: AgentState,
  overrides: Partial<AgentStatusRow> = {},
): AgentStatus {
  return { lead: statusRow(pane, state, overrides), subagents: [] };
}

export const PERMITTED: Notify = { decision: 'permitted' };

export const SESSION_BOUNDARY: Notify = {
  decision: 'suppressed',
  reason: 'session_boundary',
};

export const RESTORED: Notify = {
  decision: 'suppressed',
  reason: 'restored_unconfirmed',
};

export const LEAD: StatusTarget = { target: 'lead' };

export function statusChange(
  status: AgentStatus,
  notify: Notify = PERMITTED,
  changed: StatusTarget = LEAD,
): AgentStatusChange {
  return { status, changed, notify };
}
