// A string literal holding a block-comment opener.
//
// If the comment blanker cannot tell a string from code, this opens a block comment that
// never closes and every line below it is blanked — the rule reports nothing and the gate
// keeps saying success. That is the worst failure this file can have, so the literal sits
// alone: a later `*/` anywhere in the file would close the runaway comment and rescue the
// import by accident.
const open = "/*";
const store = await import('../store/StoreContext');

export const reached = [open, store];
