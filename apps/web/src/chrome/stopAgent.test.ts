import { describe, expect, it } from 'vitest';

import {
  DEFAULT_GENERAL,
  loadGeneral,
  saveGeneral,
} from '../settings/generalPreferences';
import { memoryStorage } from '../settings/memoryStorage';
import { statusOf } from '../store/agentStatusFixture';
import { openQuestion, workAtStake } from './stopAgent';
import { AGENT, NOW, OTHER_AGENT, SHELL, settled, stopScene } from './stopAgentFixture';

/*
 * Which tab closes ask first, and what each answer does to the session.
 *
 * The gate is driven against a real `MockStore` through `routeCommands`, and every case that
 * says a session survived waits for commands to settle before looking — see `settled()`.
 */

const HOUR = 60 * 60 * 1000;

describe('what closing a pane puts at stake', () => {
  it('names the work a working agent would lose', () => {
    expect(workAtStake(statusOf(AGENT.paneKey, 'working', { observedAt: NOW }), NOW)).toBe(
      "Closing this tab will stop the agent's current work.",
    );
  });

  it('calls a waiting agent waiting, not working', () => {
    // Mid-turn, so there is a turn to lose — but nothing is running, and a sentence about
    // "current work" would describe a different state from the amber dot beside it.
    const reason = workAtStake(statusOf(AGENT.paneKey, 'waiting', { observedAt: NOW }), NOW);
    expect(reason).toContain('waits on your answer');
    expect(reason).not.toContain('current work');
  });

  it('has nothing to say about a turn that already ended, or an agent never heard from', () => {
    for (const state of ['done', 'interrupted'] as const) {
      expect(workAtStake(statusOf(AGENT.paneKey, state, { observedAt: NOW }), NOW), state).toBe(
        null,
      );
    }
    expect(workAtStake(undefined, NOW)).toBe(null);
  });

  it('says how long a quiet working agent has been quiet instead of calling its work current', () => {
    // A long tool call reports nothing until it finishes, so this still asks; what it must
    // not do is state as current what the decayed dot beside it no longer claims.
    const reason = workAtStake(
      statusOf(AGENT.paneKey, 'working', { observedAt: NOW - 2 * HOUR }),
      NOW,
    );
    expect(reason).toContain('2h ago');
    expect(reason).not.toContain("the agent's current work");
  });
});

describe('which question the strip shows', () => {
  const scene = stopScene([[AGENT.paneKey, 'working']]);
  scene.gate.request(AGENT);
  const pending = scene.gate.getSnapshot();
  const tabs = scene.store.getSnapshot().tabs;

  it('shows the question while the session it asks about is in the strip', () => {
    expect(pending).not.toBe(null);
    expect(openQuestion(pending, tabs)).toBe(pending);
    expect(openQuestion(null, tabs)).toBe(null);
  });

  it('drops it once that tab has left the strip', () => {
    expect(openQuestion(pending, tabs.filter((tab) => tab.paneKey !== AGENT.paneKey))).toBe(
      null,
    );
  });

  it('does not hand it to a new session that reuses the pane key', () => {
    // A `PaneKey` is durable and reused; the session handle is not. A new session under the
    // old key is not mid-turn because the old one was.
    const reopened = tabs.map((tab) =>
      tab.paneKey === AGENT.paneKey ? { ...tab, handle: 'sess_reopened' } : tab,
    );
    expect(openQuestion(pending, reopened)).toBe(null);
  });
});

