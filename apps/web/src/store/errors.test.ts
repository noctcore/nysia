import { describe, expect, it } from 'vitest';

import { StoreCommandError, hasDistinctIds, type StoreError } from './errors';

function failure(id: string, command: StoreError['command'] = 'openTab'): StoreError {
  return { id, command, message: 'no', at: 0 };
}

describe('hasDistinctIds', () => {
  it('accepts an empty list and a list of distinct ids', () => {
    expect(hasDistinctIds([])).toBe(true);
    expect(hasDistinctIds([failure('a'), failure('b'), failure('c')])).toBe(true);
  });

  it('rejects a provider that reuses one id for every failure', () => {
    // The proof that the contract's assertion has teeth. Such a provider satisfies every
    // other line of the suite and then renders duplicate React keys, with a dismiss that
    // clears every notice at once.
    expect(hasDistinctIds([failure('err_1'), failure('err_1')])).toBe(false);
  });

  it('does not treat two failures of the same command as one', () => {
    // Same command, same message, a second apart: two notices, so two ids.
    expect(hasDistinctIds([failure('err_1'), failure('err_2')])).toBe(true);
    expect(hasDistinctIds([failure('same'), failure('same', 'selectTab')])).toBe(false);
  });
});

describe('StoreCommandError', () => {
  it('carries the command and the id of the entry it corresponds to', () => {
    const error = new StoreCommandError('openTab', 'no pwsh on PATH', 'err_7');
    expect(error).toBeInstanceOf(Error);
    expect(error.name).toBe('StoreCommandError');
    expect(error.command).toBe('openTab');
    expect(error.message).toBe('no pwsh on PATH');
    expect(error.errorId).toBe('err_7');
  });
});
