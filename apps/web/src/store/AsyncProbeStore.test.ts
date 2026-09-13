import { describe, expect, it } from 'vitest';

import { AsyncProbeStore } from './AsyncProbeStore';
import { createSeedSnapshot } from './mock/seed';
import { describeStoreContract } from './storeContract';

/*
 * The contract, run against a provider shaped like a socket rather than like the mock.
 *
 * This is the proof that `describeStoreContract` is reusable rather than a second copy of
 * `MockStore`'s tests — and the reason it is worth a whole extra store implementation is
 * that the previous version of the contract looked reusable and was not. Passing here
 * without a single `if (isMock)` in the suite is what the claim "W5 swaps the provider and
 * touches no component" rests on.
 *
 * The delay is real, not zero: a provider whose first frame lands on a later macrotask is
 * what forces the suite to wait on `status` instead of reading the snapshot at t=0.
 */
describeStoreContract('AsyncProbeStore', () => new AsyncProbeStore(createSeedSnapshot(), 5));

describe('AsyncProbeStore', () => {
  it('has nothing to show until its first frame arrives', () => {
    const store = new AsyncProbeStore(createSeedSnapshot(), 5);
    const snapshot = store.getSnapshot();
    expect(snapshot.status).toBe('connecting');
    expect(snapshot.tabs).toEqual([]);
    expect(snapshot.projects).toEqual([]);
  });

  it('emits an acknowledgement frame before the state frame', async () => {
    const store = new AsyncProbeStore(createSeedSnapshot(), 0);
    await new Promise((resolve) => setTimeout(resolve, 10));

    let frames = 0;
    store.subscribe(() => {
      frames += 1;
    });
    await store.selectNav('tasks');
    // Two, so a contract that pins the notification count to exactly one cannot pass.
    expect(frames).toBe(2);
  });

  it('allocates a fresh snapshot even for a command that changes nothing', async () => {
    const store = new AsyncProbeStore(createSeedSnapshot(), 0);
    await new Promise((resolve) => setTimeout(resolve, 10));

    const before = store.getSnapshot();
    await store.selectNav(before.nav);
    expect(store.getSnapshot()).not.toBe(before);
  });
});
