/**
 * Turning an issue into the branch its worktree is keyed by.
 *
 * **This module exists because the wire cannot express a task id.** D-6 says worktrees are
 * keyed by branch and never by a task id, and wave C made that unrepresentable rather than
 * merely forbidden: the `Start →` request carries a project and a branch, so the daemon
 * never sees the issue at all. Deriving a name is therefore the window's job, done *before*
 * the request exists — which is also why it is a pure function in `apps/web` rather than
 * anything the daemon could be asked to do.
 *
 * Three shipped bugs in the system Nysia replaces came from task-keying, and this is the one
 * file where the temptation comes back: the issue number is right there and makes a tidy
 * directory name. What stops it is not restraint, it is that the number leaves this module
 * as *text inside a branch name* and the branch is the key from then on. A worktree that
 * outlives its issue, or two issues that end up on one branch, are both ordinary states
 * afterwards, and nothing downstream can recover an issue number from a branch or would be
 * right to try.
 *
 * # What is chosen, and what follows from it
 *
 * `issue/<number>-<slug>`, where the slug is the title reduced to what is certainly legal in
 * a ref. The shape answers the two questions the contract asks of it:
 *
 * **Two issues never collide**, because an issue number is unique within a repository and
 * the number is in the name. Two issues whose titles slug identically still differ by their
 * number, so there is no case where deduplication or a counter is needed — see
 * `branchName.test.ts`, which states that as a property rather than an example.
 *
 * **A title that is not a legal ref cannot produce an illegal branch**, because
 * {@link slugify} is a whitelist. See its documentation for why that is the load-bearing
 * choice rather than a stylistic one.
 *
 * # The cost, stated rather than left to be found
 *
 * **Renaming an issue derives a different branch.** Start it, retitle it, start it again and
 * the second `Start →` asks for a branch the first one did not create — so instead of
 * adopting the existing worktree it makes a second one. That is a real wrinkle and it is the
 * price of a readable name; a branch of `issue/200` alone would be stable under a retitle
 * and would tell you nothing when six of them are open in the sidebar.
 *
 * It is survivable rather than merely accepted. Nothing is lost when it happens — both
 * worktrees are real branches with real work on them, which is the state git is built for —
 * and D-6 is the reason it is not a bug: the *branch* is the identity, so two branches are
 * two things by definition, and it is only a surprise if you were still thinking in tasks.
 * The fix, if it ever bites, is for `Start →` to adopt a worktree whose branch begins
 * `issue/<number>-`; that is a decision for the side that enumerates worktrees, which is the
 * daemon's, and it would not change this module's output.
 */

/**
 * How much of the title survives into the branch.
 *
 * Long enough to tell six open worktrees apart, short enough that the worktree directory
 * underneath it is nowhere near a Windows path limit. Applied to the slug alone: the
 * `issue/<number>-` part is never truncated, because it is the part that has to stay unique.
 */
const MAX_SLUG_LENGTH = 48;

/** Everything before the number. A single component, so `issue/200-x` is one level deep. */
const PREFIX = 'issue/';

/**
 * The branch `Start →` should ask for, for an issue.
 *
 * Total, and deterministic: the same number and title always give the same branch, which is
 * what makes a second `Start →` on an untouched issue *adopt* the worktree the first one
 * created rather than fail beside it.
 */
export function branchForIssue(issue: { readonly number: number; readonly title: string }): string {
  const slug = slugify(issue.title);
  // An empty slug is not a failure and gets no placeholder. A title in a script this
  // whitelist does not cover — which is most of them — reduces to nothing, and
  // `issue/200` is then the honest name: it is still unique, still legal, and still says
  // which issue it came from. Appending a `-` or an `untitled` would add a character that
  // means nothing.
  return slug === '' ? `${PREFIX}${issue.number}` : `${PREFIX}${issue.number}-${slug}`;
}

/**
 * An issue title reduced to characters that are certainly legal in a ref.
 *
 * **A whitelist, and that is the whole design.** `git check-ref-format` forbids a list —
 * control characters, a space, `~^:?*[\`, `..`, `@{`, a `.lock` suffix, a leading `.`, a
 * trailing `.` or `/`, a bare `@` — and a blacklist implementing it would be a second copy
 * of git's rules that has to be right about every one of them, forever, including the ones
 * added after this was written. Keeping only `[a-z0-9]` and joining with `-` cannot emit any
 * forbidden sequence, so the rules do not have to be enumerated to be satisfied: there is no
 * `.`, so there is no `..`, no `.lock` and no leading dot; no `@`, so no `@{`; no `/`, so no
 * trailing slash and no empty component.
 *
 * The leading and trailing `-` go for a reason `check-ref-format` does not cover: a ref
 * beginning with `-` is legal and is still a branch every command-line tool reads as an
 * option, which is the same class of mistake `git/command.rs` keeps paths out of arguments
 * to avoid.
 *
 * Non-ASCII is dropped rather than transliterated. Git accepts UTF-8 in a ref, so this is
 * stricter than it has to be, and deliberately: a transliteration table is a large thing to
 * be subtly wrong about per language, and the failure mode here — an issue titled in
 * Japanese branching as `issue/200` — is a name that is plain rather than a name that is
 * wrong.
 */
export function slugify(title: string): string {
  const kept = [...title.toLowerCase()]
    .map((character) => (/[a-z0-9]/.test(character) ? character : '-'))
    .join('');
  // Collapse first, then cut, then trim again: cutting first can leave a `-` at the end,
  // and trimming before the cut would not have seen it.
  return trimDashes(trimDashes(collapse(kept)).slice(0, MAX_SLUG_LENGTH));
}

/** Runs of separators become one. */
function collapse(text: string): string {
  return text.replace(/-+/g, '-');
}

/** A branch may not usefully begin or end with the separator. */
function trimDashes(text: string): string {
  return text.replace(/^-+/, '').replace(/-+$/, '');
}
