import type { Issue } from '../generated/Issue';
import type { IssueState } from '../generated/IssueState';
import type { ProjectStarted } from '../generated/ProjectStarted';

/**
 * Reading the two task answers off the wire, now that the wire has a type for both.
 *
 * **The mirror is gone.** This module held a hand-written `TaskStarted` and read every issue
 * field out of `unknown`, on the stated understanding that it was a stand-in until
 * `nysia-proto` spelled the shapes — the same arrangement `./bridge.ts` still uses for
 * `CommandFailure`, and named as one. v0.3 wave C1 spelled them, so D-13 applies and the
 * mirror gives way: {@link readIssues} and {@link readStarted} now answer with the generated
 * `Issue` and `ProjectStarted` themselves, and there is no second declaration of either
 * shape anywhere in `apps/web`.
 *
 * # Why there is still a parse
 *
 * **A generated type is a compile-time claim about a wire the daemon controls, not a runtime
 * guarantee.** `tsc` checks that this build agrees with the `nysia-proto` it was generated
 * from; it cannot check that the daemon on the other end of the socket is that build. A
 * daemon one version behind still answers, and D-1 is the whole reason it can be — the
 * runtime outlives the window, so a freshly upgraded window talking to a daemon that has
 * been up for days is the ordinary case rather than the exotic one.
 *
 * So the check stays, and it is the same argument `bridge.ts` makes for checking
 * `CommandFailure` at runtime despite having a type for it. What a cast would buy is one
 * fewer function; what it would cost is that a disagreement between the two halves renders
 * as `undefined` in a table cell — a row with no title, a `● undefined` pill, an `updatedAt`
 * that reads `unknown` — which looks like a repository with strange issues in it rather than
 * like a protocol mistake. These refuse instead, carrying the daemon's own verb, which the
 * Tasks screen shows as a failed query with a sentence that says what went wrong.
 *
 * # Nothing here is lenient about a spelling the wire has settled
 *
 * Three readings used to accept more than one shape, because which shape C1 would send was
 * genuinely undecided: `state` was lowercased so `OPEN` would pass, a label could be a string
 * *or* an object keyed `name`, and an author could be absent or empty as well as `null`.
 *
 * C1 decided all three, in the daemon, and `crates/nysia-core/src/rpc/tasks.rs` is where:
 * the state becomes an enum that serialises lowercase, labels are flattened to their names
 * with the empty ones dropped, and an author is a login or `null` and never `""`. **A
 * tolerance for a shape no daemon sends is not harmless** — it is a reader that would accept
 * a daemon which had quietly stopped converting, and go on drawing a table while the two
 * halves disagreed. There is no daemon on the other side of this that ever served these
 * verbs with `gh`'s own spellings: a daemon older than C1 does not serve them at all, it
 * answers `unsupported`. So each of the three now refuses, which turns a regression in the
 * converter into a sentence on screen instead of a silence.
 */

/** A failure shaped like the rest of the transport's, so `describeFailure` can render it. */
function malformed(verb: string, problem: string): Error {
  return new Error(`${verb} answered with ${problem}`);
}

/**
 * The issue rows, or a refusal.
 *
 * **Every field is checked because every field is rendered**, and the three that were merely
 * coerced are the reason that is worth writing down. `state` was the worst of them: anything
 * that was not the string `OPEN` read as `closed`, so a field spelled `status`, or sent as a
 * number, painted an accent **Closed** pill on every row — directly under a filter bar that
 * says `is:issue is:open`, with nothing refused and nothing logged. `author` as `gh`'s own
 * object — `{id, is_bot, login, name}`, which is what `gh issue list --json author` really
 * returns — read as *"no author"*. A non-array `labels` read as no labels, and so did an
 * array of objects keyed `title` instead of `name`.
 *
 * All three refuse now, because a quiet wrong answer is the one thing this repository ranks
 * below a loud failure, and because a reader that hides a shape it cannot read is exactly
 * what the whole parse exists instead of. What a refusal costs is a screen saying the query
 * failed, with the daemon's verb in it; what a coercion cost was a table that looked right.
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
 * `open` or `closed`, or a refusal.
 *
 * **The daemon's spelling exactly, with no case folding.** `IssueState` is a Rust enum that
 * serialises lowercase, and the conversion from `gh`'s `OPEN` happens in the daemon — so the
 * two spellings this used to accept are now one spelling the daemon promises and one that
 * says the converter is not running. Folding the case would let the second through silently,
 * which is the shape of every defect this module exists to notice.
 */
