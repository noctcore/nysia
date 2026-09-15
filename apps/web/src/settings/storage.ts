/**
 * The browser storage the settings panes persist into, or `undefined` where there is none.
 *
 * There is no DOM in the vitest project (D-18) and none under `renderToStaticMarkup`, so
 * every pane has to cope with the global being absent rather than assume a window. This
 * used to be a private helper in `GeneralPane.tsx`; the Agents pane needed the same three
 * lines, and two copies of a guard is how one of them ends up being the one that forgets.
 *
 * It stays a function rather than a constant: `localStorage` can throw on access in a
 * hardened webview, and a module-level read would take the whole bundle down at import.
 */
export function settingsStorage(): Storage | undefined {
  return typeof localStorage === 'undefined' ? undefined : localStorage;
}
