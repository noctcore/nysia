// A raw query written as an object with a true value.
//
// Vite 8.3.0 turns this into `?raw=true`, and its raw handling matches `raw` followed by
// `&` or end of query — so `raw=true` is not the raw flag and the real module comes back.
// Executed against the pinned version, not read off the docs: the loader returns an object
// carrying the provider's exports.
//
// The old check exempted it because the key was spelled `raw`, which is the whole reason the
// exemption is now a short verified list rather than a guess at Vite's option surface.
const modules = import.meta.glob('../store/*.ts', { query: { raw: true } });

export const reached = modules;
