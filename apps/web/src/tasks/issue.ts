/**
 * One GitHub issue, as the Tasks screen draws it.
 *
 * **Shaped from what `gh issue list --json` actually returns, not from what the design mock
 * draws.** The two differ in three places and each difference is a decision rather than an
 * omission — §4 of the design spec is the drawing, this is the data, and where they
 * disagree the data wins:
 *
 * 1. **There is no `repository` field.** `gh issue list --json` refuses the name outright
 *    (`Unknown JSON field: "repository"`), so the mock's `owner · repo` sub-line has nothing
 *    behind it. {@link repositoryOf} recovers it from the issue's own URL, which is the only
 *    place either half appears.
 * 2. **`state` arrives uppercase** — `OPEN`, `CLOSED` — and the pill is drawn `● Open`.
 * 3. **Labels carry a colour and it is deliberately dropped.** `gh` gives each label a hex,
 *    and the design draws label pills in `--bg3` with a `--line` border. Keeping GitHub's
 *    colour would put a hardcoded hex on screen that the theme switcher cannot reach, which
 *    is a bug by this repository's own rule — so only the name survives, and the pill is a
 *    token like every other pixel.
 *
 * # What is deliberately absent
 *
 * **The body.** An issue body is someone else's text (traps register #13/#14) and nothing on
 * this screen renders it, so it is not requested, not carried and not available to be logged
 * by accident later. A field that is not on the wire cannot leak.
 *
 * **Anything derived and stored.** D-5: tasks are GitHub Issues queried live, with no local
 * task domain model — no table, no cache, no schema. This interface is a *row in flight*;
 * nothing persists it and nothing is keyed by it. In particular nothing anywhere keys a
 * worktree by an issue number (D-6) — see `./branchName`, which is the one module that
 * touches a number at all and turns it into text inside a branch.
 *
 * # Why this is hand-written, and what happens when C1 lands
 *
 * D-13 makes Rust the sole authority on a wire shape and forbids hand-writing a type Rust
 * already exports. Rust does not export a task answer yet: wave C1 serves `tasks_list` and
 * its `nysia-proto` type lands with it. Until then `transport/tasks.ts` holds the runtime
 * check that the answer really has this shape, so a disagreement surfaces as a refusal
 * rather than as `undefined` in a table cell.
 *
 * **This file does not then become a re-export of the generated type**, and the three
 * differences at the top of this comment are the reason. C1 serves what `gh` gives it, so
 * the generated `Issue` will carry `OPEN` and label objects with a hex on each — re-exporting
 * it would push both into the table, which is the one outcome every decision here is for
 * avoiding. What the generated type replaces is the *wire* half: `transport/tasks.ts` starts
 * from it rather than from `unknown`, `tsc` takes over the work of noticing a missing field,
 * and the conversion those functions already do is what stays. This stays too, and keeps
 * saying what it says now — the shape the screen draws, which is nobody's wire format.
 */

/** Whether an issue is open, as the `● Open` pill reads it. `gh` sends these uppercase. */
export type IssueState = 'open' | 'closed';

/** One row of the table. */
export interface Issue {
  /**
   * The issue number, which the ID column prints after a hash.
   *
   * Unique within a repository, which is the whole of the argument that a derived branch
   * name is unique too — see `./branchName`.
   *
   * **No comment in `apps/web/src` writes one out with its hash**, and that is not a style
   * preference. `theme/colourGuard.ts` rule 1 reads whole files as text, and a hash followed
   * by exactly 3, 4, 6 or 8 hex digits is a colour literal to it — so an issue numbered 200,
   * or 4021, fails the colour gate when it appears in prose. It never appears in *code*,
   * because the hash is a template literal and the number comes off the wire.
   */
  readonly number: number;
  readonly title: string;
  readonly state: IssueState;
  /** ISO 8601, exactly as `gh` sends it. Formatted at the edge by {@link updatedPhrase}. */
  readonly updatedAt: string;
  /** The issue on github.com, and the only place the owner and repository appear. */
  readonly url: string;
  /** The login that opened it, or `null` where `gh` reported no author. */
  readonly author: string | null;
  /**
   * Label names, without their colours.
   *
   * See the module documentation: `gh` sends a hex per label and rendering it would put a
   * colour on screen that the accent picker cannot reach.
   */
  readonly labels: readonly string[];
}

/** An owner and repository, as the source chip row and each row's sub-line print them. */
export interface Repository {
  readonly owner: string;
  readonly name: string;
}

/**
 * The owner and repository an issue belongs to, recovered from its URL.
 *
 * The mock draws `owner · repo` on every row and `gh` will not answer the question, so this
 * is where that sub-line comes from. The URL is the one field carrying both halves, and its
 * shape — `<host>/<owner>/<repo>/issues/<number>` — is the same on github.com and on a
 * GitHub Enterprise host, which is why the host itself is not checked.
 *
 * `null` for anything that does not parse, and the caller renders the row without the
 * sub-line rather than guessing. Inventing an owner from a URL that did not have one would
 * put a person's name on somebody else's issue.
 */
export function repositoryOf(url: string): Repository | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  // Leading empty segment from the root slash, then owner, repo, `issues`, number.
  const [, owner, name, kind] = parsed.pathname.split('/');
  if (owner === undefined || name === undefined || owner === '' || name === '') {
    return null;
  }
  // `issues` is checked because a URL that is not an issue URL is a sign the answer is not
  // what this module thinks it is, and a wrong owner is worse than a missing one.
  return kind === 'issues' ? { owner, name } : null;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * The Updated column: `7 days ago`.
 *
 * Deliberately **not** `src/format.ts`'s `formatAge`, which prints `7d`. That form exists
 * because the sidebar's age column is right-aligned inside 222px and a second unit would
 * push a title's ellipsis around every minute; the Updated column is 110px of prose in a
 * table and the design spec writes it out in words. Two columns, two jobs, and neither is a
 * near-miss of the other.
 *
 * A timestamp that will not parse reads as `unknown`, not as `56 years ago`. `gh` sends ISO
 * 8601 and this should never fire, which is exactly why it must not fall through to an epoch
 * of zero and print a confident lie.
 */
export function updatedPhrase(now: number, updatedAt: string): string {
  const at = Date.parse(updatedAt);
  if (Number.isNaN(at)) {
    return 'unknown';
  }
  // A clock behind the server's reads as `just now` rather than as a time in the future,
  // which is the same stance `formatAge` takes on a daemon whose machine has just synced.
  const elapsed = Math.max(0, now - at);
  if (elapsed < MINUTE) {
    return 'just now';
  }
  if (elapsed < HOUR) {
    return plural(Math.floor(elapsed / MINUTE), 'minute');
  }
  if (elapsed < DAY) {
    return plural(Math.floor(elapsed / HOUR), 'hour');
  }
  return plural(Math.floor(elapsed / DAY), 'day');
}

function plural(count: number, unit: string): string {
  return `${count} ${unit}${count === 1 ? '' : 's'} ago`;
}
