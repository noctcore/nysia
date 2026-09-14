// `main.helper.tsx` — the file that proved the two allowlists were copies rather than one
// list (#20).
//
// lint-meta allowlisted the store carve-out by the prefix `apps/web/src/main.`, which this
// name starts with. ESLint carved out `apps/web/src/main.{ts,tsx,mts,cts,js,jsx,mjs,cjs}`,
// which this name does not match. So the same file was inside one allowlist and outside the
// other, and a dynamic import of the provider from here tripped nothing at all: ESLint does
// not see `import()`, and lint-meta thought this was the entry point.
//
// The carve-out is the entry point itself, in whichever extension it carries — never a file
// that merely starts with its name.
const store = await import('../store/StoreContext');

export const reached = store;
