// A string literal holding a line-comment opener, with the import after it on the same
// line — which is the only way this can hide anything, since a line comment ends at the
// newline.
const line = "//"; const store = await import('../store/StoreContext');

export const reached = [line, store];
