/**
 * The architecture boundaries, written once and read by both lint layers.
 *
 * ESLint gives the error at the import site and sees an AST; lint-meta covers `require()`,
 * dynamic `import()`, Rust, and every file ESLint's `ignores` excludes. Two layers is the
 * design. Two *lists* was the defect: `eslint.config.js` and `rules.ts` each carried their
 * own copy of the same allowlist, described as mirrored so they could not disagree silently,
 * with nothing cross-checking them — and they already disagreed. lint-meta allowlisted the
 * store carve-out by the prefix `apps/web/src/main.` while ESLint carved out
 * `main.{ts,tsx,…}`, so `apps/web/src/main.helper.tsx` was inside one allowlist and outside
 * the other and a dynamic import of the provider from it tripped neither (#20).
 *
 * So this module is the list, and each layer derives its own spelling of it:
 *
 * - `eslintFiles()` renders a boundary as the glob a flat-config `files` entry wants;
 * - `matches()` answers the same question for a repo-relative path, which is what the
 *   lint-meta rules walk;
 * - `directoryPrefixes()` renders the directory boundaries as plain path prefixes, which is
 *   what the cargo rules match manifests against.
 *
 * `scripts/prove-eslint-bans.ts` lints a set of edge paths through *both* layers and fails if
 * they give different answers, so the single source stays a single source rather than
 * becoming a comment claiming to be one.
 *
 * This file is imported by `eslint.config.js`, which is plain JavaScript: Node strips the
 * types on the way in, so nothing here may use syntax that is not erasable — no `enum`, no
 * `namespace`, no parameter properties.
 */

/**
 * Every extension that can carry an import into the bundle, enumerated deliberately rather
 * than grown one at a time.
 *
 * In:  ts tsx mts cts js jsx mjs cjs
 * Out: json, css, html, svg — none of them can import anything.
 *
 * `.mts` and `.cts` are in the list because Vite 8's default `resolve.extensions` includes
 * `.mts`, so such a file bundles; leaving them out left a file covered by neither layer.
 */
export const BUNDLED_EXTENSIONS: readonly string[] = [
  'ts',
  'tsx',
  'mts',
  'cts',
  'js',
  'jsx',
  'mjs',
  'cjs',
];

/** The brace list a flat-config glob wants: `{ts,tsx,…}`. */
export const BUNDLED_EXTENSION_GLOB = `{${BUNDLED_EXTENSIONS.join(',')}}`;

/**
 * A place in the tree a rule may carve out.
 *
 * `directory` is everything beneath a directory. `file` is one file, named without its
 * extension, in whichever bundled extension it carries — which is the shape the webview
 * entry point needs and the shape a prefix cannot express. The distinction is the whole
 * point of this module: `apps/web/src/main` as a `file` covers `main.tsx` and `main.mts` and
 * stops there, where the prefix `apps/web/src/main.` also swallowed `main.helper.tsx`.
 */
export type Boundary =
  | { readonly kind: 'directory'; readonly path: string }
  | { readonly kind: 'file'; readonly path: string };

/** The Tauri shell. Tauri is its whole job. */
export const DESKTOP: Boundary = { kind: 'directory', path: 'apps/desktop' };

/** The browser bundle, as a whole. */
export const WEBVIEW: Boundary = { kind: 'directory', path: 'apps/web' };

/**
 * The one module in the webview allowed to hold a Tauri `Channel` (D-1, D-2). Every other
 * component reaches the daemon through it.
 */
export const TRANSPORT: Boundary = { kind: 'directory', path: 'apps/web/src/transport' };

/** The store module itself, which owns the provider. */
export const STORE: Boundary = { kind: 'directory', path: 'apps/web/src/store' };

/**
 * The entry point that composes the provider — the one line wave 2 changes when the mock
 * store becomes the daemon-backed one.
 *
 * A `file` and not a directory or a prefix. `apps/web/src/main/index.ts` is not the entry
 * point and neither is `apps/web/src/main.helper.tsx`; both are ordinary webview files and
 * both layers must say so.
 */
