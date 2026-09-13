/**
 * The segmented control from design-spec.md §5: a `bg2` track with a `line` border, 3px of
 * padding, and the selected segment filled with `line2`.
 *
 * Implemented as a radio group, which is what it is — arrow keys move the selection, the
 * whole group is one tab stop, and a screen reader reads "2 of 3" instead of three
 * unrelated buttons.
 */
export function Segmented<T extends string>({
  options,
  value,
  label,
  onChange,
}: {
  readonly options: readonly T[];
  readonly value: T;
  /** Accessible name for the group. */
  readonly label: string;
  readonly onChange: (next: T) => void;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className="border-line bg-bg2 flex flex-none rounded-control border p-[3px] text-xs"
    >
      {options.map((option) => {
        const selected = option === value;
        return (
          <button
            key={option}
            type="button"
            role="radio"
            aria-checked={selected}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChange(option)}
            className={`cursor-pointer rounded-chip border-0 px-3 py-1 focus-visible:shadow-focus focus-visible:outline-none ${
              selected ? 'bg-line2 text-fg' : 'text-fg2 bg-transparent'
            }`}
          >
            {option}
          </button>
        );
      })}
    </div>
  );
}
