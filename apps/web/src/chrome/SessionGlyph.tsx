import type { SessionKind } from '../generated/SessionKind';
import { GLYPH } from '../ui/glyphs';

/**
 * The mark that says what is running in a pane.
 *
 * An agent is the accent asterisk with a glow; a shell is `>_` in Fira Code, so the two
 * characters occupy one cell and line up down the sidebar. `SessionKind` is the whole
 * discriminator — Claude is the only agent and there is no provider trait (D-3, D-4), so a
 * second agent glyph would be a shape with nothing behind it.
 */
export function SessionGlyph({
  kind,
  className = '',
}: {
  readonly kind: SessionKind;
  readonly className?: string;
}) {
  if (kind === 'agent') {
    return (
      <span
        aria-hidden="true"
        className={`text-acc text-[11px] [text-shadow:0_0_8px_var(--color-acc)] ${className}`}
      >
        {GLYPH.agent}
      </span>
    );
  }
  return (
    <span aria-hidden="true" className={`font-mono text-[11px] ${className}`}>
      {GLYPH.shell}
    </span>
  );
}
