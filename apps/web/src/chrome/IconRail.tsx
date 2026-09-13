import { useSnapshot, useStore } from '../store/useStore';
import type { NavSection } from '../store/types';
import { GLYPH } from '../ui/glyphs';

/**
 * The 48px icon rail (design-spec.md §2).
 *
 * 32×32 items at radius 8; the active one takes an `acc14` fill with accent foreground,
 * which is the one place the derived alpha earns its keep — it reads as a tint of the
 * identity colour over `bg0` chrome without a second token per theme.
 *
 * Session, Tasks and History sit at the top; Settings and Help are pushed to the bottom.
 * Settings is not a destination like the other three — it takes the whole window — so it
 * is separated from the nav group here as well as visually.
 */
const DESTINATIONS: readonly { readonly section: NavSection; readonly label: string; readonly glyph: string }[] = [
  { section: 'session', label: 'Session', glyph: GLYPH.session },
  { section: 'tasks', label: 'Tasks', glyph: GLYPH.tasks },
  { section: 'history', label: 'History', glyph: GLYPH.history },
];

export function IconRail({ onOpenSettings }: { readonly onOpenSettings: () => void }) {
  const { nav } = useSnapshot();
  const store = useStore();

  return (
    <nav
      aria-label="Primary"
      className="border-line bg-bg0 flex flex-col items-center gap-1.5 border-r py-3"
    >
      {DESTINATIONS.map(({ section, label, glyph }) => (
        <RailButton
          key={section}
          label={label}
          glyph={glyph}
          active={nav === section}
          onClick={() => void store.selectNav(section)}
        />
      ))}
      <RailButton
        label="Settings"
        glyph={GLYPH.settings}
        active={false}
        className="mt-auto"
        onClick={onOpenSettings}
      />
      <RailButton label="Help" glyph={GLYPH.help} active={false} onClick={() => {}} />
    </nav>
  );
}

function RailButton({
  label,
  glyph,
  active,
  className = '',
  onClick,
}: {
  readonly label: string;
  readonly glyph: string;
  readonly active: boolean;
  readonly className?: string;
  readonly onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-current={active ? 'page' : undefined}
      onClick={onClick}
      className={`grid size-8 cursor-pointer place-items-center rounded-control border-0 focus-visible:shadow-focus focus-visible:outline-none ${
        active ? 'bg-acc14 text-acc' : 'text-fg2 hover:text-fg bg-transparent'
      } ${className}`}
    >
      <span aria-hidden="true">{glyph}</span>
    </button>
  );
}
