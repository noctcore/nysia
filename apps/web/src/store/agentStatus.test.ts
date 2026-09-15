import { describe, expect, it } from 'vitest';

import type { AgentState } from '../generated/AgentState';
import { AGENT_STATUS_STALE_AFTER_MS } from '../generated/wireConstants';
import { STATUS_TOKENS } from '../theme/themes';
import {
  agentDot,
  applyAgentStatus,
  findAgentStatus,
  isStale,
  retainAgentStatus,
  STALE_CLASS,
  STATE_LABEL,
  STATE_TONE,
  TONE_CLASS,
  UNKNOWN_CLASS,
} from './agentStatus';
import { statusChange, statusOf, statusRow } from './agentStatusFixture';

/*
 * The join between two four-entry lists that are not the same list.
 *
 * Most of what is asserted here is a *decision* rather than an invariant — that a finished
 * agent is grey and an interrupted one is red is a reading of the design spec, not a fact
 * derivable from the wire. It is asserted anyway, because the alternative is that the next
 * person changes it while looking at one component and nothing anywhere disagrees.
 */

const EVERY_STATE: readonly AgentState[] = ['working', 'waiting', 'done', 'interrupted'];
const NOW = 1_700_000_000_000;

describe('the state → palette mapping', () => {
  it('maps exactly the four states §2.1 fixes, and no others', () => {
    // A total that is a fact about the list beneath it: if the wire grows a fifth state the
    // `satisfies` in `agentStatus.ts` fails the typecheck, and if this list drifts from the
    // generated union this case fails. Neither can move alone.
    expect(Object.keys(STATE_TONE).sort()).toEqual([...EVERY_STATE].sort());
    expect(Object.keys(STATE_LABEL).sort()).toEqual([...EVERY_STATE].sort());
  });

  it('paints each state with the palette entry design-spec.md §1 names', () => {
    expect(STATE_TONE.working).toBe('running');
    expect(STATE_TONE.waiting).toBe('needsInput');
    // Not a Done entry — the palette has four and Queued is its "nothing is happening".
    expect(STATE_TONE.done).toBe('queued');
    expect(STATE_TONE.interrupted).toBe('failed');
  });

  it('has a class for every palette entry and no class for anything else', () => {
    // Tied to `theme/themes.ts` rather than to a second list of four names: a palette entry
    // added there without a utility here would otherwise be a dot with no colour.
    expect(Object.keys(TONE_CLASS).sort()).toEqual(Object.keys(STATUS_TOKENS).sort());
  });

  it('names a status token in every class and never an accent one', () => {
    // The spec's sentence, mechanised: accent is identity, status is semantics, and the
    // accent picker must not recolour these. `theme/tokens.test.ts` holds the other half —
    // that the switcher never emits a status variable at all.
    for (const [tone, className] of Object.entries(TONE_CLASS)) {
      expect(className, tone).toContain('status-');
      expect(className, tone).not.toContain('acc');
    }
  });

  it('keeps the wording out of the palette', () => {
    // `done` is painted with the Queued entry and must not be *called* Queued: the colour
    // is a join and the label is the state.
    expect(STATE_LABEL.done).toBe('Done');
    expect(STATE_TONE.done).toBe('queued');
  });
});

describe('staleness', () => {
  it('is §2.3’s thirty minutes, as Rust generated it', () => {
    // Not a pin on a transcribed copy any more. `nysia-proto`'s `bindings` module writes
    // this from `AGENT_STATUS_STALE_AFTER_MS`, `pnpm ts-drift` fails if the committed
    // output and the crate disagree, and the crate's own test asserts the export against
    // the constant — so this case reads the value rather than restating it, and what it
    // adds is that thirty minutes is what §2.3 asked for.
    expect(AGENT_STATUS_STALE_AFTER_MS).toBe(30 * 60 * 1000);
  });

  it('is strictly older, so a row exactly on the boundary is still fresh', () => {
    const row = statusRow('tab_1:leaf_1', 'working', { observedAt: NOW });
    expect(isStale(row, NOW)).toBe(false);
    expect(isStale(row, NOW + AGENT_STATUS_STALE_AFTER_MS)).toBe(false);
    expect(isStale(row, NOW + AGENT_STATUS_STALE_AFTER_MS + 1)).toBe(true);
  });

  it('never calls a row from the future stale', () => {
    // A daemon on a machine that has just synced NTP legitimately reports an `observed_at`
    // ahead of this window's clock. `AgentStatusRow::is_stale` treats that as fresh, and
    // the two implementations have to agree or the same row decays on one side only.
    const row = statusRow('tab_1:leaf_1', 'working', { observedAt: NOW + 60_000 });
    expect(isStale(row, NOW)).toBe(false);
  });

  it('decays only a working dot, and never into a fifth colour', () => {
    const old = NOW - AGENT_STATUS_STALE_AFTER_MS - 1;

    const working = agentDot(statusRow('tab_1:leaf_1', 'working', { observedAt: old }), NOW);
    expect(working.stale).toBe(true);
    expect(working.tone).toBe('running');
    expect(working.className).toBe(STALE_CLASS);
    // Still the Running token. The decay is weight, not hue: a new colour here would be the
    // fifth state §2.1 says does not exist, wearing a different hat.
    expect(working.className).toContain(TONE_CLASS.running);

    // An old `waiting` row is not stale. Someone who has not answered a question for an
    // hour is still being waited on, and dimming that dot would hide the one state that
    // needs them.
    const waiting = agentDot(statusRow('tab_1:leaf_1', 'waiting', { observedAt: old }), NOW);
    expect(waiting.stale).toBe(false);
    expect(waiting.className).toBe(TONE_CLASS.needsInput);
  });

  it('says how old a decayed dot is, so it does not read as a rendering bug', () => {
    const dot = agentDot(
      statusRow('tab_1:leaf_1', 'working', { observedAt: NOW - 4 * 60 * 60 * 1000 }),
      NOW,
    );
    expect(dot.label).toContain('Working');
    expect(dot.label).toContain('4h');
  });
});

