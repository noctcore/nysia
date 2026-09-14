// Three specifiers the literal reader dropped as computed, all of which resolve to the store
// in dev and in build — and none of which ESLint can see, because it does not follow a
// dynamic import at all.
const parenthesised = await import(('../store/StoreContext'));
const asserted = await import('../store/StoreContext' as string);
const satisfied = await import('../store/StoreContext' satisfies string);

export const reached = [parenthesised, asserted, satisfied];
