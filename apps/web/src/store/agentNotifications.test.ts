import { describe, expect, it } from 'vitest';

import type { AgentState } from '../generated/AgentState';
import type { AgentStatusChange } from '../generated/AgentStatusChange';
import {
  createAgentNotificationSink,
  NOTIFIABLE,
  NOTIFICATION_CAP,
  type AgentNotificationSink,
} from './agentNotifications';
import {
  LEAD,
  PERMITTED,
  RESTORED,
  SESSION_BOUNDARY,
  statusChange,
  statusOf,
  statusRow,
} from './agentStatusFixture';

/*
 * The rule that is easy to get wrong, held from both sides.
 *
 * §2.1 says a session boundary must never notify, and §2.3 adds a row rehydrated from the
 * spool. Both are decisions the daemon already made and put on the frame as `notify`, and
 * the failure they prevent is specific and loud: a consumer that toasted on
 * `state === 'done'` would fire on every session start, every `/clear`, every resume and
 * every daemon restart — a stack of notices about work that finished half an hour ago.
 *
 * Asserting the suppressed cases raise nothing is necessary and not sufficient: an
 * implementation that read `row.sessionBoundary` instead of `notify` would pass every one
 * of them. The two cases under "reads the decision, not the row" are what separate the two
 * implementations, by feeding shapes where the flag and the arm disagree.
 */

const PANE = 'tab_1:leaf_1';
const EVERY_STATE: readonly AgentState[] = ['working', 'waiting', 'done', 'interrupted'];

/** What the provider was calling the pane's session when the change arrived. */
const SESSION = 'Kirei deps but we already did…';

/**
 * Offer a change under the default label.
 *
 * `report` takes the session name because the sink holds no tabs and a notice's label
 * belongs to the moment it was raised — see `AgentNotification.session`. Every case that is
 * not *about* the label goes through here so the argument does not clutter twenty call
 * sites; the two that are about it call `sink.report` directly with their own.
 */
function report(sink: AgentNotificationSink, change: AgentStatusChange): void {
  sink.report(change, SESSION);
}

describe('which changes deserve a notification', () => {
  it('raises one for an agent that is waiting on the user', () => {
    const sink = createAgentNotificationSink();
    report(sink,statusChange(statusOf(PANE, 'waiting')));

    const notices = sink.getSnapshot();
    expect(notices).toHaveLength(1);
    expect(notices[0]?.pane).toBe(PANE);
    expect(notices[0]?.title).toBe('Needs input');
    expect(notices[0]?.tone).toBe('needsInput');
  });

  it('raises one when a turn ends, either way', () => {
    const sink = createAgentNotificationSink();
    report(sink,statusChange(statusOf(PANE, 'done')));
    report(sink,statusChange(statusOf('tab_2:leaf_1', 'interrupted')));
    expect(sink.getSnapshot().map((notice) => notice.title)).toEqual([
      'Done',
      'Interrupted',
    ]);
  });

  it('raises none for working, which arrives on every tool call', () => {
    // Permitted by the contract and still not worth a toast. The dot is already showing it,
    // and a notice per `PostToolUse` is the noise a status dot exists to replace.
    const sink = createAgentNotificationSink();
    report(sink,statusChange(statusOf(PANE, 'working')));
    expect(sink.getSnapshot()).toEqual([]);
  });

  it('raises none for a subagent', () => {
    // The roster still moves; the user is waiting on the lead, not on its helpers.
    const sink = createAgentNotificationSink();
    report(sink,
      statusChange(statusOf(PANE, 'done'), PERMITTED, {
        target: 'subagent',
        agentId: 'sub_1',
      }),
    );
    expect(sink.getSnapshot()).toEqual([]);
  });

  it('lists every state, so an omission cannot pass for a decision', () => {
    expect(Object.keys(NOTIFIABLE).sort()).toEqual([...EVERY_STATE].sort());
    expect(NOTIFIABLE.working).toBeNull();
    for (const state of ['waiting', 'done', 'interrupted'] as const) {
      expect(NOTIFIABLE[state], state).not.toBeNull();
    }
  });
});

describe('suppression', () => {
  it('raises nothing for a session boundary', () => {
    // The exact trap. `SessionStart` maps to `done` with a session boundary, so this is a
    // change whose state is the one that normally notifies — and it must not.
    const sink = createAgentNotificationSink();
    report(sink,
      statusChange(
        statusOf(PANE, 'done', { sessionBoundary: true }),
        SESSION_BOUNDARY,
      ),
    );
    expect(sink.getSnapshot()).toEqual([]);
  });

  it('raises nothing for a row restored from the spool', () => {
    // §2.3's second reason: a status recovered from disk on daemon start is not an event
    // that just happened, and a window that toasted it would replay every notice the user
    // already dismissed.
    const sink = createAgentNotificationSink();
    report(sink,
      statusChange(
        statusOf(PANE, 'done', { restoredUnconfirmed: true }),
        RESTORED,
      ),
    );
    expect(sink.getSnapshot()).toEqual([]);
  });

  it('survives a burst of session starts, which is what a reconnect looks like', () => {
    const sink = createAgentNotificationSink();
    for (let pane = 1; pane <= 10; pane += 1) {
      report(sink,
        statusChange(
          statusOf(`tab_${pane}:leaf_1`, 'done', { sessionBoundary: true }),
          SESSION_BOUNDARY,
        ),
      );
    }
    expect(sink.getSnapshot()).toEqual([]);
  });
});

