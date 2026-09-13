import { useSnapshot } from '../store/hooks';
import { TerminalView } from '../transport/TerminalView';
import { GLYPH } from '../ui/glyphs';

/**
 * The main pane.
 *
 * The geometry is this file's: `0 24px` of padding, Fira Code at 12.5px/1.65, and a prompt
 * line carrying the accent focus ring. The terminal inside it is `TerminalView` from
 * `src/transport`, which owns the renderer, the WebGL pool and the acknowledgement of
 * rendered bytes — none of which a component should know about.
 *
 * Terminal state lives in Rust (D-7): this pane is a display cache, never an authority,
 * which is why it holds no scrollback of its own.
 */
export function SessionPane() {
  const { tabs, activeTab } = useSnapshot();
  const tab = tabs.find((candidate) => candidate.paneKey === activeTab);

  return (
    <div className="flex min-h-0 flex-col px-6">
      <div className="text-term flex min-h-0 flex-1 flex-col gap-4 overflow-hidden pt-4 font-mono">
        {tab ? (
          <TerminalView />
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
