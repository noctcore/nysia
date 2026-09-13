import { runCommand } from '../store/runCommand';
import { useStore } from '../store/useStore';
import { GLYPH } from '../ui/glyphs';

/**
 * Minimise, maximise and close — 46px each, custom on every platform (design-spec.md §6.7).
 *
 * The commands go through the store rather than `@tauri-apps/api/window`: only
 * `src/transport` may import Tauri (D-1, D-2), and the ESLint ban makes that structural
 * rather than a convention. Under the mock provider they resolve and do nothing, which is
 * correct for a window that has no shell around it yet.
 */
export function WindowControls() {
  const { window } = useStore();

  return (
    <div className="flex h-titlebar">
      <ControlButton label="Minimise" glyph={GLYPH.minimize} onClick={window.minimize} />
      <ControlButton
        label="Maximise"
        glyph={GLYPH.maximize}
        className="text-[11px]"
        onClick={window.toggleMaximize}
      />
      <ControlButton
        label="Close"
        glyph={GLYPH.quit}
        className="hover:bg-status-failed hover:text-fg"
        onClick={window.close}
      />
    </div>
  );
}

function ControlButton({
  label,
  glyph,
  className = '',
  onClick,
}: {
  readonly label: string;
  readonly glyph: string;
  readonly className?: string;
  readonly onClick: () => Promise<void>;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={() => runCommand(onClick())}
      className={`text-fg2 hover:bg-bg2 grid w-[46px] cursor-pointer place-items-center border-0 bg-transparent focus-visible:shadow-focus focus-visible:outline-none ${className}`}
    >
      <span aria-hidden="true">{glyph}</span>
    </button>
  );
}