function readState(state: unknown, issue: number): IssueState {
  if (state === 'open' || state === 'closed') {
    return state;
  }
  throw malformed('tasks_list', `an issue #${issue} that is neither open nor closed`);
}

/**
 * The login, or `null` where there genuinely is none, or a refusal.
 *
 * `null` is right for an *absent* author and only for that: GitHub really does answer with
 * no author for an issue whose account is gone, and a row that renders without a name is
 * better than a list that will not render at all. The daemon settles that case for the
 * window — an author object with an empty login becomes `null` rather than `""` — so an
 * empty string arriving here is a converter that has stopped converting, not an issue
 * nobody owns, and a missing field is a daemon that does not send the field at all.
 *
 * Neither is coerced. The shape that would have hidden the disagreement is measured rather
 * than hypothetical: `gh issue list --json author` returns
 * `{"id":…,"is_bot":false,"login":"…","name":""}`, and reading a non-string as *"no author"*
 * reported every row in the table as authorless while looking perfectly well.
 */
function readAuthor(author: unknown, issue: number): string | null {
  if (author === null) {
    return null;
  }
  if (typeof author !== 'string' || author === '') {
    throw malformed('tasks_list', `an issue #${issue} whose author is not a login`);
  }
  return author;
}

/**
 * Label names, or a refusal.
 *
 * **Refused rather than skipped**, which reverses what this did. The argument for skipping
 * was that a list is readable without one pill and that refusing a hundred issues over a
 * label is the tail wagging the dog — true of *one* malformed label among good ones, and
 * false of the case that actually happens, which is a whole answer keyed the other way.
 * `{nodes: […]}`, a comma-separated string, or objects carrying a name under some other key
 * all produced the same empty array as an issue with no labels, on every row at once. That
 * is not a missing pill, it is the screen saying the repository does not label its work.
 *
 * **Names only, and the object spelling is no longer read.** `gh` sends objects with a name,
 * a description and a hex colour; the daemon keeps the name and drops the rest, because a
 * wire colour on screen is a pixel the theme switcher cannot reach and that is a bug by this
 * repository's own rule. Reading the object here as well would be a second implementation of
 * a conversion that has one home.
 */
function readLabels(labels: unknown, issue: number): string[] {
  if (!Array.isArray(labels)) {
    throw malformed('tasks_list', `an issue #${issue} whose labels are not a list`);
  }
  // A `Set`, so a repeated name is one pill rather than two React children under one key.
  // Unlike a repeated issue number this is not worth refusing a list over — a duplicate label
  // says nothing about whether the rest of the answer is trustworthy.
  const names = new Set<string>();
  for (const label of labels) {
    if (typeof label !== 'string' || label === '') {
      throw malformed('tasks_list', `an issue #${issue} with a label it cannot name`);
    }
    names.add(label);
  }
  return [...names];
}

/**
 * The `Start →` answer, or a refusal.
 *
 * `adopted` is required rather than defaulted to `false`. A default would make a daemon that
 * forgot the field say *"started in a new worktree"* about one it had adopted, which is
 * exactly the sentence the flag exists to get right — the generated type's own comment is
 * that the dialog needs it to say *"opened the worktree you already had"* rather than
 * implying it made one. So a missing one is a protocol disagreement and is reported as one.
 */
export function readStarted(answer: unknown): ProjectStarted {
  const fields = asRecord(answer);
  if (fields === null) {
    throw malformed('project_start', 'something that is not a started session');
  }
  const { branch, adopted, handle, paneKey } = fields;
  if (typeof branch !== 'string' || branch === '') {
    throw malformed('project_start', 'no branch for the worktree it opened');
  }
  if (typeof handle !== 'string' || typeof paneKey !== 'string') {
    throw malformed('project_start', 'no session to open a tab for');
  }
  if (typeof adopted !== 'boolean') {
    throw malformed('project_start', 'no answer to whether it adopted a worktree or made one');
  }
  return { branch, adopted, handle, paneKey };
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
