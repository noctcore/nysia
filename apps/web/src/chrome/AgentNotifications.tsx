import type { AgentNotification } from '../store/agentNotifications';
import { TONE_CLASS } from '../store/agentStatus';
import { useAgentNotifications } from '../store/hooks';
import { GLYPH } from '../ui/glyphs';

/**
 * Where a status change that deserves attention lands.
 *
 * The third surface the v0.2 plan asks for, after the sidebar dot and the tab dot: a dot
 * tells you something has changed only if you are looking at it, and the whole point of
 * hook-driven status is that you are looking at the terminal instead.
 *
 * **Which changes get here is not this component's decision.** `store/agentNotifications.ts`
 * reads the `notify` arm the daemon put on the change and then applies the window's own
 * narrowing; by the time a notice reaches this list the question is settled. That split is
 * deliberate — a component that decided would be a component that had to remember the rule.
 *
 * Anchored bottom-**right**, mirroring `CommandErrors` on the left. They are different
 * kinds of message — one is "what you asked for did not happen", the other is "something
 * you were waiting on moved" — and an agent finishing while a shell fails to launch should
 * not push either off the screen.
 *
 * They do not time out, for `CommandErrors`' reason: `Needs input` is a notice the user has
 * to act on, and a toast that removes itself while someone is reading it is worse than one
 * they dismiss. The list is capped instead, in the sink, so a burst cannot climb past the
 * titlebar.
 */
export function AgentNotifications() {
  const { notices, dismiss } = useAgentNotifications();

  if (notices.length === 0) {
    return null;
  }

  return (
    <div
      role="log"
      aria-live="polite"
      aria-label="Agent status"
      // 320px is the target, not a promise: the column is absolutely positioned against one
      // edge, so a fixed width runs off a narrow window. Same cap as `CommandErrors` (#71).
      className="pointer-events-none absolute right-3.5 bottom-statusbar z-30 flex w-[320px] max-w-[calc(100vw-1.75rem)] flex-col gap-2 pb-2"
    >
      {notices.map((notice) => (
        <Notice key={notice.id} notice={notice} onDismiss={() => dismiss(notice.id)} />
      ))}
    </div>
  );
}

function Notice({
  notice,
  onDismiss,
}: {
  readonly notice: AgentNotification;
  readonly onDismiss: () => void;
}) {
  const { session } = notice;
  return (
    <div className="border-line2 bg-bg2 text-term pointer-events-auto flex items-start gap-3 rounded-panel border p-3 shadow-flyout">
      {/* The same palette entry the pane's dot is showing, from the same table, so the
          notice and the dot cannot disagree about what happened. */}
      <span
        aria-hidden="true"
        className={`mt-1.5 size-1.5 flex-none rounded-full ${TONE_CLASS[notice.tone]}`}
      />
      <div className="min-w-0 flex-1">
        <div className="text-fg font-semibold">{notice.title}</div>
        {/* A session title is whatever the daemon calls the pane, which for a shell is
            routinely a path with no spaces — the same unbroken run that ran past the border
            in `CommandErrors` (#71), so it gets the same `overflow-wrap: anywhere`. */}
        <div className="text-fg2 mt-1 leading-normal wrap-anywhere">
          <span className="text-fg">{session}</span> {notice.message}
        </div>
      </div>
      <button
        type="button"
        aria-label={`Dismiss ${notice.title} for ${session}`}
        onClick={onDismiss}
        className="text-fg3 hover:text-fg cursor-pointer border-0 bg-transparent p-0 leading-none focus-visible:shadow-focus focus-visible:outline-none"
      >
        {GLYPH.close}
      </button>
    </div>
  );
}
