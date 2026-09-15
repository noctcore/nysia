import type { AgentState } from '../generated/AgentState';
import type { AgentStatusChange } from '../generated/AgentStatusChange';
import type { PaneKey } from '../generated/PaneKey';
import { STATE_TONE, type StatusTone } from './agentStatus';

/**
 * The one place a status change becomes something that interrupts the user.
 *
 * ## Why this reads `notify` and nothing else
 *
 * `AgentStatusChange` carries a `notify` decision the daemon already made, and
 * `nysia-proto` says plainly why it is on the wire at all: the consumer that would forget
 * the rule is the window, so the decision travels with the change already made. Two rules
 * are folded into it — a session start raises nothing, and neither does a row the daemon
 * rehydrated from its disk spool — and both are invisible from the state alone. A consumer
 * that toasted on a finished agent would fire on every session start, every `/clear`, every
 * resume and every daemon restart, which is precisely the noise the suppression exists to
 * prevent.
 *
 * So this module never looks at the flags behind that decision. It switches on the arm.
 * `agentNotifications.source.test.ts` holds it to that by reading this file: the two field
 * names appear nowhere in it, which makes the guarantee greppable rather than a promise in
 * a comment.
 *
 * ## The window's own narrowing, on top
 *
 * Permitted means *the contract does not forbid it*, not *raise one*. What to do with a
 * permitted change is the window's decision, and the window narrows it twice:
 *
 *  - **Only the lead.** A subagent starting or finishing is not something to interrupt
 *    someone for; the lead is the thing they are waiting on. The roster still updates the
 *    pane's status — it just does not toast.
 *  - **Not `working`.** That arrives on every tool call. A toast per tool call is exactly
 *    the noise a status *dot* exists to replace, and the dot is already showing it.
 *
 * Both narrowings run *after* the `notify` arm, never instead of it. That order is the
 * whole point: a narrowing that ran first would be re-deriving the rule from the state.
 *
 * ## Why a sink rather than a field on `StoreSnapshot`
 *
 * The same reason `unexpectedFailures.ts` gives, plus one that is specific to this. A
 * snapshot is state and a notification is an *event* — the only thing carrying the
 * decision is the change, and folding a change stream into state and then asking "was that
 * new?" is the re-derivation this module exists to avoid. And a notification is a statement
 * about *this window*: two windows on one daemon each raise their own and dismiss them
 * independently, because under D-1/D-2 the daemon does not know what a window has shown.
 */

/** One notice, as the window shows it. */
export interface AgentNotification {
  readonly id: string;
  /** Which pane. */
  readonly pane: PaneKey;
  /**
   * What to call the session, captured when the notice was raised.
   *
   * Carried rather than looked up at render time, because a `PaneKey` is durable and
   * therefore **reusable** — `agentStatus.ts` and `MockStore` both treat reuse as real and
   * drop a row whose pane is gone. A notice that resolved its own title against the current
   * tab list would survive the session it describes and then be relabelled with whatever
   * took the key next: "codex · deskmate finished its turn" over a shell that just opened.
   *
   * A notice is a record of something that happened, so its label belongs to the moment it
   * happened. The provider supplies it because the provider is the one holding the tabs.
   */
  readonly session: string;
  /** The palette entry, so the notice and the dot agree without a second mapping. */
  readonly tone: StatusTone;
  /** The heading: `Needs input`, `Done`, `Interrupted`. */
  readonly title: string;
  /** One sentence saying what happened. */
  readonly message: string;
  /** Epoch milliseconds, for ordering and for the age the notice prints. */
  readonly at: number;
}

/**
 * How many notices are kept.
 *
 * Four, because they stack above a 30px status bar in a window that is mostly terminal, and
 * the fifth would be the first one pushed off the top of the screen — which is worse than
 * never having shown it, since the user cannot tell that it happened.
 */
export const NOTIFICATION_CAP = 4;

/** The heading and the sentence a notice carries. */
interface Wording {
  readonly title: string;
  readonly message: string;
}

/**
 * Which transitions deserve a notice, and what each one says.
 *
 * Every state is listed, and `working` is listed as `null` rather than left out. An
 * omission and a decision look identical in a partial table, and this is a table someone
 * will come back to; a `satisfies Record<AgentState, …>` also means a fifth state on the
 * wire fails the typecheck here instead of silently never notifying.
 */
export const NOTIFIABLE = {
  // Arrives on every tool call. A toast per tool call is the noise the dot replaces.
  working: null,
  waiting: { title: 'Needs input', message: 'is waiting for you.' },
  done: { title: 'Done', message: 'finished its turn.' },
  interrupted: { title: 'Interrupted', message: 'was interrupted.' },
} as const satisfies Record<AgentState, Wording | null>;

export interface AgentNotificationSink {
  /** Referentially stable between changes — this is read through `useSyncExternalStore`. */
  getSnapshot(): readonly AgentNotification[];
  subscribe(listener: () => void): () => void;
  /**
   * Offer a change, with what the pane's session is called right now.
   *
   * Whether it becomes a notice is this module's decision; what the notice is *called* is
   * not something this module can answer, because it holds no tabs. `session` is required
   * rather than optional so a provider cannot forget it and silently produce notices
   * labelled with pane keys — falling back to the key is the provider's decision to state,
   * for a pane no tab is showing.
   */
  report(change: AgentStatusChange, session: string): void;
  dismiss(id: string): void;
  /** Test-only: drop everything, so one case cannot leak into the next. */
  clear(): void;
}

export function createAgentNotificationSink(): AgentNotificationSink {
  let notices: readonly AgentNotification[] = [];
  let next = 1;
  const listeners = new Set<() => void>();

  function emit(updated: readonly AgentNotification[]): void {
    notices = updated;
    for (const listener of [...listeners]) {
      listener();
    }
  }

  return {
    getSnapshot: () => notices,

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },

    report(change, session) {
      // Step one, and it is the only step that can veto on the contract's behalf. Both arms
      // are named because the type is an enum rather than a boolean for exactly that
      // reason: a caller has to say which one it is in.
      switch (change.notify.decision) {
        case 'suppressed':
          return;
        case 'permitted':
          break;
      }

      // Step two: the window's own narrowing, and only ever after step one.
      switch (change.changed.target) {
        case 'subagent':
          return;
        case 'lead':
          break;
      }

      const row = change.status.lead;
      const wording = NOTIFIABLE[row.state];
      if (wording === null) {
        return;
      }

      const notice: AgentNotification = {
        id: `agent_${next}`,
        pane: row.pane,
        session,
        tone: STATE_TONE[row.state],
        title: wording.title,
        message: wording.message,
        at: row.observedAt,
      };
      next += 1;
      emit([...notices, notice].slice(-NOTIFICATION_CAP));
    },

    dismiss(id) {
      const remaining = notices.filter((notice) => notice.id !== id);
      if (remaining.length !== notices.length) {
        emit(remaining);
      }
    },

    clear() {
      if (notices.length > 0) {
        emit([]);
      }
    },
  };
}

/**
 * The window's sink.
 *
 * Module-level like `unexpectedFailures`, because there is one window and one stack of
 * notices in the corner of it. The factory exists so a test — and a second `MockStore` —
 * gets its own.
 */
export const agentNotifications = createAgentNotificationSink();
