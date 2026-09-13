import { useEffect, useRef, type KeyboardEvent } from 'react';

import { isArrowKey, nextOption, tabbableIndex } from './roving';

/**
 * The segmented control from design-spec.md §5: a `bg2` track with a `line` border, 3px of
 * padding, and the selected segment filled with `line2`.
 *
 * A radio group, which is what it is — one tab stop for the whole control, arrow keys
 * moving the selection, and a screen reader reading "2 of 3" instead of three unrelated
 * buttons. Two details of the APG pattern are easy to leave out and both break it:
 *
 *  - **Arrow moves focus as well as checking.** Changing `value` alone leaves the focus
 *    ring sitting on a segment that is now unselected, and a screen reader announces the
 *    state of the wrong one. Focus has to follow the check, which means after the parent
 *    has re-rendered — hence the pending ref rather than a `focus()` in the handler.
 *  - **Exactly one segment is tabbable, always.** Deriving that from `option === value`
 *    alone means a stored value outside the option list — a preference written by a build
 *    that spelled something differently — leaves every radio at `tabIndex={-1}` and the
 *    whole control unreachable from the keyboard. The fallback is the first segment.
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
  const buttons = useRef(new Map<T, HTMLButtonElement>());
  const pendingFocus = useRef<T | null>(null);

  useEffect(() => {
    const target = pendingFocus.current;
    if (target === null) {
      return;
    }
    pendingFocus.current = null;
    // Only if the parent actually took the change. A controlled component whose owner
    // ignores `onChange` — a disabled group, a form that vetoes the value — would
    // otherwise keep the request pending until some unrelated render, and steal focus
    // then, long after the key that asked for it.
    if (target === value) {
      buttons.current.get(target)?.focus();
    }
  });

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!isArrowKey(event.key)) {
      return;
    }
    // Swallowed even when the selection does not move, so the arrow never scrolls the
    // settings pane out from under the control the user is operating.
    event.preventDefault();
    const next = nextOption(options, value, event.key);
    if (next !== undefined) {
      pendingFocus.current = next;
      onChange(next);
    }
  }

  const tabbable = tabbableIndex(options, value);

  return (
    <div
      role="radiogroup"
      aria-label={label}
      onKeyDown={onKeyDown}
      className="border-line bg-bg2 flex flex-none rounded-control border p-[3px] text-xs"
    >
      {options.map((option, index) => {
        const selected = option === value;
        return (
          <button
            key={option}
            ref={(node) => {
              if (node) {
                buttons.current.set(option, node);
              } else {
                buttons.current.delete(option);
              }
            }}
            type="button"
            role="radio"
            aria-checked={selected}
            tabIndex={index === tabbable ? 0 : -1}
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