export const WEB_ENTRY: Boundary = { kind: 'file', path: 'apps/web/src/main' };

/**
 * Where Tauri may be imported.
 *
 * Read by ESLint's `no-restricted-imports` carve-out and by lint-meta's rules (a), (b) and
 * (c). The Rust half matches manifest paths against the directory prefixes, which is why
 * only directories belong here.
 */
export const TAURI_ALLOWED: readonly Boundary[] = [DESKTOP, TRANSPORT];

/**
 * The agent module: the only place Claude's specifics may be named (D-3, D-4).
 *
 * Rust-only, so ESLint never reads this one — it has no `import` to restrict. It lives here
 * anyway because this module is where a boundary is written down, and a second list kept
 * somewhere else is the defect this file exists to have removed (#20).
 */
export const AGENT: Boundary = { kind: 'directory', path: 'crates/nysia-core/src/agent' };

/**
 * The Claude module inside it: the specifics themselves.
 *
 * `agent/**` may name these; nothing else may, and nothing in `agent/**` outside this
 * directory may re-export them back out under a neutral name.
 */
export const AGENT_CLAUDE: Boundary = {
  kind: 'directory',
  path: 'crates/nysia-core/src/agent/claude',
};

/**
 * Where the raw store provider may be reached.
 *
 * Two carve-outs and only two: the store module itself, and the entry point that composes
 * the provider. Everything else goes through `useCommands()`, whose verbs return `void` so
 * there is no promise left for a call site to drop.
 */
export const STORE_CONTEXT_ALLOWED: readonly Boundary[] = [STORE, WEB_ENTRY];

/** The flat-config `files` glob for one boundary. */
export function eslintFile(boundary: Boundary): string {
  return boundary.kind === 'directory'
    ? `${boundary.path}/**/*.${BUNDLED_EXTENSION_GLOB}`
    : `${boundary.path}.${BUNDLED_EXTENSION_GLOB}`;
}

/** The flat-config `files` array for a set of boundaries. */
export function eslintFiles(boundaries: readonly Boundary[]): string[] {
  return boundaries.map(eslintFile);
}

/**
 * Is this repo-relative, POSIX-separated path inside the boundary?
 *
 * A `file` boundary matches the named file in any bundled extension and nothing else, which
 * is exactly what {@link eslintFile} renders. A `directory` boundary matches the directory
 * and everything beneath it, at any extension — the Rust scan and the cargo rules need that.
 */
export function matches(file: string, boundary: Boundary): boolean {
  if (boundary.kind === 'directory') {
    return file === boundary.path || file.startsWith(`${boundary.path}/`);
  }
  return BUNDLED_EXTENSIONS.some((extension) => file === `${boundary.path}.${extension}`);
}

/** Is this path inside any of them? */
export function matchesAny(file: string, boundaries: readonly Boundary[]): boolean {
  return boundaries.some((boundary) => matches(file, boundary));
}

/**
 * The directory boundaries as plain path prefixes, trailing slash included.
 *
 * For the cargo rules, which match manifest paths and know nothing about extensions. A
 * `file` boundary has no prefix form and is dropped rather than approximated — approximating
 * it with `path + '.'` is precisely the defect this module exists to remove.
 */
export function directoryPrefixes(boundaries: readonly Boundary[]): string[] {
  return boundaries
    .filter((boundary) => boundary.kind === 'directory')
    .map((boundary) => `${boundary.path}/`);
}

/** The boundaries as a reader expects to see them in an error message. */
export function describe(boundaries: readonly Boundary[]): string {
  return boundaries
    .map((boundary) => (boundary.kind === 'directory' ? `${boundary.path}/**` : `${boundary.path}.*`))
    .join(' and ');
}
