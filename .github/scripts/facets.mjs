// The label facets from the table in CLAUDE.md section 1, as data.
//
// Its own module, with no side effects, because two checks need the same lists and one
// importing the other would run the other's `main()`. Data belongs where importing it
// costs nothing.
//
// Keep these in step with CLAUDE.md. `.github/labels.yml` is the source of truth for the
// names, colours and descriptions themselves; this file only records which facet each
// one belongs to, which is the part the automation has to reason about.

/**
 * Exactly one of these per pull request. The kind of change, which a diff cannot infer.
 *
 * `dependencies` is deliberately absent: it is an Extra, because a manifest change is a
 * fact about the diff rather than a kind of change, and a PR can be an `enhancement`
 * that also bumps a lockfile. While it was listed here, a PR labelled exactly as
 * CLAUDE.md mandates — `enhancement`, `dependencies`, `P2-medium`, `area:web` — failed
 * for carrying two type labels.
 */
export const TYPE_LABELS = [
  'bug',
  'enhancement',
  'chore',
  'refactor',
  'performance',
  'security',
  'dx',
  'documentation',
  'test',
];

/** Exactly one of these per pull request. How soon it matters, which is a call, not a fact. */
export const PRIORITY_LABELS = ['P0-critical', 'P1-high', 'P2-medium', 'P3-low'];

/**
 * Applied from the paths a pull request touches, so a path may decide them.
 *
 * The distinction that matters to `.github/labeler.yml`: these and the areas are
 * mechanical, types are editorial. The labeler may write these; it may never write a type.
 */
export const EXTRA_LABELS = ['dependencies', 'gate', 'design-system'];
