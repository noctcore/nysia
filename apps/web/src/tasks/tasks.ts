import type { Issue } from './issue';

/**
 * Where the Tasks screen's query has got to, and how each ending is put to the user.
 *
 * The three failure states are the interesting part of this screen, not the happy path. The
 * v0.3 plan's §5 names the consequence of querying GitHub live (D-5) and wave C's contract
 * makes it a requirement: *"an empty list for any of those is a lie. A user with no issues
 * and a user whose token expired must not see the same screen."*
 *
 * So there are **four** endings that all draw a table with nothing in it, and every one of
 * them says something different:
 *
 * - `gh` is not installed at all;
 * - `gh` is installed and nobody is signed in;
 * - the query itself failed — offline, rate-limited, no such repository;
 * - the query worked and this repository has no open issues.
 *
 * The reading of an outcome lives here, apart from the rendering, and is asserted without a
 * DOM (D-18). What the component still owns is colour: every colour is a token and this
 * module names none — {@link TasksNotice.tone} is a name for the feeling, which the
 * component maps to a token, exactly as `store/addProject.ts` does for the register dialog.
 */

/**
 * Why there is no list, in a form the screen can branch on.
 *
 * The three the contract requires a user to be able to tell apart. `query_failed` is
 * deliberately the widest: offline, rate-limited and "no such repository" are one heading
 * with three different sentences underneath, because the daemon's own message is what
 * distinguishes them and a heading per network condition would be four words guessing at a
 * sentence that is already there.
 */
export type TaskUnavailableReason = 'gh_missing' | 'gh_unauthenticated' | 'query_failed';

/**
 * The wire codes this window has a distinct answer for.
 *
 * **A `Map`, not an object literal**, for `store/addProject.ts`'s reason and it is worth
 * repeating because it is not obvious: the key is a string off the wire, and an object
 * literal answers for every name on `Object.prototype` — so a daemon (or anything between)
 * sending `kind: "constructor"` would be read as a function rather than as a miss. A `Map`
 * holds only what was put in it.
 *
 * **These spellings are proposed rather than generated, and the fallback is what makes that
 * safe.** `nysia-proto`'s `ErrorCode` does not carry them yet — wave C1 serves `tasks_list`
 * and its codes land with it — so this cannot be derived from the generated union the way
 * `RegisterRefusalCode` is, and an `Extract<ErrorCode, …>` here would resolve to `never`
 * today. The names below are the contract's own words for the three states, which makes them
 * the least surprising thing for C1 to spell; and a code that does not match reads as
 * {@link TaskUnavailableReason} `query_failed`, carrying the daemon's own message and next
 * steps. That is a heading one notch less specific than it could be, not a lie — which is
 * the right way round for a guess to be wrong.
 *
 * When C1's codes land this becomes an `Extract<ErrorCode, …>` and a rename in proto fails
 * the typecheck here instead of silently falling through.
 */
const UNAVAILABLE = new Map<string, TaskUnavailableReason>([
  ['gh_missing', 'gh_missing'],
  ['gh_unauthenticated', 'gh_unauthenticated'],
  ['query_failed', 'query_failed'],
]);

/**
 * Which of the three a failed query was.
 *
 * Never `null`: unlike registering a folder, there is no ending here that belongs in the
 * notice list instead. Nobody is standing in a dialog, the screen is the thing that has
 * nothing to show, and a dropped socket that produced no answer is still a reason the table
 * is empty — so it is `query_failed` with whatever sentence came back, rather than a notice
 * in the corner and a blank table that looks like a repository with no work in it.
 */
export function unavailableReason(kind: string | null): TaskUnavailableReason {
  return (kind === null ? null : UNAVAILABLE.get(kind)) ?? 'query_failed';
}

/** Where the Tasks screen's query has got to. */
export type TasksState =
  /** Nothing has been asked for yet — the screen has not been opened on this connection. */
  | { readonly phase: 'idle' }
  /**
   * The daemon is being asked.
   *
   * Its own phase so the `↻` can be held shut while it runs: two refreshes in flight is two
   * answers racing, and the second would land on a snapshot the first has already replaced.
   */
  | { readonly phase: 'loading' }
  /**
   * The daemon answered with a list, which may be empty.
   *
   * An empty `issues` is a **fact about the repository**, not a failure, and it is the whole
   * reason this phase is separate from `unavailable`: collapsing the two is precisely the
   * lie the contract forbids.
   */
  | { readonly phase: 'loaded'; readonly issues: readonly Issue[] }
  /**
   * There is no list, and this is why.
   *
   * `message` and `nextSteps` are the daemon's own, kept apart rather than joined: the panel
   * puts the heading, the sentence and the steps in three different places, and the steps
   * are the part that says `gh auth login`. Neither ever names a path — the daemon's
   * envelope is built not to (traps register #13/#14) and this does not widen it.
   */
  | {
      readonly phase: 'unavailable';
      readonly reason: TaskUnavailableReason;
      readonly message: string;
      readonly nextSteps: readonly string[];
    };

/**
 * How an ending should read, for the panel that renders it.
 *
 * `tone` is a name for the feeling, not a colour. There are three, and the distinction that
 * matters is that `setup` is not `failed`: *"`gh` is not installed"* is a thing the user can
 * finish in a minute and be done with forever, while a failed query is something that went
 * wrong. Telling a user to install something in the voice of an error is how a first run
 * reads as a broken app.
 */
export interface TasksNotice {
  /** The line in bold. Distinct for every ending, which is the point of the module. */
  readonly heading: string;
  readonly tone: 'empty' | 'setup' | 'failed';
}

/**
 * What to show in place of a table, or `null` when there is a table to show.
 *
 * `loaded` with rows is the only state that answers `null`. Everything else — including a
 * repository that genuinely has no open issues — gets a heading of its own, because the four
 * empty screens must not be one screen.
 *
 * The headings say the same thing the daemon's message says, in the few words that fit in
 * bold above it. They do not replace it: the daemon's sentence and its next steps are
 * rendered underneath, verbatim, because those are the part that names the command to run.
 */
export function tasksNotice(state: TasksState): TasksNotice | null {
  switch (state.phase) {
    case 'idle':
    case 'loading':
      return null;
    case 'loaded':
      return state.issues.length === 0
        ? { heading: 'No open issues', tone: 'empty' }
        : null;
    case 'unavailable':
      return UNAVAILABLE_NOTICES[state.reason];
  }
}

/**
 * One heading per reason, which is the whole of "three different things".
 *
 * Both `gh` states are toned `setup` rather than `failed`. Neither is anything Nysia did
 * wrong and neither is anything the user did wrong; they are the two steps between a fresh
 * machine and a working screen, and the next steps underneath say which one.
 */
const UNAVAILABLE_NOTICES: Readonly<Record<TaskUnavailableReason, TasksNotice>> = {
  gh_missing: { heading: 'GitHub CLI is not installed', tone: 'setup' },
  gh_unauthenticated: { heading: 'GitHub CLI is not signed in', tone: 'setup' },
  query_failed: { heading: 'GitHub could not be reached', tone: 'failed' },
};

/** Whether the daemon is mid-answer, so the `↻` cannot start a second query. */
export function isTasksBusy(state: TasksState): boolean {
  return state.phase === 'loading';
}

/**
 * The rows to draw, which is empty for every state that is not a loaded list.
 *
 * Exists so the table component never has to narrow the union itself. A component that did
 * would be the fifth place the four endings are distinguished, and the fifth is the one that
 * gets a case wrong.
 */
export function issuesOf(state: TasksState): readonly Issue[] {
  return state.phase === 'loaded' ? state.issues : [];
}
