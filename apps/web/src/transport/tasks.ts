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
 * **Every field is checked because every field is rendered, and the three that were merely
 * coerced are the reason that sentence is worth writing down.** `state` was the worst of
 * them: anything that was not the string `OPEN` read as `closed`, so a field C1 spelled
 * `status`, or sent as a number, painted an accent **Closed** pill on every row — directly
 * under a filter bar that says `is:issue is:open`, with nothing refused and nothing logged.
 * `author` as `gh`'s own object — `{id, is_bot, login, name}`, which is what `gh issue list
 * --json author` really returns — read as *"no author"*. A non-array `labels` read as no
 * labels, and so did an array of objects keyed `title` instead of `name`.
 *
 * All three now refuse, because a quiet wrong answer is the one thing this repository ranks
 * below a loud failure, and because a reader that hides a shape it cannot read is exactly
 * what the whole parse exists instead of. What a refusal costs is a screen saying the query
 * failed, with the daemon's verb in it; what a coercion cost was a table that looked right.
 *
 * `labels` is still the one worth naming for a second reason: `gh` sends objects with a
 * name, a description and a **hex colour**, and only the name survives — a wire colour on
 * screen is a pixel the theme switcher cannot reach, which is a bug by this repository's own
 * rule. Whether the daemon flattens them or sends the objects is C1's to decide, so both
 * spellings are read and neither is guessed at.
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
  // Positive, because GitHub numbers issues from 1. A negative one is not a number this
  // module can be handed by anything it is talking to, and `tasks/branchName.ts` would turn
  // it into `issue/-5-…` — a branch whose name begins with a flag.
  if (typeof number !== 'number' || !Number.isInteger(number) || number <= 0) {
    throw malformed('tasks_list', `a row at position ${index} with no issue number`);
  }
  if (typeof title !== 'string' || typeof updatedAt !== 'string' || typeof url !== 'string') {
    throw malformed('tasks_list', `an issue #${number} missing its title, date or URL`);
  }
  return {
    number,
    title,
    state: readState(state, number),
    updatedAt,
    url,
    author: readAuthor(author, number),
    labels: readLabels(labels, number),
  };
}

/**
 * `open` or `closed`, or a refusal. `gh` sends these uppercase.
 *
 * **Not a coercion with a default**, which is what this was and is the trap the whole module
 * is built against. Reading everything-that-is-not-`OPEN` as closed means a field C1 named
 * differently, or typed differently, produces a full table of rows whose pills all read
 * **Closed** under a filter bar saying `is:issue is:open` — every row wrong, nothing refused,
 * nothing logged, and no way to tell it from a repository whose issues really are all shut.
 */
function readState(state: unknown, issue: number): IssueState {
  if (typeof state === 'string') {
    const spelling = state.toLowerCase();
    if (spelling === 'open' || spelling === 'closed') {
      return spelling;
    }
  }
  throw malformed('tasks_list', `an issue #${issue} that is neither open nor closed`);
}

/**
 * The login, `null` where there genuinely is none, or a refusal.
 *
 * `null` is right for an *absent* author and only for that: GitHub really does answer with
 * no author for an issue whose account is gone, and a row that renders without a name is
 * better than a list that will not render at all.
 *
 * A value of some other shape is a different thing wearing the same answer, and it is
 * measured rather than hypothetical — `gh issue list --json author` returns
 * `{"id":…,"is_bot":false,"login":"…","name":""}`. C1 has been asked for the login string,
 * but this reader is the half that would have hidden the disagreement if it sent the object,
 * quietly reporting *"no author"* for every row in the table. It refuses a shape it cannot
 * read instead, and does not reach into the object to guess which key the login is under.
 */
function readAuthor(author: unknown, issue: number): string | null {
  if (author === null || author === undefined || author === '') {
    return null;
  }
  if (typeof author !== 'string') {
    throw malformed('tasks_list', `an issue #${issue} whose author is not a login`);
  }
  return author;
}

/**
 * Label names, from either spelling, with the colour dropped.
 *
 * **Refused rather than skipped**, which reverses what this did. The argument for skipping
 * was that a list is readable without one pill and that refusing a hundred issues over a
 * label is the tail wagging the dog — true of *one* malformed label among good ones, and
 * false of the case that actually happens, which is a whole answer keyed the other way.
 * `{nodes: […]}`, a comma-separated string, or objects carrying `title` instead of `name`
 * all produced the same empty array as an issue with no labels, on every row at once. That
 * is not a missing pill, it is the screen saying the repository does not label its work.
 */
function readLabels(labels: unknown, issue: number): readonly string[] {
  if (!Array.isArray(labels)) {
    throw malformed('tasks_list', `an issue #${issue} whose labels are not a list`);
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
    if (typeof name !== 'string') {
      throw malformed('tasks_list', `an issue #${issue} with a label it cannot name`);
    }
    names.add(name);
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
