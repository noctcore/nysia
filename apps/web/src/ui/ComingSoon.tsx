/**
 * An honest empty state.
 *
 * The design spec draws three screens and a settings tree that anticipate a year of
 * product; v0.1 ships the Session screen plus General and Appearance. Every other entry
 * renders this rather than a convincing mock-up, because a fake screen is indistinguishable
 * from a broken one and costs someone a bug report.
 *
 * `version` is the release the delivery plan actually promises it in, so the placeholder
 * carries a commitment rather than a shrug.
 */
export function ComingSoon({
  title,
  version,
  detail,
}: {
  readonly title: string;
  readonly version: string;
  readonly detail?: string | undefined;
}) {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-10">
      <div className="border-line bg-bg0 max-w-[420px] rounded-card border p-6 text-center">
        <div className="text-row font-semibold">{title}</div>
        <p className="text-fg2 text-term mt-2 leading-normal">
          Coming in {version}.{detail === undefined ? '' : ` ${detail}`}
        </p>
      </div>
    </div>
  );
}
