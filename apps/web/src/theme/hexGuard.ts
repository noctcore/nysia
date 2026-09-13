/**
 * The finder behind the hardcoded-colour guard.
 *
 * design-spec.md §6.8 is blunt about this: theme and accent are live user tweaks, so a
 * colour that is not a token is a pixel that stops following the switcher. The guard turns
 * that from a review habit into a gate.
 *
 * It looks for CSS hex colours — a hash followed by exactly 3, 4, 6 or 8 hex digits — which
 * is the only colour form a component can plausibly hardcode once the `oklch()` and `rgb()`
 * values live in the token layer.
 */

const HEX_LITERAL =
  /#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})(?![0-9a-fA-F])/g;

/** Every hex colour literal in `source`, in the order it appears. */
export function findHexLiterals(source: string): readonly string[] {
  return source.match(HEX_LITERAL) ?? [];
}

/**
 * The one TypeScript module allowed to carry colour literals: the theme tables.
 *
 * Adding a second entry is a design decision, not a convenience — which is why it has to be
 * made here, in a diff a reviewer sees. The guard also asserts that every entry *still
 * contains* a hex literal, so one left behind after a refactor fails rather than silently
 * widening the hole.
 */
export const TOKEN_DEFINITION_MODULES: readonly string[] = ['src/theme/themes.ts'];

/**
 * The one stylesheet in the package, which is the Tailwind `@theme` block itself.
 *
 * Its contents cannot be scanned: vitest runs with CSS processing off, so a `?raw` import
 * of a stylesheet comes back empty, and turning it on is a change to the shared root
 * `vitest.config.ts`. The guard covers the same ground a different way — it asserts this is
 * the *only* stylesheet in `apps/web`, so a second one cannot appear without failing here
 * and forcing the colours in it to be reviewed.
 */
export const TOKEN_DEFINITION_STYLESHEETS: readonly string[] = ['src/index.css'];
