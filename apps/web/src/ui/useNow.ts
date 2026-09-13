import { useEffect, useState } from 'react';

/**
 * A clock that ticks, for the ages beside session rows.
 *
 * Without it a `3m` printed at mount stays `3m` until something unrelated re-renders the
 * sidebar, which reads as a frozen UI. The default interval is 30s: the sidebar's ages are
 * one unit wide, so a shorter tick would repaint for nothing, and a longer one lets `59s`
 * sit visibly past the minute.
 */
export function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);

  return now;
}
