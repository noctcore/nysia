// An escaped quote inside a string. A scanner that stops at the first `"` after the opener
// leaves the rest of the literal read as code, and the `/*` inside it then opens a block
// comment that swallows the import below.
const said = "he said \"/*\"";
const store = await import('../store/StoreContext');

export const reached = [said, store];
