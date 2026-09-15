import { describe, expect, it } from 'vitest';

import { createSeedSnapshot } from './mock/seed';
import { observable } from './storeContract';

/*
 * The proof for the trade `observable()` makes.
 *
 * It compares telemetry by shape rather than by value, so a provider streaming metrics
 * stays green and one that empties or reshapes them as a side effect of an unrelated
 * command goes red. Both halves are asserted here, because "we loosened it but not too
 * far" is exactly the kind of claim that is true when written and false a month later.
 */
const base = createSeedSnapshot();

describe('observable', () => {
  it('ignores telemetry that has only moved', () => {
    const ticked = {
      ...base,
      daemon: { ...base.daemon, memoryBytes: base.daemon.memoryBytes + 4096 },
      usage: base.usage.map((window) => ({ ...window, percentLeft: window.percentLeft - 1 })),
    };
    expect(observable(ticked)).toEqual(observable(base));
  });

  it('ignores a status flicker and a newly recorded failure', () => {
    const busy = {
      ...base,
      status: 'reconnecting' as const,
      errors: [{ id: 'e1', command: 'openTab' as const, message: 'no', at: 0 }],
    };
    expect(observable(busy)).toEqual(observable(base));
  });

  it('ignores a quota list that is not there yet', () => {
    // Populated-ness looks structural and is not: an empty list is the value
    // `emptySnapshot()` ships, so requiring entries would fail a provider that reaches
    // ready on its handshake and samples quota on the next frame. That is timing coupling,
    // which is the thing this comparison exists to avoid.
    expect(observable({ ...base, usage: [] })).toEqual(observable(base));
  });

  it('ignores a status row that arrived mid-command', () => {
    // A hook fires while a `selectTab` is in flight and the pane list legitimately moves
    // under it. Requiring `agentStatus` to be frozen across an unrelated command would fail
    // a correct provider on a dot that ticked — and unlike the metrics, this one is *never*
    // a command's doing, so holding it still is the transport's business and not the
    // contract's. `agentStatus.test.ts` is what holds the fold to replacing one pane.
    const arrived = { ...base, agentStatus: [] };
    expect(observable(arrived)).toEqual(observable(base));
    expect(base.agentStatus.length).toBeGreaterThan(0);
  });

  it('catches a command that reshaped the metrics object', () => {
    // A provider that replaced `daemon` with something of a different shape — the case
    // the plain omission stopped seeing.
    const reshaped = { terminalCount: 1, worktreeCount: 1 } as typeof base.daemon;
    expect(observable({ ...base, daemon: reshaped })).not.toEqual(observable(base));
  });

  it('still catches the session state it was always there to catch', () => {
    expect(observable({ ...base, nav: 'tasks' })).not.toEqual(observable(base));
    expect(observable({ ...base, activeTab: null })).not.toEqual(observable(base));
    expect(observable({ ...base, tabs: base.tabs.slice(1) })).not.toEqual(observable(base));
  });
});
