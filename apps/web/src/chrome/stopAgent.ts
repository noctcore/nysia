import { formatAge } from '../format';
import type { AgentState } from '../generated/AgentState';
import type { AgentStatus } from '../generated/AgentStatus';
import { loadGeneral, saveGeneral } from '../settings/generalPreferences';
import { settingsStorage } from '../settings/storage';
import { agentDot, findAgentStatus } from '../store/agentStatus';
import type { StoreCommands } from '../store/commands';
import type { Tab } from '../store/types';

/**
 * Whether closing a tab ends work somebody would want back, and the question it asks if so.
 *
 * Closing a tab ends its session. For a shell that is usually nothing, and for an agent
 * between turns it is nothing either; for an agent **mid-turn** it is the rest of that turn,
 * cut off wherever it had got to — half an edit, a test run, a plan it was partway through.
 * That is the one case worth a question, and a question anywhere else is nagging.
 *
 * **Mid-turn means the pane's lead row is `working` or `waiting`.** §2.1 maps both from
 * events inside a turn — a prompt submitted, a tool about to run or just finished, a
 * permission request, an `AskUserQuestion` — and only `Stop` or a session boundary leaves
 * them. `done` and `interrupted` are a turn that already ended, and no row at all is an
 * agent nobody has heard from, which is not the same thing as a busy one.
 *
 * **The row decides, not `Tab.kind`.** Every session carries `NYSIA_PANE_KEY` and the daemon
 * resolves a hook's pane from the process tree, so `claude` started by hand inside a shell
 * tab reports its turns against that shell's pane — and closing the tab stops that turn just
 * the same. A shell with only a build in it has no row and closes without a word, which is
 * the case `kind` could not have told apart.
 *
 * **Only the lead.** Subagent rows are left alone for the reason `agentDot` gives and one
 * more: `SubagentStop` maps to no state, so a finished subagent's row keeps whatever it last
 * said, and reading the roster would ask about agents that stopped hours ago.
 *
 * The sentence and the decision are one table on purpose. There is no path that asks
 * without a sentence to ask with, so "will stop the agent's current work" cannot be shown
 * for an agent that has none.
 */
const AT_STAKE = {
  working: "Closing this tab will stop the agent's current work.",
  waiting: "Closing this tab will end the agent's turn while it waits on your answer.",
  done: null,
  interrupted: null,
} as const satisfies Record<AgentState, string | null>;

/**
 * What closing the pane would cut off, as the sentence the dialog says, or `null` when
 * closing it loses nothing and nobody should be asked.
 *
 * A `working` row past §2.3's thirty minutes still asks — a long tool call reports nothing
 * until it finishes, so a quiet agent is as likely to be busy as gone — but it says what is
 * known rather than what is assumed: the dot beside it is already decayed, and a sentence
 * that stated the work as current would be claiming more than the dot does.
 */
export function workAtStake(status: AgentStatus | undefined, now: number): string | null {
  if (status === undefined) {
    return null;
  }
  const { lead } = status;
  const sentence = AT_STAKE[lead.state];
  if (sentence !== null && agentDot(lead, now).stale) {
    return (
      `It last reported working ${formatAge(now, lead.observedAt)} ago. ` +
      'Closing this tab will stop that work if it is still going.'
    );
  }
  return sentence;
}

/** The dialog's heading. */
export const STOP_TITLE = 'Stop this agent?';

/**
 * The pending close, while the session it asks about is still in the strip; else `null`.
 *
 * A tab can leave the strip with the question up — closed from somewhere else — and then
 * there is nothing left to stop, and Stop would only earn a "no longer open" notice. The
 * handle is compared as well as the pane key because a `PaneKey` is durable and gets reused:
 * a new session opened under the old key is not the one the question was about, and it
 * must not inherit a dialog saying it is mid-turn.
 */
export function openQuestion(
  pending: PendingStop | null,
  tabs: readonly Tab[],
): PendingStop | null {
  if (pending === null) {
    return null;
  }
  const { paneKey, handle } = pending.tab;
  return tabs.some((tab) => tab.paneKey === paneKey && tab.handle === handle) ? pending : null;
}

