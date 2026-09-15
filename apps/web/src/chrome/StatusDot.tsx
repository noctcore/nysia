import type { AgentStatus } from '../generated/AgentStatus';
import { agentDot } from '../store/agentStatus';

/**
 * The live status dot — one per agent session in the sidebar, one per agent tab in the
 * strip (design-spec.md §6.2, v0.2 delivery plan §2).
 *
 * The colour comes from `store/agentStatus.ts`, which holds the whole mapping from the
 * wire's `AgentState` to the design spec's §1 status palette. This component decides
 * nothing about it: it renders a class and a label, so there is exactly one place to read
 * if the question is "why is that dot amber".
 *
 * **Every colour is a token.** `--color-status-*` is defined once in `index.css` and the
 * runtime theme switcher never re-emits it (`theme/tokens.ts`), which is the mechanical
 * reason the accent picker cannot recolour status — accent is identity, status is
 * semantics, and the spec is explicit that the two must not be confused.
 *
 * `now` is a prop rather than a `useNow()` call inside. Staleness is a comparison against
 * the clock, and a sidebar of twelve rows each holding its own 30-second interval is twelve
 * timers where one will do — the surface that renders a list owns the tick.
 */
export function StatusDot({
  status,
  now,
  className = '',
}: {
  /** The pane's status, or `undefined` when the daemon has none for it. */
  readonly status: AgentStatus | undefined;
  /** Epoch milliseconds, from the caller's clock. */
  readonly now: number;
  readonly className?: string;
}) {
  const dot = agentDot(status?.lead, now);
  return (
    <span
      // Labelled rather than hidden: the dot is the only place the state appears, so a
      // reader that skipped it would be told a session exists and not what it is doing.
      role="img"
      aria-label={dot.label}
      title={dot.label}
      data-status={dot.tone ?? 'unknown'}
      className={`size-1.5 flex-none rounded-full ${dot.className} ${className}`}
    />
  );
}
