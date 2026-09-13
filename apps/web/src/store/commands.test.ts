import { afterEach, describe, expect, it, vi } from 'vitest';

import { routeCommands } from './commands';
import { StoreCommandError } from './errors';
import { createMockStore } from './mock/MockStore';
import type { Store } from './types';
import { unexpectedFailures } from './unexpectedFailures';

afterEach(() => {
  unexpectedFailures.clear();
  vi.restoreAllMocks();
});

/** A provider that fails the way a broken one does: without wrapping. */
function throwingStore(cause: unknown): Store {
  const base = createMockStore();
  return {
    ...base,
    closeTab: async () => {
      throw cause;
    },
  };
}

describe('routeCommands', () => {
  it('returns void, so there is no promise for a caller to drop', () => {
    // The whole structural point: `void store.closeTab(key)` used to compile and leave an
    // unhandled rejection. There is nothing to leave unhandled now.
    const commands = routeCommands(createMockStore());
    expect(commands.selectNav('tasks')).toBeUndefined();
    expect(commands.window.minimize()).toBeUndefined();
  });

  it('applies the command and calls back once the store has settled', async () => {
    const store = createMockStore();
    const commands = routeCommands(store);
    const settled = await new Promise<string | null>((resolve) => {
      const target = store.getSnapshot().tabs[1]?.paneKey ?? '';
      commands.selectTab(target, () => resolve(commands.getSnapshot().activeTab));
    });
    expect(settled).toBe(store.getSnapshot().tabs[1]?.paneKey);
  });

  it('swallows a StoreCommandError, because the provider already recorded it', async () => {
    const store = createMockStore();
    const commands = routeCommands(store);
    const console_ = vi.spyOn(console, 'error').mockImplementation(() => {});

    await new Promise<void>((resolve) => {
      commands.closeTab('tab_nope:leaf_nope', resolve);
    });

    expect(store.getSnapshot().errors.at(-1)?.command).toBe('closeTab');
    expect(unexpectedFailures.getSnapshot()).toEqual([]);
    expect(console_).not.toHaveBeenCalled();
  });

  it('sends anything else to the failure sink rather than only the console', async () => {
    // `errors.ts` promises a dropped connection reaches the user. A provider that lets a
    // raw error out has broken that, and a console line nobody has open is not a surface.
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const commands = routeCommands(throwingStore(new TypeError('socket closed')));

    await new Promise<void>((resolve) => {
      commands.closeTab('tab_1:leaf_1', resolve);
    });

    const reported = unexpectedFailures.getSnapshot();
    expect(reported).toHaveLength(1);
    expect(reported[0]?.command).toBe('closeTab');
    expect(reported[0]?.message).toContain('socket closed');
    expect(reported[0]?.message).toContain('connection may have dropped');
  });

  it('still calls back when the command failed', async () => {
    // The tab strip moves DOM focus in `onSettled`; a close that failed must not leave
    // focus on a node that may or may not still be there.
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const commands = routeCommands(throwingStore('plain string rejection'));
    const called = await new Promise<boolean>((resolve) => {
      commands.closeTab('tab_1:leaf_1', () => resolve(true));
    });
    expect(called).toBe(true);
  });

  it('routes every verb on the interface', async () => {
    // A verb added to `Store` and forgotten here would be unreachable from the chrome,
    // which is a quieter failure than an unhandled rejection and just as wrong.
    const commands = routeCommands(createMockStore());
    const verbs = Object.keys(commands).filter((key) => key !== 'getSnapshot');
    expect(verbs.sort()).toEqual(
      [
        'closeTab',
        'dismissError',
        'openTab',
        'selectNav',
        'selectProject',
        'selectTab',
        'window',
      ].sort(),
    );
  });

  it('leaves the StoreCommandError available to a provider-side caller', () => {
    // Routing is for components. The contract still requires the rejection itself, and
    // `MockStore` still produces one.
    const store = createMockStore();
    return expect(store.closeTab('tab_nope:leaf_nope')).rejects.toBeInstanceOf(
      StoreCommandError,
    );
  });
});
