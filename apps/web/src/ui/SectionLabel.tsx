import type { ReactNode } from 'react';

/**
 * A group header: 10.5px uppercase in `fg3` (design-spec.md §1).
 *
 * The design tracks the three places it appears differently — .06em in the projects
 * sidebar, .08em in the `+` menu, .1em in the settings nav — so the tracking is a prop
 * rather than a constant, and all three values are tokens.
 */
export type LabelTracking = 'section' | 'group' | 'nav';

const TRACKING: Readonly<Record<LabelTracking, string>> = {
  section: 'tracking-section',
  group: 'tracking-group',
  nav: 'tracking-nav',
};

export function SectionLabel({
  tracking = 'group',
  className = '',
  children,
}: {
  readonly tracking?: LabelTracking;
  readonly className?: string;
  readonly children: ReactNode;
}) {
  return (
    <div
      className={`text-fg3 text-label font-semibold uppercase ${TRACKING[tracking]} ${className}`}
    >
      {children}
    </div>
  );
}
