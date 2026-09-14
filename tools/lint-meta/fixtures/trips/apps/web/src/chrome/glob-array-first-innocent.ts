// The array form of a glob pattern, which Vite documents as first-class and which the
// first version of this rule waved straight through.
//
// The check read the FIRST literal in the call and decided the whole call on it. A
// stylesheet in front of the store is enough: the rule saw `.css`, concluded the call could
// not return a module, and exempted a glob that returns every module in the store — the
// provider included. It passed ESLint at zero warnings, the lint-meta CLI at four rules and
// no violations, and the type checker, which is the same shape as the defect this rule was
// written to close.
const modules = import.meta.glob(['../**/*.css', '../store/*.ts']);

export const reached = modules;
