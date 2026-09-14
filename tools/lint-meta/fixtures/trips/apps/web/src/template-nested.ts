// A template literal whose `${…}` substitution holds a string with a backtick in it.
//
// The scanner ran to the first unescaped backtick and called that the end of the literal,
// which is only true when substitutions contain none. Here the quoted backtick inside the
// substitution ends the outer template early, the real closing quote then opens a new one,
// and the template on the next line is read one quote out of step — leaving its `/*` loose.
const label = `outer ${JSON.stringify("`")} tail`;
const opener = `/*`;
const store = await import('../store/StoreContext');

/*
 * As in `regex-backtick.ts`, this closer is what makes the fixture discriminating: a
 * runaway comment above ends here rather than at end of file, so only scanning the
 * substitution keeps the import visible.
 */
export const reached = [label, opener, store];
