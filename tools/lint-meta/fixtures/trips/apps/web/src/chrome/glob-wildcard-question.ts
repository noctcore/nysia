// A question mark in a glob is the single-character wildcard, not a URL query separator.
//
// The extension check split the pattern on `?` as if it were a query string and read what
// was left — "pinned to extension t" — so it exempted the call. Vite hands the pattern to
// its matcher verbatim, where `*.t?x` matches `StoreProvider.tsx` and the call returns every
// matching module in the store. Zero violations, zero ESLint warnings, a clean type check.
const modules = import.meta.glob('../store/*.t?x');

export const reached = modules;
