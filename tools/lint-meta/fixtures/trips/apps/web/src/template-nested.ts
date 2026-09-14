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
 * The closer that made this fixture discriminating while a scanner read the file: a runaway
 * comment above ended here rather than at end of file. Kept, because the shape is what the
 * fixture is about.
 */
export const reached = [label, opener, store];
