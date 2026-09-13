/**
 * `nysia` with an accent-coloured full stop, in the fixed 270px slot that lines the
 * titlebar up with the body grid below it: 48px of rail plus 222px of sidebar.
 *
 * The width is the `--spacing-wordmark` token so the two numbers cannot drift apart — the
 * body grid reads the same pair.
 */
export function Wordmark() {
  return (
    <div className="flex w-wordmark flex-none items-center gap-2.5">
      <span className="text-wordmark tracking-wordmark font-semibold">
        nysia<span className="text-acc">.</span>
      </span>
    </div>
  );
}