describe('a pane with no row', () => {
  it('keeps the accent rather than claiming a lifecycle', () => {
    // The case that is real in production for every pane, for as long as it takes the first
    // hook to arrive — and permanently for a shell. Grey would say "queued", which is a
    // claim about something nobody has reported.
    const dot = agentDot(undefined, NOW);
    expect(dot.tone).toBeNull();
    expect(dot.stale).toBe(false);
    expect(dot.className).toBe(UNKNOWN_CLASS);
    expect(dot.className).toContain('acc');
    expect(dot.label).toContain('no status yet');
  });

  it('is the only dot that uses the accent', () => {
    for (const state of EVERY_STATE) {
      const dot = agentDot(statusRow('tab_1:leaf_1', state), NOW);
      expect(dot.className, state).not.toContain('acc');
    }
  });
});

describe('every state produces a dot', () => {
  it('has a tone, a token class and a label for each', () => {
    for (const state of EVERY_STATE) {
      const dot = agentDot(statusRow('tab_1:leaf_1', state), NOW);
      expect(dot.tone, state).toBe(STATE_TONE[state]);
      expect(dot.className, state).toBe(TONE_CLASS[STATE_TONE[state]]);
      expect(dot.label, state).toBe(STATE_LABEL[state]);
    }
  });
});

describe('folding a change into the pane list', () => {
  it('appends a pane it has not seen', () => {
    const folded = applyAgentStatus([], statusChange(statusOf('tab_1:leaf_1', 'working')));
    expect(folded).toHaveLength(1);
    expect(folded[0]?.lead.state).toBe('working');
  });

  it('replaces the pane’s whole status rather than merging it', () => {
    // The wire sends the pane's whole status, never a diff, so a client that merged would
    // be maintaining a second opinion about a document that arrived complete — and a
    // subagent that left the roster would never leave this list.
    const before = [
      { lead: statusRow('tab_1:leaf_1', 'working'), subagents: [statusRow('tab_1:leaf_1', 'working', { agentId: 'sub_1' })] },
    ];
    const after = applyAgentStatus(before, statusChange(statusOf('tab_1:leaf_1', 'done')));
    expect(after).toHaveLength(1);
    expect(after[0]?.lead.state).toBe('done');
    expect(after[0]?.subagents).toEqual([]);
  });

  it('leaves every other pane exactly as it was', () => {
    const other = statusOf('tab_2:leaf_1', 'waiting');
    const after = applyAgentStatus(
      [statusOf('tab_1:leaf_1', 'working'), other],
      statusChange(statusOf('tab_1:leaf_1', 'done')),
    );
    expect(after[1]).toBe(other);
  });

  it('allocates a new array, because React compares by reference', () => {
    const before = [statusOf('tab_1:leaf_1', 'working')];
    expect(applyAgentStatus(before, statusChange(statusOf('tab_1:leaf_1', 'working')))).not.toBe(
      before,
    );
  });

  it('finds a pane by key and admits when it has none', () => {
    const panes = [statusOf('tab_1:leaf_1', 'working'), statusOf('tab_2:leaf_1', 'done')];
    expect(findAgentStatus(panes, 'tab_2:leaf_1')?.lead.state).toBe('done');
    expect(findAgentStatus(panes, 'tab_9:leaf_1')).toBeUndefined();
  });
});

describe('dropping a status whose pane is gone', () => {
  it('keeps only the panes still open', () => {
    // A `PaneKey` is durable and therefore reusable. A row that outlived its tab would
    // eventually be shown against a different session under the same key, which is a dot
    // that is not merely stale but wrong.
    const panes = [statusOf('tab_1:leaf_1', 'working'), statusOf('tab_2:leaf_1', 'done')];
    const kept = retainAgentStatus(panes, new Set(['tab_2:leaf_1']));
    expect(kept.map((status) => status.lead.pane)).toEqual(['tab_2:leaf_1']);
  });

  it('hands back the same array when nothing was dropped', () => {
    // So a provider that calls this on every session refresh does not notify React with a
    // new array on every frame that changed nothing.
    const panes = [statusOf('tab_1:leaf_1', 'working')];
    expect(retainAgentStatus(panes, new Set(['tab_1:leaf_1']))).toBe(panes);
  });
});
