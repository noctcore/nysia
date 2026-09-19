import { useCallback, useRef, useState, type RefObject } from 'react';

import type { StoreCommands } from '../store/commands';
import { useCommands, useSnapshot } from '../store/hooks';
import type { Launcher, LauncherGroup } from '../store/types';
import { GLYPH, launcherGlyph } from '../ui/glyphs';
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
 *
 * Which is why the glyph comes from `launcherGlyph` and not from `kind`. Reading `kind`
 * gave all four shells `>_` and left the hint at the far end of the row doing the entire
 * job of telling them apart (#73) — in the one menu whose whole purpose is that
 * distinction. The lookup keys off the launcher id, which is the same closed set
 * `DaemonStore.profileFor` switches on; see `ui/glyphs.ts` for why each shell wears what
 * it wears.
 *
 * The hooks live here and nothing else does: everything drawn and everything pressed is
 * {@link NewSessionMenu}, which has none, so a node-only test can call it and press its
 * controls.
 */
export function NewTabButton() {
  const { launchers } = useSnapshot();
  const commands = useCommands();
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLDivElement>(null);

  const close = useCallback(() => setOpen(false), []);
  useDismiss(container, open, close);

  return (
    <NewSessionMenu
      container={container}
      open={open}
      setOpen={setOpen}
      launchers={launchers}
      commands={commands}
    />
  );
}

export interface NewSessionMenuProps {
  readonly container?: RefObject<HTMLDivElement | null>;
  readonly open: boolean;
  readonly setOpen: (open: boolean) => void;
  readonly launchers: readonly LauncherGroup[];
  readonly commands: Pick<StoreCommands, 'openTab' | 'refreshLaunchers'>;
}

/**
 * The button and the menu, with no hooks.
 *
 * **Opening the menu asks the daemon again** which shells it can launch. The answer is
 * computed when it is asked, against the daemon's `PATH`, so asking on open is what makes a
 * shell installed into a directory already on that `PATH` show up without a restart. The
 * menu opens at once with the answer it already has and redraws when the new one lands; the
 * store sends one question at a time, and the question rides a connection of its own, so
 * a slow `PATH` delays this menu and nothing else.
 *
 * **A shell the daemon cannot launch is drawn, disabled, with the daemon's reason under its
 * name** — not hidden, and not left clickable. Hidden, a person who wants PowerShell 7 learns
 * nothing about why it is not there. Clickable, the row looks live and fails: one round trip
 * later a notice says what the row could have said, which is the affordance
 * `App.render.test.ts` names as the thing this chrome does not ship. Its sentence is the
 * daemon's own (`the pwsh profile is unavailable: pwsh was not found on PATH`) and names the
 * shell, never a file.
 *
 * A row with no reason is offered even when nothing has vouched for it — the agent row,
 * every row from a daemon too old to be asked — and a launch that fails there still reaches
 * the daemon's refusal. That refusal stays the backstop: a shell can vanish between the
 * answer and the click.
 */
export function NewSessionMenu({
  container,
  open,
  setOpen,
  launchers,
  commands,
}: NewSessionMenuProps) {
  function pick(item: Launcher): void {
    // Held here as well as by `disabled`, which is the browser's promise and not this
    // component's: a row the daemon has refused is never launched from this menu.
    if (item.unavailable !== null) {
      return;
    }
    setOpen(false);
    commands.openTab(item.id);
  }

  return (
    <div ref={container} className="relative">
      <button
        type="button"
        data-new-session
        aria-label="New session"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => {
          if (!open) {
            commands.refreshLaunchers();
          }
          setOpen(!open);
        }}
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
              {group.items.map((item) => {
                const refused = item.unavailable !== null;
                return (
                  <button
                    key={item.id}
                    type="button"
                    role="menuitem"
                    disabled={refused}
                    onClick={() => pick(item)}
                    className={`flex gap-2.5 rounded-chip border-0 bg-transparent px-2.5 py-[7px] text-left focus-visible:shadow-focus focus-visible:outline-none ${
                      refused
                        ? 'text-fg3 cursor-default items-baseline'
                        : 'text-fg hover:bg-bg3 cursor-pointer items-center'
                    }`}
                  >
                    <span
                      aria-hidden="true"
                      className={`w-4 text-center font-mono text-[11px] ${refused ? 'text-fg3' : 'text-fg2'}`}
                    >
                      {launcherGlyph(item.id, item.kind)}
                    </span>
                    <span className="flex min-w-0 flex-1 flex-col">
                      <span>{item.label}</span>
                      {refused ? (
                        <span data-unavailable className="text-fg3 text-[11px] leading-snug">
                          {item.unavailable}
                        </span>
                      ) : null}
                    </span>
                    <span className="text-fg3 font-mono text-[11px]">{item.hint}</span>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
