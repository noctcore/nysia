/**
 * Arrow-key movement for a radio group.
 *
 * Its own module so it can be tested without a DOM (D-18) — and because the rule it
 * encodes is easy to get subtly wrong: the selection wraps, both axes move (the control is
 * a row, but a screen reader may present it as a list), and a value that is not in the
 * option list enters at the first option rather than falling off the end.
 */

const ARROW_STEP: Readonly<Record<string, number | undefined>> = {
  ArrowLeft: -1,
  ArrowUp: -1,
  ArrowRight: 1,
  ArrowDown: 1,
};

/**
 * The option an arrow key moves to, or `undefined` when the key is not an arrow, the list
 * is empty, or the move lands back on the current value.
 */
export function nextOption<T extends string>(
  options: readonly T[],
  current: T,
  key: string,
): T | undefined {
  const step = ARROW_STEP[key];
  if (step === undefined || options.length === 0) {
    return undefined;
  }
  const index = Math.max(options.indexOf(current), 0);
  const next = options[(index + step + options.length) % options.length];
  return next === undefined || next === current ? undefined : next;
}

/** Whether a key press should be swallowed, so the arrow does not also scroll the pane. */
export function isArrowKey(key: string): boolean {
  return ARROW_STEP[key] !== undefined;
}

/**
 * Which option holds the group's single tab stop.
 *
 * A roving tabindex derived from `option === current` alone has a hole: a stored value
 * that is not in the list leaves every option at `-1`, and the whole control drops out of
 * the tab order with no way back in. Falling back to the first option means the group is
 * always reachable, and the user's first arrow press then lands them somewhere valid.
 */
export function tabbableIndex<T extends string>(options: readonly T[], current: T): number {
  const index = options.indexOf(current);
  return index === -1 ? 0 : index;
}
