/*
 * Prose about the rule, which must not be the rule tripping.
 *
 * A component must never write `await import('../store/StoreContext')`, nor
 * `require('../store/StoreContext')` — the provider's commands return promises that a call
 * site can drop in silence. Both spellings are here on purpose: comments in `apps/web`
 * discuss this boundary constantly, and rule (d) blanks comment bodies before it scans so
 * that discussing a ban is not breaking it.
 */

/** A doc link, which is the shape the transport modules actually use:
 * {@link import('../store/StoreContext').StoreContext}.
 */
// And the line-comment spelling: await import('../store/StoreContext').
export const documented = true;
