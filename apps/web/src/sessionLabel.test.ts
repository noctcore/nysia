import { describe, expect, it } from 'vitest';

import { sessionLabel } from './sessionLabel';

describe('sessionLabel', () => {
  it('spells every generated session kind', () => {
    expect(sessionLabel('shell')).toBe('Shell');
    expect(sessionLabel('agent')).toBe('Agent');
  });
});
