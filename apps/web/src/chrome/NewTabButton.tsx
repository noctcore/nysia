import { useCallback, useRef, useState } from 'react';

import { runCommand } from '../store/runCommand';
import { useSnapshot, useStore } from '../store/useStore';
import { GLYPH } from '../ui/glyphs';
import { SectionLabel } from '../ui/SectionLabel';
import { useDismiss } from '../ui/useDismiss';

/**
 * The `+` button and its 250px menu (design-spec.md §2).
 *
 * The menu is the clearest statement of what a tab is: two groups, AGENTS then TERMINALS,
 * and picking either one produces the same kind of surface. What is actually launchable is
 * store data, not a constant here — which shells exist on the machine is something only
 * the daemon can answer, and on Windows that answer is the difference between pwsh, cmd,
 * WSL and Git Bash being offered or not.
 */
export function NewTabButton() {
  const { launchers } = useSnapshot();
  const store = useStore();
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLDivElement>(null);

  const close = useCallback(() => setOpen(false), []);
  useDismiss(container, open, close);

  return (
    <div ref={container} className="relative">
      <button
        type="button"
        data-new-session
        aria-label="New session"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
        className="bg-bg3 text-fg mb-1.5 grid size-7 cursor-pointer place-items-center rounded-chip border-0 text-base focus-visible:shadow-focus focus-visible:outline-none"
      >
        {GLYPH.add}
      </button>

      {open ? (
        <div
          role="menu"
          aria-label="New session"
          className="border-line2 bg-bg2 absolute top-[42px] left-0 z-20 flex w-[250px] flex-col gap-0.5 rounded-panel border p-1.5 shadow-popover"
        >
          {launchers.map((group) => (
            <div key={group.label} className="contents">
              <SectionLabel className="px-2.5 pt-2 pb-1">{group.label}</SectionLabel>
              {group.items.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    close();
                    runCommand(store.openTab(item.id));
                  }}
                  className="text-fg hover:bg-bg3 flex cursor-pointer items-center gap-2.5 rounded-chip border-0 bg-transparent px-2.5 py-[7px] text-left focus-visible:shadow-focus focus-visible:outline-none"
                >
                  <span
                    aria-hidden="true"
                    className="text-fg2 w-4 text-center font-mono text-[11px]"
                  >
                    {item.kind === 'agent' ? GLYPH.agent : GLYPH.shell}
                  </span>
                  <span className="flex-1">{item.label}</span>
                  <span className="text-fg3 font-mono text-[11px]">{item.hint}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
