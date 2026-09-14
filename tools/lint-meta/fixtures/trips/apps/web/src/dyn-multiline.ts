// Form 2 of #19: the specifier on its own line.
//
// A line-based scan sees `await import(` on one line and a bare string on the next, and
// neither line matches on its own. Prettier writes this shape by itself once the specifier
// is long enough.
const store = await import(
  '../store/StoreContext'
);

export const reached = store;
