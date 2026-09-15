import { describe, expect, it } from 'vitest';

import { detectInstalledAgents, detectedLabel, type InstalledAgent } from './installedAgents';

const CLAUDE: InstalledAgent = { id: 'claude', name: 'Claude', version: '2.1.263' };

describe('detectedLabel', () => {
  it('states a total that is a fact about the list beneath it', () => {
    // The artboard writes `4 detected` above four hardcoded rows, which is the shape that
    // drifts the moment one row is added or removed. Derived, the two cannot disagree.
    expect(detectedLabel([])).toBe('0 detected');
    expect(detectedLabel([CLAUDE])).toBe('1 detected');
    expect(detectedLabel([CLAUDE, { ...CLAUDE, name: 'Another' }])).toBe('2 detected');
  });
});

describe('detectInstalledAgents', () => {
  it('detects nothing, because no surface in the webview can measure a version', () => {
    // Not a placeholder waiting to be filled with plausible rows: an empty list is the
    // only honest answer until the daemon can be asked, and this case is what fails if
    // someone seeds it from the mock's version numbers in the meantime.
    expect(detectInstalledAgents()).toEqual([]);
  });

  it('returns a stable list, so the pane does not re-render on its own default', () => {
    expect(detectInstalledAgents()).toBe(detectInstalledAgents());
  });
});
