import type { ReactNode } from 'react';

/**
 * The two shapes every settings pane is made of (design-spec.md §5): the h1 header with its
 * bottom rule, and the `bg0` card at radius 14 that holds a stack of setting rows.
 *
 * Shared so the panes cannot drift apart — the spec gives one geometry for all of them, and
 * a second pane re-deriving it by eye is how a 14px radius becomes a 12px one.
 */
export function SettingsHeader({
  title,
  description,
}: {
  readonly title: string;
  readonly description: string;
}) {
  return (
    <div className="border-line border-b pb-3">
      <h1 className="text-h1 tracking-wordmark m-0 mb-1 font-semibold">{title}</h1>
      <div className="text-fg2 text-row">{description}</div>
    </div>
  );
}

export function SettingsCard({
  title,
  children,
}: {
  readonly title?: string;
  readonly children: ReactNode;
}) {
  return (
    <section className="border-line bg-bg0 flex flex-col gap-3 rounded-card border px-5 py-3.5">
      {title === undefined ? null : (
        <h2 className="text-row m-0 font-semibold">{title}</h2>
      )}
      {children}
    </section>
  );
}
