// The two glob imports rule (d) must leave alone, mirroring the ones `apps/web` really has.
//
// A glob import can hand back a module the specifier never names, so rule (d) reports one
// by default. Two shapes cannot reach the provider and are exempt:
//
//   - `query: '?raw'`, which yields the file's source text rather than a module;
//   - a pattern restricted to extensions that cannot carry commands.
const sources = import.meta.glob('../**/*.{ts,tsx}', {
  query: '?raw',
  import: 'default',
  eager: true,
});
const stylesheets = import.meta.glob('../**/*.css');

export const scanned = [sources, stylesheets];

// The array form, which the first version of the rule read only the first entry of. These
// two are the positive control for the fix: every pattern is read now, and a call whose
// patterns are all stylesheets must still be allowed, or "read every literal" would just be
// "report every glob" wearing a proof.
const icons = import.meta.glob(['../**/*.css', '../**/*.svg']);
const rawSources = import.meta.glob(['../**/*.ts', '../store/*.tsx'], { query: '?raw' });

export const alsoScanned = [icons, rawSources];
