// Reaching the raw provider through a call instead of an import statement.
//
// ESLint's no-restricted-imports does not see either of these spellings, so before rule
// (d) existed this file passed typecheck, eslint, lint-meta and the Vite build — and the
// commands it hands back return promises that `void` drops in silence.
//
// Both spellings are here, and the second deliberately puts a segment between `store` and
// the filename: the ban patterns do not require the two to be adjacent, and a rule that
// only matched `store/StoreContext` would miss it.
const dynamic = await import('../store/StoreContext');
const required = require('../store/./StoreContext');

export const reached = [dynamic, required];
