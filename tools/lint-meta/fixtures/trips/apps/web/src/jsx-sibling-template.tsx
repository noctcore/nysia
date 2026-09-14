// An ordinary component line, and the end of hand-lexing JavaScript.
//
// The regex-versus-division guess treated `}` and `<` as characters a regex may follow, so
// the `/` closing the first element opened a regex scan. It ran to the next `/` on the line
// — the one inside the second element's template prop, a Tailwind fraction — and swallowed
// the opening backtick on the way. Everything after that was read one quote out of step, so
// the template on the next line leaked a `/*` into the scan and the import below vanished.
const Row = () => (<><Icon n={i} /><Tag c={`w-1/2`} /></>);
const opener = `/*`;
const store = await import('../store/StoreContext');

/* An ordinary comment, which is what a runaway above would run to. */
export const reached = [Row, opener, store];
