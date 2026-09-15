import { describe, expect, it } from 'vitest';

import {
  PERMITTED,
  SESSION_BOUNDARY,
  statusChange,
  statusOf,
  statusRow,
} from '../store/agentStatusFixture';
import {
  AgentStatusFrameError,
  decodeAgentStatusChange,
  parseAgentStatusChange,
} from './agentStatusFrames';

/*
 * What an `agent_status` frame is allowed to be.
 *
 * The round trip is the main case: `nysia-proto` serialises an `AgentStatusChange` with
 * serde and this reads it back, so a change built from the generated types must survive
 * `JSON.stringify` → decode unchanged. Everything else here is a malformed document, and
 * the reason each one is rejected rather than tolerated is the same: a field that arrived
 * as `undefined` produces no error anywhere downstream, it produces a `switch` that matches
 * nothing and a suppression that silently stops applying.
 */

function encode(value: unknown): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

describe('decoding a change', () => {
  it('round-trips a change built from the generated types', () => {
    const change = statusChange(
      statusOf('tab_1:leaf_1', 'waiting', { observedAt: 1_700_000_000_123 }),
    );
    expect(decodeAgentStatusChange(encode(change))).toEqual(change);
  });

  it('round-trips a suppressed decision with its reason', () => {
    // The field the whole feature turns on. If this did not survive the trip, every
    // suppression would arrive as a decision nobody recognises.
    const change = statusChange(
      statusOf('tab_1:leaf_1', 'done', { sessionBoundary: true }),
      SESSION_BOUNDARY,
    );
    const decoded = decodeAgentStatusChange(encode(change));
    expect(decoded.notify).toEqual({
      decision: 'suppressed',
      reason: 'session_boundary',
    });
  });

  it('round-trips a subagent target and its roster', () => {
    const change = statusChange(
      {
        lead: statusRow('tab_1:leaf_1', 'working'),
        subagents: [statusRow('tab_1:leaf_1', 'done', { agentId: 'sub_1' })],
      },
      PERMITTED,
      { target: 'subagent', agentId: 'sub_1' },
    );
    const decoded = decodeAgentStatusChange(encode(change));
    expect(decoded.changed).toEqual({ target: 'subagent', agentId: 'sub_1' });
    expect(decoded.status.subagents[0]?.agentId).toBe('sub_1');
  });

  it('carries the question through without looking at it', () => {
    // `question` is `unknown` on the wire because Claude's `tool_input` is arbitrary JSON.
    // Validating a shape for it here would be the window acquiring an opinion about a field
    // D-13 gives Rust.
    const change = statusChange(statusOf('tab_1:leaf_1', 'waiting'));
    const wire = { ...change, status: { ...change.status, lead: { ...change.status.lead, question: { tool: 'AskUserQuestion', nested: [1, null, 'x'] } } } };
    const decoded = decodeAgentStatusChange(encode(wire));
    expect(decoded.status.lead.question).toEqual({
      tool: 'AskUserQuestion',
      nested: [1, null, 'x'],
    });
  });
});

describe('a payload that is not a change', () => {
  const change = statusChange(statusOf('tab_1:leaf_1', 'done'));

  it('rejects bytes that are not JSON', () => {
    expect(() => decodeAgentStatusChange(new TextEncoder().encode('{'))).toThrow(
      AgentStatusFrameError,
    );
  });

  it('rejects an empty payload rather than reading it as an empty change', () => {
    expect(() => decodeAgentStatusChange(new Uint8Array(0))).toThrow(AgentStatusFrameError);
  });

  it('rejects a decision this build does not know', () => {
    // A daemon speaking a contract this build has not been compiled against. Reading it as
    // "suppressed for some reason" would be guessing on the axis that decides whether the
    // user is interrupted.
    expect(() =>
      parseAgentStatusChange({ ...change, notify: { decision: 'maybe' } }),
    ).toThrow(AgentStatusFrameError);
    expect(() =>
      parseAgentStatusChange({
        ...change,
        notify: { decision: 'suppressed', reason: 'because' },
      }),
    ).toThrow(AgentStatusFrameError);
  });

  it('rejects a fifth state', () => {
    // §2.1 fixes four. A fifth would index off the end of the palette table and paint a dot
    // with no colour at all.
    expect(() =>
      parseAgentStatusChange({
        ...change,
        status: { ...change.status, lead: { ...change.status.lead, state: 'thinking' } },
      }),
    ).toThrow(AgentStatusFrameError);
  });

  it('rejects a row missing any of the eight fields', () => {
    for (const field of [
      'pane',
      'state',
      'isInterrupt',
      'sessionBoundary',
      'agentId',
      'observedAt',
      'restoredUnconfirmed',
    ]) {
      const lead: Record<string, unknown> = { ...change.status.lead };
      delete lead[field];
      expect(
        () => parseAgentStatusChange({ ...change, status: { ...change.status, lead } }),
        field,
      ).toThrow(AgentStatusFrameError);
    }
  });

  it('rejects a target that is neither lead nor subagent', () => {
    expect(() =>
      parseAgentStatusChange({ ...change, changed: { target: 'roster' } }),
    ).toThrow(AgentStatusFrameError);
    expect(() =>
      parseAgentStatusChange({ ...change, changed: { target: 'subagent' } }),
    ).toThrow(AgentStatusFrameError);
  });

  it('rejects a roster that is not an array', () => {
    expect(() =>
      parseAgentStatusChange({
        ...change,
        status: { ...change.status, subagents: { sub_1: {} } },
      }),
    ).toThrow(AgentStatusFrameError);
  });

  it('rejects an array where an object belongs', () => {
    // `typeof [] === 'object'`, so the obvious guard lets a JSON array through and then
    // reads every field as undefined.
    expect(() => parseAgentStatusChange([])).toThrow(AgentStatusFrameError);
  });

  it('says what was wrong, naming the field', () => {
    // A frame error means resubscribe either way; only one kind means there is a daemon bug
    // to go and find, and a message that names the field is what makes that a five-minute
    // job rather than a bisect.
    expect(() =>
      parseAgentStatusChange({
        ...change,
        status: { ...change.status, lead: { ...change.status.lead, observedAt: 'soon' } },
      }),
    ).toThrow(/observedAt/);
  });
});
