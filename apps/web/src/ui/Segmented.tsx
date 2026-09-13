import type { KeyboardEvent } from 'react';

import { isArrowKey, nextOption } from './roving';

/**
 * The segmented control from design-spec.md §5: a `bg2` track with a `line` border, 3px of
 * padding, and the selected segment filled with `line2`.
 *
 * Implemented as a radio group, which is what it is — one tab stop for the whole control,
 * arrow keys moving the selection, and a screen reader reading "2 of 3" instead of three
 * unrelated buttons. Roving tabindex without the arrow handler would be worse than a plain
 * row of buttons: the keyboard could reach the selected option and nothing else, so the
 * control could not be changed at all without a mouse.
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
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!isArrowKey(event.key)) {
      return;
    }
    // Swallowed even when the selection does not move, so the arrow never scrolls the
    // settings pane out from under the control the user is operating.
    event.preventDefault();
    const next = nextOption(options, value, event.key);
    if (next !== undefined) {
      onChange(next);
    }
  }

  return (
    <div
      role="radiogroup"
      aria-label={label}
      onKeyDown={onKeyDown}
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
