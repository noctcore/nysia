import { useCallback, useRef, useState } from 'react';

import { formatMemory, formatUsageWindow } from '../format';
import type { StoreStatus } from '../store/types';
import { useSnapshot } from '../store/useStore';
import { GLYPH } from '../ui/glyphs';
import { useDismiss } from '../ui/useDismiss';

/**
 * The 30px status bar (design-spec.md §2).
 *
 * Left: the accent asterisk, a 60×4 progress bar and the usage summary. Right: daemon
 * state, memory, terminal count and worktree count — four numbers that answer "is the
 * thing that owns my shells alive, and how much is it holding", which under D-1/D-2 is
 * exactly what a user cannot otherwise see, because the daemon outlives this window.
 *
 * The usage segment opens a placeholder. The real multi-provider surface — per-provider
 * rows, quota windows, the hover flyout, "Manage accounts…" — is v0.4, and building a
 * convincing shell of it now would be a screen someone has to delete. The affordance is
 * here so the status bar does not have to change when it lands.
 */
export function StatusBar() {
  const { usage, daemon, status } = useSnapshot();
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useDismiss(container, open, close);

  const leadWindow = usage[0];
  const summary = usage
    .map((window) => formatUsageWindow(window.label, window.percentLeft))
    .join(' · ');

  return (
    <div className="border-line bg-bg0 text-fg2 text-chip flex items-center gap-4 border-t px-3.5">
      <div ref={container} className="relative flex items-center gap-4">
        <button
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          onClick={() => setOpen((current) => !current)}
          className="flex cursor-pointer items-center gap-2 border-0 bg-transparent p-0 focus-visible:shadow-focus focus-visible:outline-none"
        >
          <span aria-hidden="true" className="text-acc">
            {GLYPH.agent}
          </span>
          <span
            aria-hidden="true"
            className="bg-line2 relative block h-1 w-[60px] overflow-hidden rounded-sm"
          >
            <span
              className="bg-acc absolute inset-y-0 left-0 block"
              style={{ width: `${clampPercent(leadWindow?.percentLeft ?? 0)}%` }}
            />
          </span>
          <span className="text-fg">{summary}</span>
        </button>
        <span aria-hidden="true" className="text-fg3">
          {GLYPH.refresh}
        </span>
        {open ? <UsagePlaceholder /> : null}
      </div>

      <div className="ml-auto flex gap-4">
        <span title={DAEMON_STATE[status].title}>
          <span aria-hidden="true" className={DAEMON_STATE[status].tone}>
            {GLYPH.dot}
          </span>{' '}
          {DAEMON_STATE[status].label}
        </span>
        <span title="Daemon memory">{formatMemory(daemon.memoryBytes)}</span>
        <span title="Open terminals">
          <span aria-hidden="true" className="font-mono">
            {GLYPH.shell}
          </span>{' '}
          {daemon.terminalCount}
        </span>
        <span title="Worktrees">
          <span aria-hidden="true" className="font-mono">
            {GLYPH.branch}
          </span>{' '}
          {daemon.worktreeCount}
        </span>
      </div>
    </div>
  );
}

/** The affordance, not the feature. See the note on `StatusBar`. */
function UsagePlaceholder() {
  return (
    <div
      role="dialog"
      aria-label="Usage"
      className="border-line2 bg-bg2 absolute bottom-[26px] left-0 z-20 w-[360px] rounded-card border p-4 shadow-flyout"
    >
      <div className="text-fg text-row font-semibold">Usage</div>
      <p className="text-fg2 text-term mt-2 leading-normal">
        Per-provider quota windows, accounts and history arrive in v0.4. Until then the
        status bar shows the summary the daemon already reports.
      </p>
    </div>
  );
}

/**
 * The four things the window can honestly say about the daemon.
 *
 * `Reconnecting` is the one that matters under D-1/D-2: the daemon outlives this window,
 * so a dropped socket is not a dead session, and a dot that went straight to `Off` would
 * tell the user their work had stopped when it had not.
 */
const DAEMON_STATE: Readonly<
  Record<StoreStatus, { readonly label: string; readonly title: string; readonly tone: string }>
> = {
  connecting: { label: 'Connecting', title: 'Connecting to the daemon', tone: 'text-status-queued' },
  ready: { label: 'On', title: 'Daemon running', tone: 'text-status-running' },
  reconnecting: {
    label: 'Reconnecting',
    title: 'Lost the daemon connection, retrying — sessions keep running',
    tone: 'text-status-needs-input',
  },
  failed: { label: 'Off', title: 'Daemon unreachable', tone: 'text-status-failed' },
};

function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}
