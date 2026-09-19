/**
 * A `Storage` held in a `Map`, for the suites that need somewhere to persist a preference.
 *
 * There is no DOM in the vitest project (D-18), so there is no `localStorage` either. A
 * plain module rather than a `.test.ts`, like `store/agentStatusFixture.ts`: the settings
 * pane and the stop dialog both need one, and the dialog's suite needs the *same* instance
 * the pane later reads, which is the whole claim it makes.
 */
export function memoryStorage(): Storage {
  const items = new Map<string, string>();
  return {
    get length() {
      return items.size;
    },
    clear: () => items.clear(),
    getItem: (key) => items.get(key) ?? null,
    key: (index) => [...items.keys()][index] ?? null,
    removeItem: (key) => {
      items.delete(key);
    },
    setItem: (key, value) => {
      items.set(key, String(value));
    },
  };
}
