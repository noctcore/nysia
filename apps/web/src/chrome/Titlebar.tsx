import { COMMAND_PALETTE_HINT, GLYPH } from '../ui/glyphs';
import { TabStrip } from './TabStrip';
import { WindowControls } from './WindowControls';
import { Wordmark } from './Wordmark';

/**
 * The 40px titlebar (design-spec.md §2).
 *
 * Two shapes, one row. In the app it carries the wordmark, the tab strip, the Command-K
 * chip and the window controls; in settings — a full-window mode, not a modal — the strip
 * is replaced by a single back control and everything else stays put, so the window never
 * appears to change size or lose its buttons.
 *
 * `data-tauri-drag-region` is a plain DOM attribute the shell reads, not an import, so the
 * titlebar can be draggable without `apps/web` touching Tauri (D-1, D-2). It goes on the
 * bar and on the wordmark slot only: a drag region over a button swallows its clicks.
 */
export function Titlebar({
  settingsOpen,
  onCloseSettings,
}: {
  readonly settingsOpen: boolean;
  readonly onCloseSettings: () => void;
}) {
  return (
    <div
      data-tauri-drag-region
      className="border-line bg-bg0 flex items-center gap-4 border-b pl-3.5"
    >
      <Wordmark />
      {settingsOpen ? (
        <div className="flex flex-1 items-center gap-2">
          <button
            type="button"
            onClick={onCloseSettings}
            className="bg-bg2 text-fg2 hover:text-fg cursor-pointer rounded-chip border-0 px-2.5 py-[5px] focus-visible:shadow-focus focus-visible:outline-none"
          >
            {GLYPH.back} Back to app
          </button>
        </div>
      ) : (
        <TabStrip />
      )}
      <div className="text-fg2 flex items-center gap-2 text-xs">
        <span className="bg-bg2 rounded-chip px-2.5 py-[5px] font-mono">
          {COMMAND_PALETTE_HINT}
        </span>
      </div>
      <WindowControls />
    </div>
  );
}
