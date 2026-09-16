import { GLYPH } from '../ui/glyphs';
import { isTasksBusy, tasksNotice, type TasksState } from './tasks';

/**
 * What the screen shows instead of a table.
 *
 * **The interesting part of this screen, not the happy path.** There are four ways to end up
 * with no rows and wave C's contract is explicit that they must not look alike: *"an empty
 * list for any of those is a lie. A user with no issues and a user whose token expired must
 * not see the same screen."* So `./tasks.ts` reads the ending and this renders it — the
 * heading it chose, then the daemon's own sentence, then the daemon's own next steps,
 * verbatim and each in its own place.
 *
 * Keeping the three apart is what makes the panel useful. Joining them into one sentence is
 * what `describeFailure` does for a *notice*, where there is one line to work with; here
 * there is a panel, and the steps are the part that says `gh auth login`.
 *
 * # Colour
 *
 * The tone comes off the notice as a *name* and is mapped to a token here. That split is the
 * reason `tasks.ts` can be tested without a DOM and the reason the accent picker reaches
 * every pixel of this panel: a module that returned a colour would be a module that had to
 * be edited when a theme was added.
 *
 * `setup` is deliberately not the failure colour. *"`gh` is not installed"* is a thing a
 * person finishes in a minute, and saying it in red is how a first run reads as a broken
 * app — so it takes the accent, which is what this window uses for *"something is waiting on
 * you"*. `empty` takes no colour at all, because a repository with no open issues is not an
 * event.
 */
const TONE_BORDER: Readonly<Record<'empty' | 'setup' | 'failed', string>> = {
  empty: 'border-line',
  setup: 'border-acc35',
  failed: 'border-status-failed',
};

export function TasksPanel({
  state,
  onRetry,
}: {
  readonly state: TasksState;
  readonly onRetry: () => void;
}) {
  const notice = tasksNotice(state);
  if (notice === null) {
    return null;
  }

  // Narrowed off the phase rather than off the tone: `unavailable` is the only phase
  // carrying a message, and reading one from a tone would be reading it from a rendering
  // decision.
  const detail = state.phase === 'unavailable' ? state : null;

  return (
    <div className="flex min-h-0 flex-1 items-start justify-center p-10">
      <div
        className={`bg-bg0 rounded-card w-[460px] max-w-full border p-6 ${TONE_BORDER[notice.tone]}`}
      >
        <div className="text-row font-semibold">{notice.heading}</div>
        {detail === null ? null : (
          <>
            {/*
             * The daemon's sentence, verbatim. `wrap-anywhere` for `CommandErrors`' reason:
             * a message can carry an unbroken run — a URL to install from, a host name —
             * and a box that only breaks between words runs it past the border.
             */}
            <p className="text-fg2 text-term mt-2 leading-normal wrap-anywhere">
              {detail.message}
            </p>
            <ul className="text-fg2 text-term mt-3 flex list-none flex-col gap-1.5 p-0 leading-normal">
              {/* Keyed by position, not by the text: the steps come off the wire, two can
                  legitimately read the same, and the list never reorders within a render. */}
              {detail.nextSteps.map((step, index) => (
                <li key={index} className="wrap-anywhere">
                  <span aria-hidden="true" className="text-fg3 mr-2">
                    {GLYPH.chevron}
                  </span>
                  {step}
                </li>
              ))}
            </ul>
          </>
        )}
        {/*
         * Offered for every ending except the one that is still running. A repository with no
         * open issues today has one tomorrow, and D-5 says the list is live — so "ask again"
         * is the honest affordance for the empty list too, rather than a reload of the
         * window. While a query is in flight there is nothing to ask again.
         */}
        {isTasksBusy(state) ? null : (
          <button
            type="button"
            onClick={onRetry}
            className="border-line2 bg-bg3 text-fg text-term rounded-control mt-4 cursor-pointer border px-3 py-1 focus-visible:shadow-focus focus-visible:outline-none"
          >
            Ask again
          </button>
        )}
      </div>
    </div>
  );
}
