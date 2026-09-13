import type { ReactNode } from 'react';

/**
 * The setting-row pattern from design-spec.md §5: a 14px/600 label, a 12.5px `fg2`
 * description, and a right-aligned control, separated from the row above by a `line` rule.
 *
 * The first row in a card has no rule, which is what `divider` turns off — the mock draws
 * the separator on the row rather than between rows, and the card's own border already
 * closes the top.
 */
export function SettingRow({
  label,
  description,
  control,
  divider = true,
}: {
  readonly label: string;
  readonly description: string;
  readonly control: ReactNode;
  readonly divider?: boolean;
}) {
  return (
    <div
      className={`flex items-start gap-6 ${divider ? 'border-line border-t pt-3' : ''}`}
    >
      <div className="flex-1">
        <div className="text-row font-semibold">{label}</div>
        <div className="text-fg2 text-term mt-[3px] leading-normal">{description}</div>
      </div>
      {control}
    </div>
  );
}
