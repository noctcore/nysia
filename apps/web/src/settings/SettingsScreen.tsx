import { useMemo, useState } from 'react';

import type { Project } from '../store/types';
import { useSnapshot } from '../store/hooks';
import { GLYPH, SETTINGS_SEARCH_HINT } from '../ui/glyphs';
import { ComingSoon } from '../ui/ComingSoon';
import { SectionLabel } from '../ui/SectionLabel';
import { AppearancePane } from './AppearancePane';
import { GeneralPane } from './GeneralPane';
import {
  DEFAULT_SETTINGS_ENTRY,
  SETTINGS_TREE,
  type SettingsEntry,
  type SettingsEntryId,
} from './nav';
import { matches, matchingGroups } from './search';

/**
 * Settings as a full-window mode (design-spec.md §5): 270px of nav, then content at
 * max-width 980px with 22px by 40px of padding.
 *
 * A mode rather than a modal, and the 270px matches the wordmark slot above it, so the
 * vertical rule down the window does not move when you enter settings — only what is
 * beside it changes.
 *
 * The nav renders the whole tree; only General and Appearance are built. Everything else
 * opens an honest placeholder naming the release it is due in, because a settings entry
 * that silently does nothing is worse than one that says it is not here yet.
 *
 * The selected entry is local state. Which pane a window is showing is not something the
 * daemon knows, and two windows should be able to sit on different panes.
 */
export function SettingsScreen() {
  const { projects } = useSnapshot();
  const [selected, setSelected] = useState<SettingsEntryId | string>(
    DEFAULT_SETTINGS_ENTRY,
  );
  const [query, setQuery] = useState('');

  /*
   * The search box used to be a `<div>` styled as an input — a control that looked live
   * and was not. The nav beside it names a release for every entry it has not built, and
   * the same standard applies here; the difference is that this one can simply be built,
   * because the tree it filters is already in memory. Nothing about it is a daemon round
   * trip, so there is nothing to defer.
   */
  const groups = useMemo(() => matchingGroups(query), [query]);
  const matchedProjects = useMemo(
    () => projects.filter((project) => matches(project.name, query)),
    [projects, query],
  );
  const empty = groups.length === 0 && matchedProjects.length === 0;

  return (
    <div className="grid min-h-0 grid-cols-[var(--spacing-wordmark)_1fr]">
      <nav
        aria-label="Settings"
        className="border-line bg-bg0 flex flex-col gap-0.5 overflow-y-auto border-r px-3 py-3.5"
      >
        <label className="bg-bg2 mb-3 flex items-center gap-2 rounded-control px-3 py-2 focus-within:shadow-focus">
          <span aria-hidden="true" className="text-fg3">
            {GLYPH.search}
          </span>
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search settings"
            aria-label="Search settings"
            className="text-fg placeholder:text-fg3 w-full min-w-0 border-0 bg-transparent outline-none"
          />
          <span aria-hidden="true" className="text-fg3 font-mono text-[10px]">
            {SETTINGS_SEARCH_HINT}
          </span>
        </label>

        {groups.map((group) => (
          <div key={group.label} className="contents">
            <SectionLabel tracking="nav" className="px-3 pt-3 pb-1.5">
              {group.label}
            </SectionLabel>
            {group.entries.map((entry) => (
              <NavItem
                key={entry.id}
                label={entry.label}
                tag={entry.tag}
                selected={selected === entry.id}
                onSelect={() => setSelected(entry.id)}
              />
            ))}
          </div>
        ))}

        {matchedProjects.length === 0 ? null : (
          <>
            <SectionLabel tracking="nav" className="px-3 pt-3 pb-1.5">
              Projects
            </SectionLabel>
            {matchedProjects.map((project) => (
              <NavItem
                key={project.id}
                label={project.name}
                selected={selected === project.id}
                onSelect={() => setSelected(project.id)}
              />
            ))}
          </>
        )}

        {empty ? (
          <p className="text-fg3 px-3 py-2 text-xs">No settings match “{query}”.</p>
        ) : null}
      </nav>

      <div className="flex max-w-[980px] min-h-0 flex-col gap-3.5 overflow-y-auto px-10 py-[22px]">
        <SettingsPane selected={selected} projects={projects} />
      </div>
    </div>
  );
}

function SettingsPane({
  selected,
  projects,
}: {
  readonly selected: string;
  readonly projects: readonly Project[];
}) {
  if (selected === 'general') {
    return <GeneralPane />;
  }
  if (selected === 'appearance') {
    return <AppearancePane />;
  }

  const entry = findEntry(selected);
  if (entry) {
    return (
      <ComingSoon title={entry.label} version={entry.version ?? 'a later version'} detail={entry.detail} />
    );
  }

  // A project row, matched by id. Per-project overrides are v0.4, with the worktree
  // manager.
  const project = projects.find((candidate) => candidate.id === selected);
  return (
    <ComingSoon
      title={project?.name ?? 'Project'}
      version="v0.4"
      detail="Per-project overrides arrive with the worktree manager."
    />
  );
}

function findEntry(id: string): SettingsEntry | undefined {
  for (const group of SETTINGS_TREE) {
    const entry = group.entries.find((candidate) => candidate.id === id);
    if (entry) {
      return entry;
    }
  }
  return undefined;
}

function NavItem({
  label,
  tag,
  selected,
  onSelect,
}: {
  readonly label: string;
  readonly tag?: string | undefined;
  readonly selected: boolean;
  readonly onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-current={selected ? 'page' : undefined}
      onClick={onSelect}
      className={`flex w-full cursor-pointer items-center gap-2.5 rounded-control border-0 px-3 py-[7px] text-left focus-visible:shadow-focus focus-visible:outline-none ${
        selected ? 'bg-bg3 text-fg' : 'text-fg2 hover:text-fg bg-transparent'
      }`}
    >
      {label}
      {tag === undefined ? null : (
        <span className="bg-bg2 text-fg3 tracking-group ml-auto rounded-pill px-1.5 py-px text-[9.5px] uppercase">
          {tag}
        </span>
      )}
    </button>
  );
}
