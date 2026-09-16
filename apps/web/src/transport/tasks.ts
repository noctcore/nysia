import type { PaneKey } from '../generated/PaneKey';
import type { Issue, IssueState } from '../tasks/issue';

/**
 * Reading the two task answers off the wire, while the wire has no type for them.
 *
 * D-13 makes Rust the sole authority on a wire shape and forbids hand-writing a type Rust
 * already exports. Rust does not export these yet — wave C1 serves `tasks_list` and
 * `task_start`, and their `nysia-proto` types land with them — so this is the same
 * arrangement `./bridge.ts` already uses for `CommandFailure`: a mirror, named as one, in
 * the transport module and nowhere else.
 *
 * **What makes that safe rather than merely temporary is that it is checked.** A
 * hand-written `as Issue[]` would turn a disagreement between the two halves into
 * `undefined` in a table cell — a row with no title, a `● undefined` pill, an `updatedAt`
 * that reads `unknown` — which looks like a repository with strange issues in it rather than
 * like a protocol mistake. These parse instead: a row that is not the promised shape is a
 * refusal carrying the daemon's own verb, which the Tasks screen shows as a failed query
 * with a sentence that says what went wrong.
 *
 * When C1's types land, `apps/web/src/generated` gains them, `tasks/issue.ts` re-exports the
 * generated `Issue`, and these two functions are the only things that change — the screen
 * and the store do not know they were ever written by hand.
 */

/** What `task_start` answers with: the worktree, the session, and which of the two happened. */
export interface TaskStarted {
  /** The branch the worktree is keyed by, as the daemon opened it. */
  readonly branch: string;
  /** Runtime-scoped routing id for the session started in it. */
  readonly handle: string;
  /** The durable pane identity, which is what the window focuses. */
  readonly paneKey: PaneKey;
  /**
   * Whether a worktree that already existed was taken rather than a new one created.
   *
   * The flag wave C's contract requires: a worktree for that branch may have been made
   * outside Nysia or by an earlier `Start →`, the verb adopts it rather than failing, and the
   * user should be able to tell which happened.
   */
  readonly adopted: boolean;
}

/** A failure shaped like the rest of the transport's, so `describeFailure` can render it. */
function malformed(verb: string, problem: string): Error {
  return new Error(`${verb} answered with ${problem}`);
}

/**
 * The issue rows, or a refusal.
 *
 * Every field is checked because every field is rendered. `labels` is the one worth naming:
 * `gh` sends objects with a name, a description and a **hex colour**, and only the name
 * survives — a wire colour on screen is a pixel the theme switcher cannot reach, which is a
 * bug by this repository's own rule. Whether the daemon flattens them or sends the objects
 * is C1's to decide, so both spellings are read and neither is guessed at.
 */
export function readIssues(answer: unknown): readonly Issue[] {
  if (!Array.isArray(answer)) {
    throw malformed('tasks_list', 'something that is not a list of issues');
  }
  const issues = answer.map((row, index) => readIssue(row, index));

  // **Two rows with one number is refused, and this is load-bearing rather than tidy.** The
  // entire argument that a derived branch name is unique is that an issue number is unique
  // within a repository (`tasks/branchName.ts`); two rows sharing one would be two `Start →`
  // buttons asking for the same worktree, and the table would key two React rows alike. GitHub
  // cannot produce it, so meeting it means the answer is not what this module thinks it is —
  // which is exactly what this function exists to notice.
  const numbers = new Set(issues.map((issue) => issue.number));
  if (numbers.size !== issues.length) {
    throw malformed('tasks_list', 'two issues sharing one number');
  }
  return issues;
}

function readIssue(row: unknown, index: number): Issue {
  const fields = asRecord(row);
  if (fields === null) {
    throw malformed('tasks_list', `a row at position ${index} that is not an issue`);
  }
  const { number, title, state, updatedAt, url, author, labels } = fields;
  if (typeof number !== 'number' || !Number.isInteger(number)) {
    throw malformed('tasks_list', `a row at position ${index} with no issue number`);
  }
  if (typeof title !== 'string' || typeof updatedAt !== 'string' || typeof url !== 'string') {
    throw malformed('tasks_list', `an issue #${number} missing its title, date or URL`);
  }
  return {
    number,
    title,
    state: readState(state),
    updatedAt,
    url,
    // `null` rather than a refusal: GitHub really does answer with no author for an issue
    // whose account is gone, and a row that renders without a name is better than a list
    // that will not render at all.
    author: typeof author === 'string' && author !== '' ? author : null,
    labels: readLabels(labels),
  };
}

/** `gh` sends `OPEN` and `CLOSED`; anything else is read as closed rather than refused. */
function readState(state: unknown): IssueState {
  return typeof state === 'string' && state.toLowerCase() === 'open' ? 'open' : 'closed';
}

/**
 * Label names, from either spelling, with the colour dropped.
 *
 * A label that is neither a string nor an object with a name is skipped rather than refused:
 * a list is still perfectly readable without one pill, and refusing the whole query over a
 * label would be the tail wagging the dog.
 */
function readLabels(labels: unknown): readonly string[] {
  if (!Array.isArray(labels)) {
    return [];
  }
  // A `Set`, so a repeated name is one pill rather than two React children under one key.
  // Unlike a repeated issue number this is not worth refusing a list over — a duplicate label
  // says nothing about whether the rest of the answer is trustworthy.
  const names = new Set<string>();
  for (const label of labels) {
    if (typeof label === 'string') {
      names.add(label);
      continue;
    }
    const name = asRecord(label)?.name;
    if (typeof name === 'string') {
      names.add(name);
    }
  }
  return [...names];
}

/**
 * The `Start →` answer, or a refusal.
 *
 * `adopted` is required rather than defaulted to `false`. A default would make a daemon that
 * forgot the field say *"started in a new worktree"* about one it had adopted, which is
 * exactly the sentence the flag exists to get right — so a missing one is a protocol
 * disagreement and is reported as one.
 */
export function readStarted(answer: unknown): TaskStarted {
  const fields = asRecord(answer);
  if (fields === null) {
    throw malformed('task_start', 'something that is not a started session');
  }
  const { branch, handle, paneKey, adopted } = fields;
  if (typeof branch !== 'string' || branch === '') {
    throw malformed('task_start', 'no branch for the worktree it opened');
  }
  if (typeof handle !== 'string' || typeof paneKey !== 'string') {
    throw malformed('task_start', 'no session to open a tab for');
  }
  if (typeof adopted !== 'boolean') {
    throw malformed('task_start', 'no answer to whether it adopted a worktree or made one');
  }
  return { branch, handle, paneKey, adopted };
}

/**
 * An object with string keys, or `null`.
 *
 * `Object.hasOwn`-free and prototype-free reads: the values here come off the wire, and the
 * property names being looked up — `name`, `url`, `constructor` in a hostile payload — are
 * the same names `store/addProject.ts` uses a `Map` to stay away from. Reading a field from
 * a plain record cannot reach `Object.prototype` for any of the keys this module asks for,
 * because each is checked for a concrete type immediately afterwards and a prototype member
 * is a function.
 */
function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}
