import { afterEach, describe, expect, it, vi } from 'vitest';

import { hasDistinctIds } from './errors';
import { createFailureSink, describeCause } from './unexpectedFailures';

afterEach(() => {
  vi.restoreAllMocks();
});

function sink() {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  return createFailureSink();
}

describe('failure sink', () => {
  it('starts empty and stays referentially stable between changes', () => {
    // Read through `useSyncExternalStore`, so the same rule the store snapshot follows.
    const failures = sink();
    expect(failures.getSnapshot()).toEqual([]);
    expect(failures.getSnapshot()).toBe(failures.getSnapshot());
  });

  it('notifies on a report and stops after unsubscribe', () => {
    const failures = sink();
    let calls = 0;
    const unsubscribe = failures.subscribe(() => {
      calls += 1;
    });

    failures.report('openTab', new Error('boom'));
    expect(calls).toBe(1);
    expect(failures.getSnapshot()).toHaveLength(1);

    unsubscribe();
    failures.report('closeTab', new Error('boom'));
    expect(calls).toBe(1);
    expect(failures.getSnapshot()).toHaveLength(2);
  });

  it('gives every report its own id', () => {
    // The same invariant the contract requires of a provider, for the same reason:
    // `CommandErrors` keys on it and dismisses by it.
    const failures = sink();
    failures.report('openTab', new Error('boom'));
    failures.report('openTab', new Error('boom'));
    expect(hasDistinctIds(failures.getSnapshot())).toBe(true);
  });

  it('dismisses one notice and leaves the rest', () => {
    const failures = sink();
    failures.report('openTab', new Error('one'));
    failures.report('closeTab', new Error('two'));
    const first = failures.getSnapshot()[0];

    failures.dismiss(first?.id ?? '');
    expect(failures.getSnapshot()).toHaveLength(1);
    expect(failures.getSnapshot()[0]?.command).toBe('closeTab');
  });

  it('dismissing something already gone is not an error and notifies nobody', () => {
    const failures = sink();
    let calls = 0;
    failures.subscribe(() => {
      calls += 1;
    });
    expect(() => failures.dismiss('never_existed')).not.toThrow();
    expect(calls).toBe(0);
  });

  it('keeps one sink independent of another', () => {
    const a = sink();
    const b = sink();
    a.report('openTab', new Error('boom'));
    expect(b.getSnapshot()).toEqual([]);
  });

  it('still logs the cause, so a developer gets the stack and not just the sentence', () => {
    const console_ = vi.spyOn(console, 'error').mockImplementation(() => {});
    const failures = createFailureSink();
    const cause = new TypeError('socket closed');
    failures.report('openTab', cause);
    expect(console_).toHaveBeenCalledWith('Store command openTab failed unexpectedly', cause);
  });
});

describe('describeCause', () => {
  it('names the likely cause, which is the part the user can act on', () => {
    expect(describeCause(new Error('EPIPE'))).toContain('connection may have dropped');
    expect(describeCause(new Error('EPIPE'))).toContain('EPIPE');
  });

  it('handles a rejection that is not an Error', () => {
    expect(describeCause('a string')).toContain('a string');
    expect(describeCause(undefined)).toContain('connection may have dropped');
    expect(describeCause({ nested: true })).toContain('connection may have dropped');
  });
});
