import { useMemo, useState } from 'react';

import { StatusDot } from '../chrome/StatusDot';
import { formatAge } from '../format';
import { useAgentStatus, useCommands, useSnapshot } from '../store/hooks';
import type { Project, SessionSummary, Worktree } from '../store/types';
import { GLYPH } from '../ui/glyphs';
import { SectionLabel } from '../ui/SectionLabel';
import { useNow } from '../ui/useNow';

/**
 * The 222px projects sidebar (design-spec.md §3).
 *
 * Projects are the spine and sessions nest under them, grouped by branch — never by task
 * id (D-6), because a worktree outlives the task that created it. Only the active project
 * expands, into a block hung off a 2px accent rail.
 *
 * The search query is local state rather than store state: it is a view filter over data
 * the store already holds, and routing every keystroke through a provider that will one
 * day be a daemon round-trip would make typing wait on a socket.
 */
export function ProjectsSidebar() {
  const { projects, activeProjectId } = useSnapshot();
  const commands = useCommands();
  const [query, setQuery] = useState('');

  const visible = useMemo(() => filterProjects(projects, query), [projects, query]);
  const groups = useMemo(() => groupProjects(visible), [visible]);

  return (
    <div className="border-line bg-bg1 flex min-h-0 flex-col gap-0.5 border-r px-2.5 py-3">
      <label className="bg-bg2 mb-2.5 flex items-center gap-2 rounded-control px-2.5 py-2 focus-within:shadow-focus">
        <span aria-hidden="true" className="text-fg3">
          {GLYPH.search}
        </span>
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search projects"
          aria-label="Search projects"
          className="text-fg placeholder:text-fg3 w-full border-0 bg-transparent outline-none"
        />
      </label>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {groups.map(([group, members]) => (
          <div key={group}>
            <SectionLabel
              tracking="section"
              className="flex items-center px-2.5 py-1.5 font-medium"
            >
              {group}
              {/* Registering a repository is daemon work that does not exist yet, so
                  this says so rather than looking live and swallowing the click. */}
              <button
                type="button"
                disabled
                aria-label={`Add a project to ${group}`}
                title="Adding a project arrives with the worktree manager in v0.4"
                className="text-fg3 ml-auto cursor-not-allowed border-0 bg-transparent p-0 text-sm tracking-normal"
              >
                {GLYPH.add}
              </button>
            </SectionLabel>
            {members.map((project) => (
              <div key={project.id}>
                <button
                  type="button"
                  aria-current={project.id === activeProjectId ? 'true' : undefined}
                  onClick={() => commands.selectProject(project.id)}
                  className="text-fg hover:bg-bg2 flex w-full cursor-pointer items-center gap-2.5 rounded-chip border-0 bg-transparent px-2.5 py-[7px] text-left focus-visible:shadow-focus focus-visible:outline-none"
                >
                  <ProjectAvatar />
                  <span className="truncate">{project.name}</span>
                </button>
                {project.id === activeProjectId
                  ? project.worktrees.map((worktree) => (
                      <WorktreeBlock key={worktree.branch} worktree={worktree} />
                    ))
                  : null}
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * The 16px hatched circle from the mock — a repeating 135° gradient between `line` and
 * `bg3`, so a project with no avatar still reads as a distinct object rather than a gap.
 */
function ProjectAvatar() {
  return (
    <span
      aria-hidden="true"
      className="size-4 flex-none rounded-full bg-[repeating-linear-gradient(135deg,var(--color-line)_0_2px,var(--color-bg3)_2px_4px)]"
    />
  );
}

function WorktreeBlock({ worktree }: { readonly worktree: Worktree }) {
  const now = useNow();
  return (
    <div className="border-acc mt-0.5 mb-1.5 ml-3 flex flex-col gap-1.5 border-l-2 pl-2.5">
      <div className="text-fg2 flex items-center gap-2 text-xs">
        <span aria-hidden="true" className="font-mono">
          {GLYPH.branch}
        </span>
        {worktree.branch}
        {worktree.isPrimary ? (
          <span className="bg-line text-fg2 rounded-pill px-1.5 py-px text-[10px]">
            primary
          </span>
        ) : null}
      </div>
      {worktree.sessions.map((session) => (
        <SessionRow key={session.paneKey} session={session} now={now} />
      ))}
    </div>
  );
}

function SessionRow({
  session,
  now,
}: {
  readonly session: SessionSummary;
  readonly now: number;
}) {
  const commands = useCommands();
  const { activeTab } = useSnapshot();
  const status = useAgentStatus(session.paneKey);

  return (
    <button
      type="button"
      onClick={() => commands.selectTab(session.paneKey)}
      aria-current={session.paneKey === activeTab ? 'true' : undefined}
      className={`flex w-full cursor-pointer items-center gap-2 border-0 bg-transparent p-0 text-left text-xs focus-visible:shadow-focus focus-visible:outline-none ${
        session.kind === 'agent' ? 'text-fg' : 'text-fg2'
      }`}
    >
      {session.kind === 'agent' ? (
        // Live status, which is what design-spec.md §6.2 asks the sidebar for: sessions
        // nest under projects and each shows live status and age. It was the flat accent
        // in v0.1 because there was nothing to report; the wire carries an `AgentState`
        // now, and an agent with no row yet still gets the accent — see `agentDot`.
        <StatusDot status={status} now={now} />
      ) : (
        <span aria-hidden="true" className="font-mono text-[10px]">
          {GLYPH.shell}
        </span>
      )}
      <span className="truncate">{session.title}</span>
      <span className="text-fg3 ml-auto text-[11px]">
        {formatAge(now, session.startedAt)}
      </span>
    </button>
  );
}

/** Case-insensitive substring match on the project name, which is all the mock offers. */
function filterProjects(
  projects: readonly Project[],
  query: string,
): readonly Project[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') {
    return projects;
  }
  return projects.filter((project) => project.name.toLowerCase().includes(needle));
}

/** Group headers in first-appearance order, so the store controls the ordering. */
function groupProjects(
  projects: readonly Project[],
): readonly (readonly [string, readonly Project[]])[] {
  const groups = new Map<string, Project[]>();
  for (const project of projects) {
    const members = groups.get(project.group);
    if (members) {
      members.push(project);
    } else {
      groups.set(project.group, [project]);
    }
  }
  return [...groups.entries()];
}
