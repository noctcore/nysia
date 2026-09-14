// A regex literal holding a backtick.
//
// The doc for `blankJsComments` claimed a regex could only ever hold a quote, which the
// same-line rule defuses. A backtick is the third quoting character and a template may span
// lines, so the backtick below paired with the one on the next line, the scanner stepped
// over both as one "literal", and the `/*` left behind opened a block comment that swallowed
// the import under it. The rule then reported nothing — the over-blanking false negative
// this file names as the failure that matters.
const re = /[`]/;
const opener = `/*`;
const store = await import('../store/StoreContext');

/*
 * This ordinary comment is load-bearing, and it is the whole reason the fixture is written
 * this way. Its `*/ /*` closer is what a runaway block comment above would run to, so the
 * import stays hidden even when an unterminated comment is treated as code. Without it the
 * guard alone rescues the import and this fixture cannot tell whether regex literals are
 * scanned at all — a proof that passes with the fix reverted is trap 12 wearing a fixture.
 */
export const reached = [re, opener, store];
