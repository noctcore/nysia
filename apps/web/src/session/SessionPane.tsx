import { useSnapshot } from '../store/hooks';
import { GLYPH } from '../ui/glyphs';

/**
 * The main pane: a placeholder terminal surface.
 *
 * Deliberately not xterm.js. The terminal is W5's — the WebGL renderer pool, the
 * multiplexed binary Channel and the ≥ 1 KiB coalescing all belong with the transport, and
 * a second surface built here would be a second thing to delete. What this establishes is
 * the geometry around it: `0 24px` of padding, Fira Code at 12.5px/1.65, and a prompt line
 * carrying the accent focus ring so the pane is already the right shape when the real
 * surface drops in.
 *
 * Terminal state lives in Rust (D-7) — this pane will be a display cache, never an
 * authority, which is why it holds no scrollback of its own even as a placeholder.
 */
export function SessionPane() {
  const { tabs, activeTab } = useSnapshot();
  const tab = tabs.find((candidate) => candidate.paneKey === activeTab);

  return (
    <div className="flex min-h-0 flex-col px-6">
      <div className="text-term flex min-h-0 flex-1 flex-col gap-4 overflow-hidden pt-4 font-mono">
        {tab ? (
          <div className="border-line bg-bg0 text-fg2 rounded-control border p-3.5 whitespace-pre-wrap">
            <span className="text-fg">$</span> {tab.title}
            {'\n'}
            <span className="text-fg3">
              Terminal surface lands with the transport in wave 2. Pane {tab.paneKey} is
              held open by the daemon either way — closing this window does not stop it.
            </span>
          </div>
        ) : (
          <div className="text-fg3 m-auto text-center">
            No session open. Use {GLYPH.add} in the title bar to start an agent or a shell.
          </div>
        )}
      </div>

      <div className="border-line2 bg-bg0 text-fg mt-2.5 flex items-center gap-2.5 rounded-panel border p-3.5 font-mono text-[13px] shadow-focus">
        <span aria-hidden="true" className="text-acc">
          {GLYPH.prompt}
        </span>
        <span className="text-fg3 flex-1">Send a message</span>
        <span aria-hidden="true" className="bg-acc inline-block h-4 w-[7px]" />
      </div>
      <div className="h-3" />
    </div>
  );
}