/** A close that is waiting on an answer. */
export interface PendingStop {
  readonly tab: Tab;
  /** The row that made it ask, as it stood when the close was requested. */
  readonly status: AgentStatus;
  /** What closing it would cut off, from {@link workAtStake}. */
  readonly reason: string;
  /** When it was asked, so the dialog's dot is read against the same clock as the reason. */
  readonly askedAt: number;
  /** Whether *Don't ask again* is ticked. Unticked every time the dialog opens. */
  readonly dontAskAgain: boolean;
}

/**
 * The one way the tab strip closes a tab.
 *
 * An external store rather than component state, for the reason the rest of this package
 * reads through `useSyncExternalStore` and for one of its own: every verb the dialog's
 * buttons reach is here, so a node-only suite (D-18) can press Cancel and then look at the
 * store to see whether the session survived. With the pending close in a `useState`, the
 * last line between the button and `closeTab` would be wiring no test could reach.
 *
 * The dialog shows a *snapshot*. A row that moves while it is open — the agent finishes, or
 * stops to ask something — does not rewrite the question under the pointer; the answer is
 * to the question that was asked, and closing an agent that has since gone idle loses
 * nothing.
 */
export interface StopGate {
  /** Referentially stable between changes — read through `useSyncExternalStore`. */
  getSnapshot(): PendingStop | null;
  subscribe(listener: () => void): () => void;
  /**
   * Close `tab` now, or hold it and ask, according to what its pane is doing and whether the
   * user still wants to be asked.
   *
   * The preference is read here, at the moment of closing, rather than when the strip
   * mounted: Settings replaces the strip while it is open, but a value read once would make
   * a change there depend on a remount to take effect.
   *
   * `onSettled` runs once the close settles, as it does for `closeTab`, and never on Cancel
   * — nothing closed, so there is nothing to move focus after.
   */
  request(tab: Tab, onSettled?: () => void): void;
  setDontAskAgain(value: boolean): void;
  /** Answer *Cancel*: forget the close and change nothing else, the box included. */
  cancel(): void;
  /** Answer *Stop agent*: close the tab, turning the question off first if the box is ticked. */
  confirm(): void;
}

export interface StopGateOptions {
  /** Where the General preferences live. `settingsStorage` outside tests. */
  readonly storage?: () => Storage | undefined;
  readonly now?: () => number;
}

export function createStopGate(
  commands: Pick<StoreCommands, 'getSnapshot' | 'closeTab'>,
  { storage = settingsStorage, now = Date.now }: StopGateOptions = {},
): StopGate {
  let pending: PendingStop | null = null;
  // Beside the snapshot rather than in it: a callback is not something a render reads.
  let settle: (() => void) | undefined;
  const listeners = new Set<() => void>();

  function emit(next: PendingStop | null): void {
    pending = next;
    for (const listener of [...listeners]) {
      listener();
    }
  }

  return {
    getSnapshot: () => pending,

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },

    request(tab, onSettled) {
      // A new close supersedes any question still held for a tab that left the strip. The
      // strip already hides that one (`openQuestion`); this is what stops it being kept.
      if (pending !== null) {
        settle = undefined;
        emit(null);
      }
      const askedAt = now();
      const status = findAgentStatus(commands.getSnapshot().agentStatus, tab.paneKey);
      const reason = workAtStake(status, askedAt);
      if (status === undefined || reason === null || !loadGeneral(storage()).confirmStopAgent) {
        commands.closeTab(tab.paneKey, onSettled);
        return;
      }
      settle = onSettled;
      emit({ tab, status, reason, askedAt, dontAskAgain: false });
    },

    setDontAskAgain(value) {
      if (pending !== null && pending.dontAskAgain !== value) {
        emit({ ...pending, dontAskAgain: value });
      }
    },

    cancel() {
      if (pending === null) {
        return;
      }
      settle = undefined;
      emit(null);
    },

    confirm() {
      if (pending === null) {
        return;
      }
      const { tab, dontAskAgain } = pending;
      const onSettled = settle;
      settle = undefined;
      if (dontAskAgain) {
        // Read-modify-write through the same parser the Settings pane uses, so turning this
        // one question off keeps every other General preference the user set.
        const target = storage();
        saveGeneral(target, { ...loadGeneral(target), confirmStopAgent: false });
      }
      emit(null);
      commands.closeTab(tab.paneKey, onSettled);
    },
  };
}
