/**
 * The numbers the chrome prints.
 *
 * The store carries timestamps and byte counts, not the mock's pre-formatted `21h` and
 * `4.00 GB`, because that is what a daemon will send. Formatting at the edge keeps the
 * arithmetic in one testable place instead of in four components.
 */

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * How long a session has been alive, in the sidebar's compact form: `12s`, `3m`, `21h`,
 * `4d`.
 *
 * Always one unit — the column is right-aligned in 222px of sidebar and a second unit
 * would push the title's ellipsis around every minute. A clock that has gone backwards
 * (a daemon on a machine that just synced NTP) reads as `0s` rather than a negative age.
 */
export function formatAge(now: number, startedAt: number): string {
  const elapsed = Math.max(0, now - startedAt);
  if (elapsed < MINUTE) {
    return `${Math.floor(elapsed / SECOND)}s`;
  }
  if (elapsed < HOUR) {
    return `${Math.floor(elapsed / MINUTE)}m`;
  }
  if (elapsed < DAY) {
    return `${Math.floor(elapsed / HOUR)}h`;
  }
  return `${Math.floor(elapsed / DAY)}d`;
}

const MEMORY_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const;

/**
 * Daemon memory, as the status bar prints it: `4.00 GB`.
 *
 * Binary steps with decimal names, which is what every process monitor on the three target
 * platforms does — the number has to match what the user sees in Task Manager or it reads
 * as a bug in Nysia.
 */
export function formatMemory(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return `0 ${MEMORY_UNITS[0]}`;
  }
  const step = Math.min(Math.floor(Math.log2(bytes) / 10), MEMORY_UNITS.length - 1);
  const unit = MEMORY_UNITS[step] ?? MEMORY_UNITS[0];
  const value = bytes / 1024 ** step;
  return step === 0 ? `${Math.round(value)} ${unit}` : `${value.toFixed(2)} ${unit}`;
}

/** A quota window in the status bar's summary: `100% left 5h`. */
export function formatUsageWindow(label: string, percentLeft: number): string {
  return `${Math.round(percentLeft)}% left ${label}`;
}
