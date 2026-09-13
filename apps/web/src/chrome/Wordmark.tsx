/**
 * `nysia` with an accent-coloured full stop, in the fixed 270px slot that lines the
 * titlebar up with the body grid below it: 48px of rail plus 222px of sidebar.
 *
 * The width is the `--spacing-wordmark` token so the two numbers cannot drift apart — the
 * body grid reads the same pair.
 *
 * `data-tauri-drag-region` sits on the slot *and* on the text inside it. Tauri tests the
 * element the pointer actually landed on, not its ancestors, so the attribute on the
 * titlebar root only ever covered the few pixels no child occupied — with the tab strip
 * and window controls filling the bar's full height, that was a 14px pad and a couple of
 * gap seams. A window you cannot pick up by its own name is not a window.
 */
export function Wordmark() {
  return (
    <div
      data-tauri-drag-region
      className="flex w-wordmark flex-none items-center gap-2.5"
    >
      <span
        data-tauri-drag-region
        className="text-wordmark tracking-wordmark font-semibold"
      >
        nysia<span data-tauri-drag-region className="text-acc">.</span>
      </span>
    </div>
  );
}
