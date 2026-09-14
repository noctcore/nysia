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