describe('reads the decision, not the row', () => {
  /*
   * Both cases feed a frame the daemon does not send, on purpose. They are the only shapes
   * that tell a sink reading `notify` apart from one reading the flags behind it — every
   * frame a real daemon produces has the two agreeing, so every other case in this file
   * passes under either implementation.
   */

  it('suppresses a change whose row looks perfectly notifiable', () => {
    const sink = createAgentNotificationSink();
    const row = statusRow(PANE, 'done');
    expect(row.sessionBoundary).toBe(false);
    expect(row.restoredUnconfirmed).toBe(false);

    report(sink,statusChange({ lead: row, subagents: [] }, SESSION_BOUNDARY));
    expect(sink.getSnapshot()).toEqual([]);
  });

  it('permits a change whose row carries a boundary flag', () => {
    // The mirror image. A sink that re-derived the rule would swallow this; one that reads
    // the arm raises it. Nothing about this asks for the daemon to send such a frame — it
    // asks for the window to have no second opinion about the rule.
    const sink = createAgentNotificationSink();
    report(sink,
      statusChange(statusOf(PANE, 'done', { sessionBoundary: true }), PERMITTED),
    );
    expect(sink.getSnapshot()).toHaveLength(1);
  });
});

describe('the notice list', () => {
  it('keeps the newest and drops the oldest past the cap', () => {
    const sink = createAgentNotificationSink();
    for (let pane = 1; pane <= NOTIFICATION_CAP + 3; pane += 1) {
      report(sink,statusChange(statusOf(`tab_${pane}:leaf_1`, 'done')));
    }

    const notices = sink.getSnapshot();
    expect(notices).toHaveLength(NOTIFICATION_CAP);
    // The newest survives: a notice pushed off the top of the screen is worse than one that
    // was never shown, because nothing tells the user it happened.
    expect(notices[notices.length - 1]?.pane).toBe(
      `tab_${NOTIFICATION_CAP + 3}:leaf_1`,
    );
    expect(notices[0]?.pane).toBe('tab_4:leaf_1');
  });

  it('keeps the label it was raised with, not the pane’s current one', () => {
    /*
     * A `PaneKey` is durable and therefore reusable — `agentStatus.ts` drops a row whose
     * pane is gone and `MockStore` prunes on close, both because reuse is real. A notice
     * that resolved its own title against the current tab list would outlive the session it
     * describes and then be relabelled: "codex · deskmate finished its turn" printed over a
     * shell that had just taken the key.
     *
     * Two notices for one pane under two names is that situation, and both keep their own.
     */
    const sink = createAgentNotificationSink();
    sink.report(statusChange(statusOf(PANE, 'done')), 'codex · deskmate');
    sink.report(statusChange(statusOf(PANE, 'waiting')), 'pwsh · shiroani');

    expect(sink.getSnapshot().map((notice) => notice.session)).toEqual([
      'codex · deskmate',
      'pwsh · shiroani',
    ]);
    // Same pane both times: the label is the thing that differs, which is the whole point.
    expect(new Set(sink.getSnapshot().map((notice) => notice.pane))).toEqual(new Set([PANE]));
  });

  it('gives every notice its own id, even for the same pane and state', () => {
    // The list is keyed on the id and dismissed by it, so a reused id would render
    // duplicate React keys and a dismiss that cleared two notices at once.
    const sink = createAgentNotificationSink();
    report(sink,statusChange(statusOf(PANE, 'done')));
    report(sink,statusChange(statusOf(PANE, 'done')));

    const ids = sink.getSnapshot().map((notice) => notice.id);
    expect(new Set(ids).size).toBe(2);
  });

  it('dismisses by id, and dismissing twice is not an error', () => {
    const sink = createAgentNotificationSink();
    report(sink,statusChange(statusOf(PANE, 'done')));
    const id = sink.getSnapshot()[0]?.id ?? '';

    sink.dismiss(id);
    expect(sink.getSnapshot()).toEqual([]);
    expect(() => sink.dismiss(id)).not.toThrow();
  });

  it('is referentially stable until something changes', () => {
    // Read through `useSyncExternalStore`, which re-renders forever against a sink that
    // allocates per call.
    const sink = createAgentNotificationSink();
    expect(sink.getSnapshot()).toBe(sink.getSnapshot());

    report(sink,statusChange(statusOf(PANE, 'done')));
    const settled = sink.getSnapshot();
    sink.dismiss('nothing_by_this_name');
    expect(sink.getSnapshot()).toBe(settled);
  });

  it('notifies subscribers on a change and stops after unsubscribe', () => {
    const sink = createAgentNotificationSink();
    let calls = 0;
    const unsubscribe = sink.subscribe(() => {
      calls += 1;
    });

    report(sink,statusChange(statusOf(PANE, 'done')));
    expect(calls).toBe(1);

    // A suppressed change is not a change to the list, so it wakes nobody.
    report(sink,statusChange(statusOf(PANE, 'done'), SESSION_BOUNDARY));
    expect(calls).toBe(1);

    unsubscribe();
    report(sink,statusChange(statusOf(PANE, 'waiting')));
    expect(calls).toBe(1);
  });

  it('carries the observation time, not the moment the window read the frame', () => {
    // `observed_at` is when the daemon received the event. A notice stamped on arrival
    // would date a spool drain to now, which is the same mistake the suppression above
    // exists to prevent, in a smaller place.
    const sink = createAgentNotificationSink();
    report(sink,
      statusChange(statusOf(PANE, 'done', { observedAt: 1_234_567_890 }), PERMITTED, LEAD),
    );
    expect(sink.getSnapshot()[0]?.at).toBe(1_234_567_890);
  });

  it('clears everything, so one case cannot leak into the next', () => {
    const sink = createAgentNotificationSink();
    report(sink,statusChange(statusOf(PANE, 'done')));
    sink.clear();
    expect(sink.getSnapshot()).toEqual([]);
  });
});
