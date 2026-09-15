import type { StoreError } from '../store/errors';
import { useCommands, useSnapshot, useUnexpectedFailures } from '../store/hooks';
import { GLYPH } from '../ui/glyphs';

/**
 * Where a failed command lands.
 *
 * Without this the store's error list is a tree falling in an empty forest: the provider
 * records that the shell binary was missing, and the user sees a menu close. The notices
 * sit just above the status bar, anchored to the left like the usage popover, so the one
 * corner of the window that talks about the daemon is the corner that reports it failing.
 *
 * They do not time out. A launch that failed is something the user has to act on — the
 * message carries the daemon's `nextSteps` — and a notice that removes itself while
 * someone is reading it is worse than one they have to dismiss.
 *
 * That argument is about *time*, not about *size*, and the two were briefly conflated: a
 * notice that stays until dismissed is right, and a notice that grows without limit while it
 * stays is not. `nextSteps` is as long as the daemon needs it to be, so the message body is
 * capped and scrolls. The notice keeps its place in the corner instead of climbing over the
 * sidebar, and nothing in it is lost.
 *
 * Two lists are merged here, from two places, for one reason. `snapshot.errors` is what a
 * provider wrapped and recorded. `useUnexpectedFailures()` is what got out unwrapped — a
 * raw transport error, a provider bug — which the provider by definition did not record,
 * and which `errors.ts` nonetheless promises will reach the user. They render identically
 * because to the person reading them they are the same event: something they asked for did
 * not happen.
 */
export function CommandErrors() {
  const { errors } = useSnapshot();
  const escaped = useUnexpectedFailures();
  const commands = useCommands();

  if (errors.length === 0 && escaped.failures.length === 0) {
    return null;
  }

  return (
    <div
      role="log"
      aria-live="polite"
      aria-label="Command failures"
      // 380px is the target, not a promise. It sits `left-3.5` in an absolutely positioned
      // column, so a fixed width reaches the right edge of a narrow window and then past it;
      // the cap keeps the same gutter on both sides at any size.
      className="pointer-events-none absolute bottom-statusbar left-3.5 z-30 flex w-[380px] max-w-[calc(100vw-1.75rem)] flex-col gap-2 pb-2"
    >
      {errors.map((error) => (
        <Notice
          key={error.id}
          error={error}
          onDismiss={() => commands.dismissError(error.id)}
        />
      ))}
      {escaped.failures.map((error) => (
        <Notice key={error.id} error={error} onDismiss={() => escaped.dismiss(error.id)} />
      ))}
    </div>
  );
}

function Notice({
  error,
  onDismiss,
}: {
  readonly error: StoreError;
  readonly onDismiss: () => void;
}) {
  return (
    <div className="border-status-failed bg-bg2 text-term pointer-events-auto flex items-start gap-3 rounded-panel border p-3 shadow-flyout">
      <span aria-hidden="true" className="text-status-failed leading-none">
        {GLYPH.dot}
      </span>
      {/* `min-w-0` because a flex item's default `min-width: auto` is its min-content
          width, and an unbroken path has a large one — without this the item refuses to
          shrink and pushes the dismiss button out of the box no matter how the text wraps. */}
      <div className="min-w-0 flex-1">
        {/* The heading is words, so it wraps on them. */}
        <div className="text-fg font-semibold">{error.command} failed</div>
        {/*
         * The message is not words. The daemon's `nextSteps` carries an absolute path, and
         * on Windows that is `C:\Users\…\target\debug\nysia.exe` — no spaces, so no break
         * opportunities, so a box that only knows how to break between words neither wraps
         * it nor clips it and the text runs past the border (#71). The notice that reports a
         * *missing runtime* is guaranteed to carry one, so this is the common case here.
         *
         * `wrap-anywhere` is `overflow-wrap: anywhere` rather than `break-word`, and the
         * difference is the one that matters inside a flex item: `anywhere` counts the break
         * opportunities when the min-content width is computed, so the item can actually
         * shrink. `break-words` leaves min-content at the full length of the path, which is
         * why it is "often not enough" for exactly this input.
         *
         * `break-all` was the other candidate and is worse: it breaks mid-word in ordinary
         * prose too, and most of these messages are sentences.
         */}
        <div className="text-fg2 mt-1 max-h-40 overflow-y-auto leading-normal wrap-anywhere">
          {error.message}
        </div>
      </div>
      <button
        type="button"
        aria-label={`Dismiss ${error.command} failure`}
        onClick={onDismiss}
        className="text-fg3 hover:text-fg cursor-pointer border-0 bg-transparent p-0 leading-none focus-visible:shadow-focus focus-visible:outline-none"
      >
        {GLYPH.close}
      </button>
    </div>
  );
}