describe('the stop gate', () => {
  it('closes an idle agent without asking', async () => {
    for (const state of ['done', 'interrupted'] as const) {
      const scene = stopScene([[AGENT.paneKey, state]]);
      scene.gate.request(AGENT);
      expect(scene.gate.getSnapshot(), state).toBe(null);
      await settled();
      expect(scene.isOpen(AGENT), state).toBe(false);
    }
  });

  it('closes an agent nobody has heard from without asking', async () => {
    const scene = stopScene();
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()).toBe(null);
    await settled();
    expect(scene.isOpen(AGENT)).toBe(false);
  });

  it('closes a shell without asking', async () => {
    const scene = stopScene([[AGENT.paneKey, 'working']]);
    scene.gate.request(SHELL);
    expect(scene.gate.getSnapshot()).toBe(null);
    await settled();
    expect(scene.isOpen(SHELL)).toBe(false);
  });

  it('asks before closing a working agent, and closes nothing while it asks', async () => {
    const scene = stopScene([[AGENT.paneKey, 'working']]);
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()?.tab.paneKey).toBe(AGENT.paneKey);
    expect(scene.gate.getSnapshot()?.reason).toContain('current work');
    await settled();
    expect(scene.isOpen(AGENT)).toBe(true);
  });

  it('asks before closing an agent that is waiting on you', async () => {
    const scene = stopScene([[AGENT.paneKey, 'waiting']]);
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()?.reason).toContain('waits on your answer');
    await settled();
    expect(scene.isOpen(AGENT)).toBe(true);
  });

  it('asks before closing a working agent that has gone quiet', async () => {
    const scene = stopScene([[AGENT.paneKey, 'working', NOW - 2 * HOUR]]);
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()?.reason).toContain('2h ago');
  });

  it('asks about a shell tab whose pane reports a turn', () => {
    // `claude` started by hand in a shell tab reports against the shell's pane, and closing
    // the tab stops it all the same. The row decides, not the tab's kind.
    const scene = stopScene([[SHELL.paneKey, 'working']]);
    scene.gate.request(SHELL);
    expect(scene.gate.getSnapshot()?.tab.paneKey).toBe(SHELL.paneKey);
  });

  it('Cancel leaves the session alive', async () => {
    const scene = stopScene([[AGENT.paneKey, 'working']]);
    let settledCalls = 0;
    scene.gate.request(AGENT, () => {
      settledCalls += 1;
    });
    scene.gate.cancel();
    await settled();
    expect(scene.isOpen(AGENT)).toBe(true);
    expect(scene.gate.getSnapshot()).toBe(null);
    // Nothing closed, so nothing moves focus as if it had.
    expect(settledCalls).toBe(0);
  });

  it('Stop agent ends the session and settles once', async () => {
    const scene = stopScene([[AGENT.paneKey, 'working']]);
    let settledCalls = 0;
    scene.gate.request(AGENT, () => {
      settledCalls += 1;
    });
    scene.gate.confirm();
    expect(scene.gate.getSnapshot()).toBe(null);
    await settled();
    expect(scene.isOpen(AGENT)).toBe(false);
    expect(settledCalls).toBe(1);
  });

  it('does not ask a user who switched the question off', async () => {
    const storage = memoryStorage();
    saveGeneral(storage, { ...DEFAULT_GENERAL, confirmStopAgent: false });
    const scene = stopScene([[AGENT.paneKey, 'working']], storage);
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()).toBe(null);
    await settled();
    expect(scene.isOpen(AGENT)).toBe(false);
  });

  it('reads the preference when the tab closes, not when the gate was made', () => {
    const scene = stopScene([
      [AGENT.paneKey, 'working'],
      [OTHER_AGENT.paneKey, 'working'],
    ]);
    saveGeneral(scene.storage, { ...DEFAULT_GENERAL, confirmStopAgent: false });
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()).toBe(null);

    saveGeneral(scene.storage, DEFAULT_GENERAL);
    scene.gate.request(OTHER_AGENT);
    expect(scene.gate.getSnapshot()?.tab.paneKey).toBe(OTHER_AGENT.paneKey);
  });

  it("turns the question off with Don't ask again, keeping every other preference", async () => {
    const storage = memoryStorage();
    saveGeneral(storage, { ...DEFAULT_GENERAL, model: 'Sonnet 5', completionSound: true });
    const scene = stopScene(
      [
        [AGENT.paneKey, 'working'],
        [OTHER_AGENT.paneKey, 'working'],
      ],
      storage,
    );
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()?.dontAskAgain, 'the box starts unticked').toBe(false);
    scene.gate.setDontAskAgain(true);
    scene.gate.confirm();

    expect(loadGeneral(storage)).toEqual({
      ...DEFAULT_GENERAL,
      model: 'Sonnet 5',
      completionSound: true,
      confirmStopAgent: false,
    });

    // And it holds: the next working agent closes without a question.
    scene.gate.request(OTHER_AGENT);
    expect(scene.gate.getSnapshot()).toBe(null);
    await settled();
    expect(scene.isOpen(OTHER_AGENT)).toBe(false);
  });

  it('drops a question left up for a tab that has gone when the next close arrives', async () => {
    const scene = stopScene([[AGENT.paneKey, 'working']]);
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()).not.toBe(null);
    scene.gate.request(SHELL);
    expect(scene.gate.getSnapshot()).toBe(null);
    await settled();
    // The shell closed, and the agent the old question was about did not.
    expect(scene.isOpen(SHELL)).toBe(false);
    expect(scene.isOpen(AGENT)).toBe(true);
  });

  it('leaves the preference alone when the box is ticked and the answer is Cancel', () => {
    const scene = stopScene([[AGENT.paneKey, 'working']]);
    scene.gate.request(AGENT);
    scene.gate.setDontAskAgain(true);
    scene.gate.cancel();
    expect(loadGeneral(scene.storage).confirmStopAgent).toBe(true);

    // The next question starts from an unticked box rather than inheriting the last one.
    scene.gate.request(AGENT);
    expect(scene.gate.getSnapshot()?.dontAskAgain).toBe(false);
  });
});
