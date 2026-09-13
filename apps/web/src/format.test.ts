import { describe, expect, it } from 'vitest';

import { formatAge, formatMemory, formatUsageWindow } from './format';

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

describe('formatAge', () => {
  it('prints the ages the design mock shows', () => {
    const now = Date.parse('2026-09-13T12:16:00Z');
    expect(formatAge(now, now - 21 * HOUR)).toBe('21h');
    expect(formatAge(now, now - 3 * MINUTE)).toBe('3m');
  });

  it('uses exactly one unit at every scale', () => {
    const now = 10 * DAY;
    expect(formatAge(now, now - 12 * SECOND)).toBe('12s');
    expect(formatAge(now, now - (90 * MINUTE + 30 * SECOND))).toBe('1h');
    expect(formatAge(now, now - 4 * DAY - 7 * HOUR)).toBe('4d');
  });

  it('rounds down at every boundary', () => {
    const now = 10 * DAY;
    expect(formatAge(now, now - (MINUTE - 1))).toBe('59s');
    expect(formatAge(now, now - MINUTE)).toBe('1m');
    expect(formatAge(now, now - (HOUR - 1))).toBe('59m');
    expect(formatAge(now, now - HOUR)).toBe('1h');
    expect(formatAge(now, now - (DAY - 1))).toBe('23h');
    expect(formatAge(now, now - DAY)).toBe('1d');
  });

  it('reads a backwards clock as zero rather than a negative age', () => {
    expect(formatAge(0, 5 * MINUTE)).toBe('0s');
  });
});

describe('formatMemory', () => {
  it('prints the daemon figure the status bar shows', () => {
    expect(formatMemory(4 * 1024 ** 3)).toBe('4.00 GB');
  });

  it('steps through the units', () => {
    expect(formatMemory(512)).toBe('512 B');
    expect(formatMemory(1024)).toBe('1.00 KB');
    expect(formatMemory(12.5 * 1024 ** 2)).toBe('12.50 MB');
    expect(formatMemory(3 * 1024 ** 4)).toBe('3.00 TB');
  });

  it('does not run off the end of the unit table', () => {
    expect(formatMemory(1024 ** 6)).toMatch(/ TB$/);
  });

  it('survives the values a disconnected daemon reports', () => {
    expect(formatMemory(0)).toBe('0 B');
    expect(formatMemory(-1)).toBe('0 B');
    expect(formatMemory(Number.NaN)).toBe('0 B');
  });
});

describe('formatUsageWindow', () => {
  it('prints the status bar summary', () => {
    expect(formatUsageWindow('5h', 100)).toBe('100% left 5h');
    expect(formatUsageWindow('Fable', 99.4)).toBe('99% left Fable');
  });
});
