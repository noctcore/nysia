/**
 * One GitHub issue, as the Tasks screen draws it — which is now also how the wire spells it.
 *
 * **The shape is generated and this file re-exports it** (D-13). Rust is the sole authority
 * on a wire shape the moment it spells one, and as of v0.3 wave C1 it does: `nysia-proto`'s
 * `Issue` and `IssueState` land in `apps/web/src/generated` and are what everything from the
 * table to the branch derivation takes. The re-export is here rather than each caller
 * reaching into `generated/` so that `tasks/issue.ts` stays the one import for a row and its
 * two derivations, and so a reader arrives at this comment.
 *
 * # What used to be here, and why it is gone
 *
 * This declared its own `Issue` for a wave, and the argument was that the screen's shape and
 * `gh`'s answer were three decisions apart: `state` arrives from `gh` as `OPEN`, each label
 * carries a **hex colour** the theme switcher cannot reach, and `author` is an object rather
 * than a login. A generated type carrying those would have pushed all three into the table.
 *
 * **C1 made those three decisions in the daemon instead**, which is the better place for
 * them: `crates/nysia-core/src/rpc/tasks.rs` lowers the state into an enum, keeps only each
 * label's name, and reduces gh's author object to its login — so one conversion happens once
 * rather than in every client. What reaches the window is already the shape the screen
 * draws, the two halves cannot drift, and there is nothing left for a hand-written mirror to
 * be a mirror *of*.
 *
 * What survives the move is the *runtime* check, in `transport/tasks.ts`. A generated type is
 * a compile-time claim about a wire the daemon controls, not a runtime guarantee — a daemon
 * one version behind still answers — so a row that is not the promised shape stays a refusal
 * with a sentence rather than `undefined` in a table cell.
 *
 * # What this file still owns
 *
 * The two things the *screen* needs and the wire deliberately does not carry:
 *
 * - {@link repositoryOf}, because `gh issue list --json` refuses the field name `repository`
 *   outright, so the design's `owner · repo` sub-line has nothing behind it but the URL.
 * - {@link updatedPhrase}, because `7 days ago` is a rendering of a timestamp and the daemon
 *   sends ISO 8601 exactly as `gh` does.
 *
 * # What is deliberately absent from the wire
 *
 * **The body.** An issue body is someone else's text (traps register #13/#14) and nothing on
 * this screen renders it, so it is not requested, not carried and not available to be logged
 * by accident later. A field that is not on the wire cannot leak.
 *
 * **Anything derived and stored.** D-5: tasks are GitHub Issues queried live, with no local
 * task domain model — no table, no cache, no schema. A row is a row *in flight*; nothing
 * persists it and nothing is keyed by it. In particular nothing anywhere keys a worktree by
 * an issue number (D-6) — see `./branchName`, which is the one module that touches a number
 * at all and turns it into text inside a branch.
 *
 * # One rule for anyone writing a comment in this directory
 *
 * **No comment in `apps/web/src` writes an issue number out with its hash.**
 * `theme/colourGuard.ts` rule 1 reads whole files as text, and a hash followed by exactly 3,
 * 4, 6 or 8 hex digits is a colour literal to it — so an issue numbered 200, or 4021, fails
 * the colour gate when it appears in prose. It never appears in *code*, because the hash is a
 * template literal and the number comes off the wire.
 */

export type { Issue } from '../generated/Issue';
export type { IssueState } from '../generated/IssueState';

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
