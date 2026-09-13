/**
 * What a failed store command looks like to the window.
 *
 * A command can fail for reasons that are entirely normal — the shell binary is not on
 * PATH, the daemon dropped the connection mid-request, the pane closed in another window
 * a moment ago. None of those are bugs, all of them have to reach the user, and none of
 * them may escape as an unhandled rejection.
 *
 * The rule the contract enforces is that a provider does **both**: it rejects the
 * command's promise with a `StoreCommandError` *and* has already appended the matching
 * entry to `snapshot.errors` by the time the promise settles. Rejecting without recording
 * loses the message; recording without rejecting leaves a caller unable to sequence.
 */

/** The store commands that can fail, named so an error can say which one did. */
export type StoreCommandName =
  | 'selectNav'
  | 'selectProject'
  | 'selectTab'
  | 'closeTab'
  | 'openTab'
  | 'dismissError'
  | 'window.minimize'
  | 'window.toggleMaximize'
  | 'window.close';

/**
 * One failure, as the snapshot carries it.
 *
 * `id` exists so the notice list can key on it and so dismissing is idempotent — two
 * failures of the same command a second apart are two notices, not one that flickers.
 */
export interface StoreError {
  readonly id: string;
  readonly command: StoreCommandName;
  /**
   * Shown to the user verbatim. The daemon's error envelope carries `nextSteps` (§ the
   * wire surface, W1), so this is where that lands — a sentence a person can act on, not
   * a status code.
   */
  readonly message: string;
  /** Epoch milliseconds, so a notice can age out without a second clock. */
  readonly at: number;
}

/**
 * The rejection every failing command produces.
 *
 * A class rather than a plain object because callers narrow with `instanceof`: a
 * `StoreCommandError` is an expected outcome that is already on screen, and anything else
 * coming out of a command is a bug that must not be swallowed.
 */
export class StoreCommandError extends Error {
  readonly command: StoreCommandName;
  /** The `id` of the `StoreError` this rejection corresponds to in `snapshot.errors`. */
  readonly errorId: string;

  constructor(command: StoreCommandName, message: string, errorId: string) {
    super(message);
    this.name = 'StoreCommandError';
    this.command = command;
    this.errorId = errorId;
  }
}
