import { useCallback, useState } from 'react';

import { CommandErrors } from './chrome/CommandErrors';
import { IconRail } from './chrome/IconRail';
import { StatusBar } from './chrome/StatusBar';
import { Titlebar } from './chrome/Titlebar';
import { SessionPane } from './session/SessionPane';
import { SettingsScreen } from './settings/SettingsScreen';
import { ProjectsSidebar } from './sidebar/ProjectsSidebar';
import { useSnapshot } from './store/hooks';
import { ComingSoon } from './ui/ComingSoon';

/**
 * The window: 40px titlebar, 1fr body, 30px status bar (design-spec.md §2).
 *
 * Settings is a **mode**, not a modal — it replaces the body and the tab strip and keeps
 * the titlebar and status bar in place, so nothing about the window appears to move when
 * you enter it. That is why `settingsOpen` lives here and not inside the settings screen,
 * and why it is local state rather than store state: which screen this particular window is
 * showing is not something the daemon knows or should be told.
 */
export function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const openSettings = useCallback(() => setSettingsOpen(true), []);
  const closeSettings = useCallback(() => setSettingsOpen(false), []);

  return (
    <div className="bg-bg1 text-fg relative grid h-full grid-rows-[var(--spacing-titlebar)_1fr_var(--spacing-statusbar)] overflow-hidden">
      <Titlebar settingsOpen={settingsOpen} onCloseSettings={closeSettings} />
      {settingsOpen ? <SettingsScreen /> : <AppBody onOpenSettings={openSettings} />}
      <StatusBar />
      {/* Outside both bodies: a command can fail from settings as easily as from the
          session screen, and the notice must not move when the mode does. */}
      <CommandErrors />
    </div>
  );
}

/** The 48px rail / 222px sidebar / 1fr body grid (design-spec.md §2). */
function AppBody({ onOpenSettings }: { readonly onOpenSettings: () => void }) {
  const { nav } = useSnapshot();

  return (
    <div className="grid min-h-0 grid-cols-[var(--spacing-rail)_var(--spacing-sidebar)_1fr]">
      <IconRail onOpenSettings={onOpenSettings} />
      <ProjectsSidebar />
      <main className="flex min-h-0 flex-col">
        {nav === 'session' ? <SessionPane /> : null}
        {nav === 'tasks' ? (
          <ComingSoon
            title="Tasks"
            version="v0.3"
            detail="Tasks are GitHub Issues, queried live — there is no local task model to build first (D-5)."
          />
        ) : null}
        {nav === 'history' ? (
          <ComingSoon
            title="History"
            version="a later version"
            detail="Past sessions and their scrollback, once the daemon's store is the authority on both."
          />
        ) : null}
      </main>
    </div>
  );
}
