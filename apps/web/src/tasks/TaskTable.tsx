import { GLYPH } from '../ui/glyphs';
import { TASK_GLYPH } from './glyphs';
import { repositoryOf, updatedPhrase, type Issue } from './issue';
import { isStarting, type TaskStartState } from './tasks';

/*
 * The issue table (design-spec.md §4).
 *
 * `80px | 1fr | 90px | 110px | 120px` — ID, Title/context, Status, Updated, actions — with
 * the header outside the card and the rows inside it. The widths are written once, in
 * {@link COLUMNS}, because a header and a body that each carried their own copy is a table
 * whose columns drift apart the first time one of them is edited.
 *
 * It is a grid rather than a `<table>`, which the design's fixed track list is what it is
 * for, so the semantics are carried by roles instead of by elements. A screen reader gets a
 * table either way; a `<table>` would not give the 1fr column that makes the title ellipsis
 * work.
 *
 * # What is drawn and what is disabled
 *
 * Every affordance the design draws that has nothing behind it yet is rendered **disabled
 * with a title naming when**, which is the standard this chrome already holds itself to —
 * `App.render.test.ts` asserts it of the rail and the sidebar's `+`, and *"ships no
 * affordance that looks live and does nothing"* is the name of that test. The alternative,
 * quietly dropping them, makes the screen stop looking like the design for a reason nobody
 * reading it can see.
 */

/**
 * The five tracks, and the gap between them.
 *
 * Exported so the render test can assert the header and the rows share them rather than
 * matching a string twice.
 */
export const COLUMNS = 'grid grid-cols-[80px_1fr_90px_110px_120px] gap-3';

export function TaskTable({
  issues,
  taskStart,
  now,
  onStart,
}: {
  readonly issues: readonly Issue[];
  readonly taskStart: TaskStartState;
  /** Passed in rather than read here, so every row's `7 days ago` is from one clock. */
  readonly now: number;
  readonly onStart: (issue: Issue) => void;
}) {
  return (
    <div role="table" aria-label="GitHub issues" className="flex min-h-0 flex-col">
      <div
        role="row"
        className={`${COLUMNS} text-fg3 text-label tracking-group px-3.5 pt-1.5 font-semibold uppercase`}
      >
        <span role="columnheader">ID</span>
        <span role="columnheader">Title / context</span>
        <span role="columnheader">Status</span>
        <span role="columnheader">Updated</span>
        {/* The actions column has no heading in the design, and an invented one would be
            read out on every row. `presentation` is what an empty header cell is. */}
        <span role="presentation" />
      </div>
      <div
        role="rowgroup"
        className="border-line bg-bg0 mt-1.5 min-h-0 overflow-y-auto rounded-panel border"
      >
        {issues.map((issue) => (
          <TaskRow
            key={issue.number}
            issue={issue}
            now={now}
            busy={isStarting(taskStart, issue.number)}
            // **Every** row is held shut while any start is in flight, not just the one that
            // is spinning. The store refuses a second start outright — one worktree at a
            // time — and it refuses it by *resolving*, so a row left pressable would swallow
            // a click and show nothing at all for it. Disabling is what makes that refusal
            // visible instead of silent.
            blocked={taskStart.phase === 'starting'}
            onStart={() => onStart(issue)}
          />
        ))}
      </div>
    </div>
  );
}

function TaskRow({
  issue,
  now,
  busy,
  blocked,
  onStart,
}: {
  readonly issue: Issue;
  readonly now: number;
  /** This issue is the one being started, so its button says so. */
  readonly busy: boolean;
  /** Some *other* issue is being started, so this button is shut but says nothing. */
  readonly blocked: boolean;
  readonly onStart: () => void;
}) {
  const repository = repositoryOf(issue.url);

  return (
    <div
      role="row"
      // `last:border-b-0` rather than a border on every row but the last computed here: the
      // card clips to its own radius, so a trailing rule would sit on the curve.
      className={`${COLUMNS} border-line hover:bg-bg2 items-center border-b px-3.5 py-2.5 last:border-b-0`}
    >
      <span role="cell" className="text-fg2 font-mono text-term">
        {GLYPH.tasks} {`#${issue.number}`}
      </span>
      {/* `min-w-0` so the 1fr track may shrink below its content: without it the title's
          min-content width holds the column open and the ellipsis never appears. */}
      <div role="cell" className="min-w-0">
        <div className="truncate font-medium">{issue.title}</div>
        <div className="text-fg2 text-chip mt-1 flex items-center gap-1.5">
          {issue.author === null ? null : <span className="truncate">{issue.author}</span>}
          {repository === null ? null : <span className="truncate">{repository.name}</span>}
          {issue.labels.map((label) => (
            // The design's pill: `--bg3`, a `--line` border, radius 99. `gh` sends a hex per
            // label and the daemon drops it before the window sees one — see `./issue.ts`.
            <span
              key={label}
              className="border-line bg-bg3 rounded-pill truncate border px-2 py-px"
            >
              {label}
            </span>
          ))}
        </div>
      </div>
      <span
        role="cell"
        className="border-acc35 text-acc text-chip rounded-pill justify-self-start border px-2.5 py-0.5 whitespace-nowrap"
      >
        {GLYPH.dot} {issue.state === 'open' ? 'Open' : 'Closed'}
      </span>
      <span role="cell" className="text-fg2 text-term whitespace-nowrap">
        {updatedPhrase(now, issue.updatedAt)}
      </span>
      <div role="cell" className="flex items-center justify-end gap-1.5">
        <button
          type="button"
          onClick={onStart}
          disabled={busy || blocked}
          // The accessible name carries the issue, because the visible label is the same five
          // characters on every row — a screen reader reading "Start" eleven times says
          // nothing about which one is focused.
          aria-label={`Start #${issue.number}: ${issue.title}`}
          className="border-line2 bg-bg3 text-fg text-term rounded-control cursor-pointer border px-3 py-1 whitespace-nowrap disabled:cursor-default disabled:opacity-60 focus-visible:shadow-focus focus-visible:outline-none"
        >
          {busy ? 'Starting…' : `${GLYPH.agent} Start ${TASK_GLYPH.start}`}
        </button>
        <button
          type="button"
          disabled
          aria-label={`More for #${issue.number}`}
          title="Per-issue actions arrive with the worktree manager in v0.4."
          className="text-fg3 border-0 bg-transparent px-1 leading-none"
        >
          {TASK_GLYPH.overflow}
        </button>
      </div>
    </div>
  );
}

