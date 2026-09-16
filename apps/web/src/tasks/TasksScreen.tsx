import { useEffect } from 'react';

import { useCommands, useSnapshot } from '../store/hooks';
import { GLYPH } from '../ui/glyphs';
import { useNow } from '../ui/useNow';
import { TASK_GLYPH } from './glyphs';
import { repositoryOf, type Issue } from './issue';
import { TaskTable } from './TaskTable';
import { TasksPanel } from './TasksPanel';
import { isTasksBusy, issuesOf, startedPhrase } from './tasks';

/**
 * Screen 2b — Tasks (design-spec.md §4). The first thing in Nysia that is not a terminal.
 *
 * **Tasks are GitHub Issues, queried live** (D-5). There is no table, no cache and no schema
 * behind this: the store holds one list belonging to one project, thrown away when that
 * project stops being active, and `Start →` turns the issue in front of you into a
 * *branch* — which is the only thing that outlives the query (D-6).
 *
 * # The query is asked for here
 *
 * This screen is mounted only while the rail is on Tasks, so mounting is showing, and the
 * effect below is what makes the list live: idle plus a project means ask. It fires again
 * when the project changes, because `selectProject` puts the state back to idle — a list
 * that outlived its project would put one repository's issues under another's name, and
 * `Start →` would then derive a branch for the wrong worktree.
 *
 * # What is drawn from the design and cannot work yet
 *
 * The design's filter row and source chip row are drawn, with **every control that has
 * nothing behind it disabled and carrying a title that says when**. That is this chrome's
 * own standard — `App.render.test.ts` holds the rail and the sidebar's `+` to it under the
 * name *"ships no affordance that looks live and does nothing"* — and it is why they are
 * not simply left out: a screen missing half the design is a screen nobody can check against
 * the design.
 *
 * What cannot work is not arbitrary. `tasks_list` takes a project and nothing else, so
 * `Open | Assigned to me | Closed`, `≔ Filters` and the query box are all arguments the verb
 * does not have; the repository dropdown would be a second way to choose the project the
 * sidebar already chooses; and `↗` needs a way to open a browser, which this window has no
 * permission for and should not grow one for a link. `↻` is live, and it is the one the
 * failure panel points at.
 */
export function TasksScreen() {
  const { tasks, taskStart, projects, activeProjectId } = useSnapshot();
  const commands = useCommands();
  // One clock for every row, ticking, so `7 days ago` does not freeze at whatever it was
  // when the table mounted.
  const now = useNow();

  const phase = tasks.phase;
  useEffect(() => {
    if (phase === 'idle' && activeProjectId !== null) {
      commands.refreshTasks();
    }
  }, [phase, activeProjectId, commands]);

  const project = projects.find((candidate) => candidate.id === activeProjectId) ?? null;
  const issues = issuesOf(tasks);
  const started = startedPhrase(taskStart);

  if (project === null) {
    // Not one of `TasksState`'s endings, and deliberately not: nothing was asked and nothing
    // refused. The sidebar is the thing with something to say here, and it already says it —
    // `projectsUnavailable` puts the daemon's own sentence where the projects would be.
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-10">
        <div className="border-line bg-bg0 rounded-card max-w-[420px] border p-6 text-center">
          <div className="text-row font-semibold">No project is selected</div>
          <p className="text-fg2 text-term mt-2 leading-normal">
            Tasks are the GitHub issues of a project. Add one with the {GLYPH.add} in the
            sidebar, then choose it.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2.5 p-3.5">
      <SourceRow project={project.name} issues={issues} />
      <FilterRow busy={isTasksBusy(tasks)} onRefresh={() => commands.refreshTasks()} />
      {started === null ? null : (
        <StartedLine
          phrase={started}
          onOpen={() => {
            // Two commands rather than one: which tab is focused and which rail destination
            // is showing are both properties of this window, and the store keeps them apart.
            if (taskStart.phase === 'started') {
              commands.selectTab(taskStart.paneKey);
            }
            commands.selectNav('session');
          }}
        />
      )}
      {issues.length === 0 ? (
        <TasksPanel state={tasks} onRetry={() => commands.refreshTasks()} />
      ) : (
        <TaskTable
          issues={issues}
          taskStart={taskStart}
          now={now}
          onStart={(issue: Issue) => commands.startTask(issue)}
        />
      )}
    </div>
  );
}

/**
 * `◉ GitHub · Local · owner/repo` and the project it belongs to.
 *
 * The slug comes off the first issue's URL, because `gh issue list --json` has no
 * `repository` field — see `./issue.ts`. With no issues there is no URL and the chip says
 * `GitHub · Local` alone, which is true rather than a guess.
 */
