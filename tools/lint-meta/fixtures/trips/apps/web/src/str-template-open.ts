// The same trap in a template literal, which is a third quoting character the scanner has
// to know about and the one a specifier may itself be written in.
const open = `/*`;
const store = await import('../store/StoreContext');

export const reached = [open, store];
