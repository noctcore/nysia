// A glob pattern that does not start with a relative prefix.
//
// The rule joined every pattern onto the importing file's directory, which ANCHORS it.
// Vite does not: a pattern beginning with a double star is handed to the globber untouched
// and walked from the filesystem root, so it reaches the store from anywhere. Executed
// against the pinned version, the relative control resolves in milliseconds and this form
// was still globbing after twenty seconds — which is only possible because it is not bounded
// by the directory this file sits in.
//
// The same missing branch covered a root-relative pattern, an alias and a subpath import.
// None of those exist in this app today, and the rule no longer has an opinion about any of
// them: a pattern it cannot anchor is a pattern it cannot judge.
const modules = import.meta.glob('**/StoreContext.ts');

export const reached = modules;
