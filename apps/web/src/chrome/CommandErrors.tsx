import type { StoreError } from '../store/errors';
import { useCommands, useSnapshot, useUnexpectedFailures } from '../store/hooks';
import { unexpectedFailures } from '../store/unexpectedFailures';
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

  if (errors.length === 0 && escaped.length === 0) {
    return null;
  }

  return (
    <div
      role="log"
      aria-live="polite"
      aria-label="Command failures"
      className="pointer-events-none absolute bottom-statusbar left-3.5 z-30 flex w-[380px] flex-col gap-2 pb-2"
    >
      {errors.map((error) => (
        <Notice
          key={error.id}
          error={error}
          onDismiss={() => commands.dismissError(error.id)}
        />
      ))}
      {escaped.map((error) => (
        <Notice
          key={error.id}
          error={error}
          onDismiss={() => unexpectedFailures.dismiss(error.id)}
        />
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
      <div className="flex-1">
        <div className="text-fg font-semibold">{error.command} failed</div>
        <div className="text-fg2 mt-1 leading-normal">{error.message}</div>
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