function SourceRow({
  project,
  issues,
}: {
  readonly project: string;
  readonly issues: readonly Issue[];
}) {
  const first = issues[0];
  const repository = first === undefined ? null : repositoryOf(first.url);
  const slug = repository === null ? null : `${repository.owner}/${repository.name}`;

  return (
    <div className="flex items-center gap-2.5">
      <span className="border-line bg-bg2 text-fg2 rounded-control text-term border px-3 py-1.5 font-mono">
        {GLYPH.tasks} GitHub · Local{slug === null ? '' : ` · ${slug}`}
      </span>
      <button
        type="button"
        disabled
        title="Tasks follow the project selected in the sidebar; choosing one here arrives with the worktree manager in v0.4."
        className="border-line text-fg rounded-control flex items-center gap-2 border px-3 py-1.5"
      >
        {project}
        <span aria-hidden="true" className="text-fg3">
          {TASK_GLYPH.caret}
        </span>
      </button>
      <button
        type="button"
        disabled
        aria-label="Open the repository on GitHub"
        title="Opening links in a browser arrives with a later version — the window has no permission to launch one."
        className="border-line text-fg2 rounded-control border px-2.5 py-1.5"
      >
        {TASK_GLYPH.external}
      </button>
    </div>
  );
}

/** The design's filter row: one live control, and the rest saying when. */
function FilterRow({
  busy,
  onRefresh,
}: {
  readonly busy: boolean;
  readonly onRefresh: () => void;
}) {
  const whenFiltered =
    'Filtering needs a query on the wire; the task verb takes a project and nothing else in v0.3.';

  return (
    <div className="flex items-center gap-2">
      <div className="bg-bg0 rounded-control text-term flex p-0.5" role="group" aria-label="Filter">
        <span className="bg-bg3 rounded-chip px-3 py-1">Open</span>
        {['Assigned to me', 'Closed'].map((label) => (
          <button
            key={label}
            type="button"
            disabled
            title={whenFiltered}
            className="text-fg3 rounded-chip border-0 bg-transparent px-3 py-1"
          >
            {label}
          </button>
        ))}
      </div>
      <button
        type="button"
        disabled
        title={whenFiltered}
        className="border-line text-fg2 rounded-control border px-3 py-1.5 whitespace-nowrap"
      >
        {TASK_GLYPH.filters} Filters
      </button>
      {/*
       * Not an input. It is a statement of the query that was actually sent — `is:issue
       * is:open` is what `gh issue list` asks by default — and a text box that accepted
       * typing and ignored it would be worse than one that does not accept it.
       */}
      <div className="border-line bg-bg0 text-term flex min-w-0 flex-1 items-center gap-2 rounded-control border px-3 py-1.5 font-mono">
        <span aria-hidden="true" className="text-fg3">
          {GLYPH.search}
        </span>
        <span className="truncate">is:issue is:open</span>
      </div>
      <button
        type="button"
        disabled
        aria-label="New issue"
        title="Opening an issue from Nysia arrives with a later version."
        className="border-line text-fg2 rounded-control border px-2.5 py-1.5"
      >
        {GLYPH.add}
      </button>
      <button
        type="button"
        onClick={onRefresh}
        disabled={busy}
        aria-label="Refresh the issue list"
        className="border-line text-fg2 rounded-control cursor-pointer border px-2.5 py-1.5 disabled:cursor-default disabled:opacity-60 focus-visible:shadow-focus focus-visible:outline-none"
      >
        {GLYPH.refresh}
      </button>
    </div>
  );
}

/**
 * What the last `Start →` did, including whether it adopted a worktree already on the branch.
 *
 * On the Tasks screen rather than as a toast, and the screen deliberately does **not** jump
 * to the session on its own: starting three issues in a row is an ordinary thing to do, and
 * a window that moved after each one would make it impossible. The button is how you follow
 * it, and it says where it goes.
 */
function StartedLine({
  phrase,
  onOpen,
}: {
  readonly phrase: string;
  readonly onOpen: () => void;
}) {
  return (
    <div
      role="status"
      className="border-acc35 bg-bg0 text-term rounded-control flex items-center gap-3 border px-3 py-2"
    >
      <span aria-hidden="true" className="text-acc leading-none">
        {GLYPH.agent}
      </span>
      <span className="text-fg2 min-w-0 flex-1 wrap-anywhere">{phrase}</span>
      <button
        type="button"
        onClick={onOpen}
        className="border-line2 bg-bg3 text-fg rounded-control cursor-pointer border px-3 py-1 whitespace-nowrap focus-visible:shadow-focus focus-visible:outline-none"
      >
        Open the session
      </button>
    </div>
  );
}
