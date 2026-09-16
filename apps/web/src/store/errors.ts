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
  | 'addProject'
  | 'dismissAddProject'
  // `refreshTasks` is absent on purpose, and it is the one omission here worth a line.
  // Every ending of a task query is an *answer* that belongs on the Tasks screen — including
  // the three refusals, which are the whole point of that screen — so it has no failure a
  // notice could name. `startTask` is the opposite: somebody pressed a button, and a worktree
  // that could not be created has to reach them wherever they are looking.
  | 'startTask'
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

/**
 * Whether a set of recorded failures can be rendered as a list.
 *
 * `CommandErrors` keys its notices on `StoreError.id` and dismisses by it, so duplicate
 * ids are not a tidiness question: React would warn on the keys, and one dismiss button
 * would clear every notice sharing that id. A provider can satisfy every other line of the
 * contract while reusing one id for every failure, which is why this is an invariant with
 * a name rather than an assumption inside one assertion.
 *
 * **The id must be a per-failure counter or a uuid, never a clock.** A millisecond
 * timestamp passes every check this can make and is still wrong: two failures on a real
 * round trip land in different milliseconds, so a provider using `Date.now()` goes green
 * here and collides the first time two rejections arrive in the same tick — a batched
 * frame, a reconnect flushing a queue, two commands failing on one dropped socket. Only a
 * provider fast enough to fail twice inside a millisecond, which is to say a mock, can be
 * caught from outside. That limit is the reason this paragraph exists rather than a test.
 */
export function hasDistinctIds(errors: readonly StoreError[]): boolean {
  return new Set(errors.map((error) => error.id)).size === errors.length;
}
