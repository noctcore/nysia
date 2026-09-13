/**
 * The 38×22 pill toggle from design-spec.md §5.
 *
 * On is the accent, off is `line2`, and the 16px knob slides between 2px and 16px from the
 * left. A real `<button role="switch">` rather than the mock's two spans: the chrome is
 * custom on every platform, so nothing else gives it keyboard focus, a focus ring, or a
 * state a screen reader can read.
 */
export function Toggle({
  checked,
  label,
  onChange,
}: {
  readonly checked: boolean;
  /** Accessible name. The visible label lives in the setting row beside it. */
  readonly label: string;
  readonly onChange: (next: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className={`relative h-[22px] w-[38px] flex-none cursor-pointer rounded-pill border-0 p-0 transition-colors focus-visible:shadow-focus focus-visible:outline-none ${
        checked ? 'bg-acc' : 'bg-line2'
      }`}
    >
      <span
        aria-hidden="true"
        className={`bg-knob absolute top-[3px] size-4 rounded-full transition-[left] ${
          checked ? 'left-4' : 'left-[2px]'
        }`}
      />
    </button>
  );
}
