// Form 1 of #19: a template literal with no substitution.
//
// The same module, reached the same way. The specifier test accepted a single- or
// double-quoted literal only, so this passed lint-meta, ESLint, tsc and the Vite build.
const store = await import(`../store/StoreContext`);

export const reached = store;
