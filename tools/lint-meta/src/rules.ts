import { readFileSync, readdirSync } from 'node:fs';
import { join, posix, relative, sep } from 'node:path';

import { Minimatch } from 'minimatch';

import {
  BUNDLED_EXTENSIONS,
  STORE_CONTEXT_ALLOWED,
  TAURI_ALLOWED,
  WEBVIEW,
  describe as describeBoundaries,
  directoryPrefixes,
  matches,
  matchesAny,
} from './boundaries.ts';
import { moduleReferences } from './moduleReferences.ts';
import {
  isTauriEdge,
  isTauriPackage,
  loadCargoWorkspace,
  type CargoDependencyEdge,
  type CargoWorkspace,
} from './cargoGraph.ts';

/**
 * A broken architecture rule, located precisely enough to click on.
 */
export interface Violation {
  readonly rule: string;
  /** Repo-relative, POSIX separators, so the output is identical on both runners. */
  readonly file: string;
  /** 1-indexed; 0 when the rule is about a file as a whole. */
  readonly line: number;
  readonly message: string;
}

/** Directories that never contain source we own. */
const SKIP_DIRS = new Set([
  '.git',
  'node_modules',
  'target',
  'dist',
  'bindings',
  'gen',
  '.pnpm-store',
]);

/**
 * lint-meta's own fixtures deliberately break both rules, so the repo-wide scan skips
 * them. `scripts/prove-lint-meta.ts` points the rules straight at that directory, which is
 * what keeps the exclusion honest: if a rule stopped tripping, the proof would fail.
 */
const FIXTURE_ROOT = 'tools/lint-meta/fixtures';

/**
 * Where Tauri may be imported, as the plain path prefixes the cargo rules match manifests
 * against.
 *
 * `apps/desktop` is the shell itself. `apps/web/src/transport` is the one module in the
 * webview allowed to touch the Tauri `Channel`; every other component reaches the daemon
 * through it. Derived from `boundaries.ts`, which `eslint.config.js` reads too — the two
 * layers no longer hold separate copies that can drift (#20).
 */
export const TAURI_ALLOWLIST: readonly string[] = directoryPrefixes(TAURI_ALLOWED);

/**
 * Every extension a rule here reads: the bundled set from `boundaries.ts`, which
 * `eslint.config.js` renders into its own `files` globs, plus `.rs` for the Rust scan.
 */
const SOURCE_EXTENSIONS = [...BUNDLED_EXTENSIONS.map((e) => `.${e}`), '.rs'];

/** The carve-outs as a reader expects to see them in a message. */
const STORE_CONTEXT_ALLOWED_FOR_HUMANS = describeBoundaries(STORE_CONTEXT_ALLOWED);

/**
 * Does this specifier name the store's context module?
 *
 * Run against the *cooked* specifier the parser hands back — escapes resolved, quotes gone —
 * so the three ways JavaScript can quote a string stopped being three cases.
 *
 * The query is stripped first, and here that is right for the same reason it is wrong in a
 * glob: in a specifier `?` opens Vite's query, in a pattern it is the single-character
 * wildcard. `./StoreContext.ts?raw` used to slip past here while the ESLint pattern ending
 * in `StoreContext` plus any extension caught it, so the claim that the two had the same
 * reach was an over-claim. It now has that reach rather than a footnote saying it does not.
 *
 * Any path ending in `StoreContext`, with or without a file extension, matching the ESLint
 * globs, which deliberately do not require `store/` next to the filename — so
 * `../store/./StoreContext` and `../store//StoreContext` are caught too.
 */
function namesStoreContext(specifier: string): boolean {
  const path = specifier.split('?')[0] ?? '';
  return /(?:^|\/)StoreContext(?:\.[A-Za-z0-9]+)?$/.test(path);
}

/**
 * Can this glob pattern reach a module, from the file that wrote it?
 *
 * Asked of the matcher and the tree rather than answered by reading the pattern. The
 * previous version took the pattern apart by hand to find its extension, splitting on `?`
 * as if a glob carried a URL query — so `*.t?x` read as "pinned to extension t" and was
 * exempted, while the real matcher returns `StoreProvider.tsx`. Every such question is now
 * put to a glob matcher over the files that actually exist: resolve the pattern against the
 * importing file's directory, match it, and see whether anything that comes back is a
 * module.
 *
 * A leading `!` is stripped because Vite strips it before matching. An exclusion cannot
 * itself return a file, so a negation can only ever add a report here, never remove one —
 * which is the safe direction and the reason no attempt is made to work out which patterns
 * cancel which.
 *
 * This only ever runs when the options were read as deciding nothing — a call carrying
 * `base`, `caseSensitive`, `exhaustive` or anything else unrecognised is reported before it
 * gets here, because all of those change what a pattern reaches and none of them is
 * modelled. That is the fail-closed default doing the work, and it is what makes this
 * function's approximations tolerable rather than load-bearing.
 *
 * Because they are approximations, and an earlier version claimed otherwise. "Same matcher,
 * same options" was not true: the matcher here is `minimatch` with `dot: true` and no
 * ignore list, matching every file in the scan including the importing file itself, where
 * Vite runs `picomatch` with its own dot and extglob settings, its own ignores, and the
 * importing file excluded. Every one of those differences makes this side match MORE, so
 * each is a possible over-report and none is a missed one — which is the only direction
 * that would matter. Using Vite's own matcher would mean declaring it in the root manifest,
 * which is coordinator-owned.
 *
 * One limit in the other direction, named rather than implied: a pattern that matches
 * nothing today is not reported, because today it returns nothing. The rule runs on every
 * commit, so it reports the day a module lands under it.
 */
function globReachesModule(
  pattern: string,
  from: string,
  files: readonly string[],
): boolean {
  const positive = pattern.startsWith('!') ? pattern.slice(1) : pattern;
  // A root-relative pattern resolves against Vite's root, not this scan's — fail closed
  // rather than resolve it wrongly.
  if (positive.startsWith('/')) return true;

  let matcher;
  try {
    matcher = new Minimatch(posix.join(from, positive), { dot: true });
  } catch {
    // A pattern the matcher will not compile is one whose reach is unknown.
    return true;
  }

  return files.some(
    (candidate) =>
      SOURCE_EXTENSIONS.includes(candidate.slice(candidate.lastIndexOf('.'))) &&
      !candidate.endsWith('.rs') &&
      matcher.match(candidate),
  );
}

/**
 * The crate root, in any of the spellings a `use` path can start with.
 *
 * `tauri`, and also `tauri_plugin_opener` and friends — a leading `::` is optional because
 * `use ::tauri::X;` is the same import written defensively. A lookbehind rather than a
 * consumed delimiter, for two reasons: the match index then points at the statement itself
 * so the line number is right, and refusing a preceding word character or colon keeps
 * `crate::tauri_helpers::X` a local module rather than a false positive.
 */
const RUST_TAURI_ROOT = String.raw`(?<![\w:])(?:::\s*)?tauri(?:_[A-Za-z0-9_]+)?`;

/** A whole `use` statement, however many lines it spans. */
const RUST_USE_STATEMENT = /(?<![\w:])(?:pub(?:\s*\([^)]*\))?\s+)?use\s[^;]*;/g;
/** The crate root appearing anywhere inside such a statement. */
const RUST_USE_NAMES_TAURI = new RegExp(
  `${RUST_TAURI_ROOT}\\s*(?:::|,|;|\\}|\\s+as\\b|$)`,
);
/** `extern crate tauri;` — 2015-edition spelling, still legal. */
const RUST_EXTERN_CRATE = /(?<![\w:])extern\s+crate\s+tauri(?:_[A-Za-z0-9_]+)?\b/g;
/** A fully-qualified path used without a `use`: `tauri::Builder::default()`. */
const RUST_TAURI_PATH = new RegExp(`${RUST_TAURI_ROOT}::`, 'g');

/**
 * Blank out Rust comments and string literals, preserving every byte position.
 *
 * Bodies become spaces and newlines are kept, so a match index still maps to the right
 * line. Three things depend on this:
 *
 * - the module docs in `nysia-core` discuss tauri constantly and must not trip the rule;
 * - a `;` inside a comment would otherwise truncate the `use` statement after it;
 * - **a string literal must not be able to open a comment.** `const OPEN: &str = "/*";`
 *   would otherwise start a block comment that never closes, and every `use tauri::…`
 *   below it in the file would be silently invisible. That is a false *negative*, and the
 *   worst kind: the gate keeps reporting success.
 *
 * String literals are therefore stepped over as units, in every spelling Rust has — see
 * `STRING_OPENER` for the enumeration — plus char literals, so that `'"'` is a character
 * and not the start of a string. Their contents are blanked as well, since a `tauri::`
 * inside a string is not an import.
 */
export function blankRustComments(source: string): string {
  // `split('')`, deliberately NOT `[...source]` or `Array.from(source)`.
  //
  // Those iterate by code point, giving one slot per character, while every index this
  // function computes — `i`, `source[at]`, `startsWith`, `indexOf`, `slice` — is a UTF-16
  // code unit. One astral character (any emoji) earlier in the file would shift every
  // later blank one slot right, cumulatively, and because `blank` skips newlines the drift
  // lands on real code: the first characters after each later comment get erased instead.
  // A single emoji in a doc header would quietly disarm this rule for the whole file.
  const out = source.split('');
  let i = 0;
  let blockDepth = 0;

  const blank = (at: number): void => {
    if (out[at] !== '\n') out[at] = ' ';
  };
  const blankTo = (end: number): void => {
    for (let k = i; k < end && k < source.length; k += 1) blank(k);
    i = Math.min(end, source.length);
  };
  const isWord = (at: number): boolean => {
    const c = source[at];
    return c !== undefined && /[A-Za-z0-9_]/.test(c);
  };

  /**
   * A char literal `'x'` / `'\n'` / `'"'` / `'🔥'`, as opposed to a lifetime `'a`.
   *
   * The surrogate-pair alternative is load-bearing and comes first. `[^'\\]` matches a
   * single UTF-16 code unit, so an astral char literal was not recognised: the closing
   * quote paired with whatever followed, the orphaned quote opened a string that ran to the
   * next one in the file, and every import between them was blanked. `['🔥','"']` and
   * `matches!(c, '🔥'|'"')` both erased every tauri import below them.
   *
   * The `u` flag would express this more neatly but cannot be used here: in unicode mode
   * the `\{` of the `\u{...}` escape alternative is an invalid identity escape.
   */
  const CHAR_LITERAL =
    /^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[\uD800-\uDBFF][\uDC00-\uDFFF]|[^'\\])'/;

  /**
   * Every string-literal opener Rust has, as one table rather than a chain of `if`s — the
   * `c` prefix was missing from the chain, and its absence meant `c"/*"` opened a block
   * comment that never closed and made the rest of the file invisible.
   *
   * In:  `"…"`  `b"…"`  `c"…"`  `r"…"`  `br"…"`  `cr"…"`  `r#"…"#`  `br#"…"#`  `cr#"…"#`
   * Out: nothing else. Rust has no `rb"` or `rc"`; the prefix always precedes the `r`.
   *
   * Sticky, so it can be anchored at the current index without slicing.
   */
  const STRING_OPENER = /(?:[bc]?r(#*)|[bc]?)"/y;

  while (i < source.length) {
    if (blockDepth > 0) {
      if (source.startsWith('/*', i)) {
        blockDepth += 1;
        blankTo(i + 2);
        continue;
      }
      if (source.startsWith('*/', i)) {
        blockDepth -= 1;
        blankTo(i + 2);
        continue;
      }
      blankTo(i + 1);
      continue;
    }

    if (source.startsWith('/*', i)) {
      blockDepth = 1;
      blankTo(i + 2);
      continue;
    }
    // Covers `//`, `///` and `//!` alike.
    if (source.startsWith('//', i)) {
      const newline = source.indexOf('\n', i);
      blankTo(newline === -1 ? source.length : newline);
      continue;
    }

    // A string literal in any of its nine spellings (see STRING_OPENER).
    if (!isWord(i - 1)) {
      STRING_OPENER.lastIndex = i;
      const opener = STRING_OPENER.exec(source);
      if (opener !== null) {
        const hashes = opener[1] ?? '';
        const bodyStart = i + opener[0].length;

        if (opener[0].includes('r')) {
          // Raw: no escapes, so the terminator is the quote plus the same hash count.
          const terminator = `"${hashes}`;
          const end = source.indexOf(terminator, bodyStart);
          blankTo(end === -1 ? source.length : end + terminator.length);
          continue;
        }

        // Non-raw: a backslash escapes the next character, including a quote.
        let k = bodyStart;
        while (k < source.length) {
          if (source[k] === '\\') {
            k += 2;
            continue;
          }
          if (source[k] === '"') {
            k += 1;
            break;
          }
          k += 1;
        }
        blankTo(k);
        continue;
      }
    }

    if (source[i] === "'") {
      const char = CHAR_LITERAL.exec(source.slice(i, i + 12));
      if (char !== null) {
        blankTo(i + char[0].length);
        continue;
      }
      // A lifetime: nothing to blank.
      i += 1;
      continue;
    }

    i += 1;
  }

  const blanked = out.join('');
  // The whole scanner is offset arithmetic, so this invariant is the thing that must never
  // quietly break. A rule that silently stops scanning is worse than no rule: it keeps
  // reporting success. If a future edit reintroduces code-point iteration, crash here
  // rather than start erasing code.
  if (blanked.length !== source.length) {
    throw new Error(
      `blankRustComments changed the length of its input (${source.length} -> ` +
        `${blanked.length}); every index in this function must be a UTF-16 code unit`,
    );
  }
  return blanked;
}

/** 1-indexed line of a byte offset. */
function lineAt(source: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index; i += 1) {
    if (source[i] === '\n') line += 1;
  }
  return line;
}

/**
 * Every line in a Rust file that pulls tauri in.
 *
 * Scanned per *statement* rather than per line, because a `use` can span lines and the
 * previous line-anchored regex walked straight past three legal spellings:
 * `use ::tauri::Builder;`, `use {tauri, serde};`, and the multi-line form of the second.
 * Any of them would have put the UI toolkit in the daemon with no gate tripping.
 */
function rustTauriLines(source: string): number[] {
  const code = blankRustComments(source);
  const lines = new Set<number>();

  for (const match of code.matchAll(RUST_USE_STATEMENT)) {
    if (match.index === undefined) continue;
    if (RUST_USE_NAMES_TAURI.test(match[0])) lines.add(lineAt(code, match.index));
  }
  for (const pattern of [RUST_EXTERN_CRATE, RUST_TAURI_PATH]) {
    for (const match of code.matchAll(pattern)) {
      if (match.index !== undefined) lines.add(lineAt(code, match.index));
    }
  }

  return [...lines].sort((a, b) => a - b);
}

function toPosix(path: string): string {
  return path.split(sep).join(posix.sep);
}

/** Every file under `root`, repo-relative and POSIX-separated, fixtures excluded. */
export function walk(root: string, includeFixtures = false): string[] {
  const found: string[] = [];
  const visit = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const absolute = join(dir, entry.name);
      const rel = toPosix(relative(root, absolute));
      if (entry.isDirectory()) {
        if (SKIP_DIRS.has(entry.name)) continue;
        if (!includeFixtures && rel === FIXTURE_ROOT) continue;
        visit(absolute);
      } else if (entry.isFile()) {
        found.push(rel);
      }
    }
  };
  visit(root);
  return found.sort();
}

function read(root: string, file: string): string {
  return readFileSync(join(root, file), 'utf8');
}

/**
 * Rule (a): nothing outside the Tauri allowlist may import Tauri.
 *
 * The window is a client with no privileged path (D-1, D-2). A component that reaches for
 * `@tauri-apps/api` directly, or a crate that links `tauri`, has quietly made the runtime
 * depend on the UI toolkit — which is the one thing the architecture is built to prevent.
 *
 * Rust is scanned as text; TypeScript and JavaScript are parsed. The line scan this replaced
 * had to anchor its patterns at the start of a line, because an unanchored one also fired on
 * the sample imports inside `scripts/prove-eslint-bans.ts` where they are test input — and
 * an anchor is a guess about layout. A tree removes the reason for the guess: those samples
 * are string literals, and a string is not an import however it reads.
 */
export function noTauriOutsideDesktop(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];
  const allowed = (file: string): boolean => matchesAny(file, TAURI_ALLOWED);

  const report = (file: string, line: number): void => {
    violations.push({
      rule: 'no-tauri-outside-desktop',
      file,
      line,
      message: `imports tauri; only ${TAURI_ALLOWLIST.join(' and ')} may do that (D-1, D-2)`,
    });
  };

  for (const file of files) {
    if (allowed(file)) continue;
    const extension = file.slice(file.lastIndexOf('.'));
    if (!SOURCE_EXTENSIONS.includes(extension)) continue;

    const source = read(root, file);

    if (extension === '.rs') {
      // Rust is scanned whole: comments blanked, then statement by statement, so a `use`
      // that spans lines is one unit rather than three lines none of which match.
      for (const line of rustTauriLines(source)) report(file, line);
      continue;
    }

    // TypeScript and JavaScript are parsed. This used to be a line scan anchored at the
    // start of the line, for one stated reason: an unanchored match also fired on the
    // sample imports inside `scripts/prove-eslint-bans.ts`, where they are test input. A
    // tree removes the reason rather than working around it — those samples are
    // `StringLiteral` nodes, and a string is not an import however it reads.
    for (const reference of moduleReferences(file, source)) {
      if (reference.kind === 'glob') continue;
      if (reference.specifier?.startsWith('@tauri-apps') === true) report(file, reference.line);
    }
  }
  return violations;
}

/**
 * Rule (d): nothing in the webview may reach `StoreContext` through a call.
 *
 * The other half of the boundary ESLint enforces on `import` statements. `useCommands()`
 * hands components verbs that return `void` so there is no promise left to drop; that is
 * only true while the provider itself is out of reach, and a dynamic `import()` reached it
 * without tripping anything.
 *
 * Scoped to `apps/web/` because that is the scope of the ESLint ban it completes, with the
 * same two carve-outs.
 *
 * Two questions, and neither is answered by reading text any more. What the file reaches
 * comes from a syntax tree — see `moduleReferences.ts` for why, which is three rounds of a
 * hand-written scanner finding a new spelling each time. What a glob pattern reaches comes
 * from a glob matcher run over the files that exist. Both are the same lesson the labeler
 * checker learned when it was replaced by a simulation of the real action, and the daemon
 * when it was only trusted once a test drove both real halves: model nothing you can run.
 *
 * **THE DEFAULT IS CLOSED, AND THAT IS THE POINT OF THIS RULE.** A glob call is reported
 * unless it is *provably* incapable of handing back a module, where the proof is a very
 * short list of forms executed against the pinned Vite — not a list of the ways a call might
 * be dangerous. The earlier version had it the other way round: it enumerated the safe
 * spellings and exempted them, so every option nobody had thought of failed OPEN. That is
 * unbounded, because Vite's option surface is Vite's to change and this rule is guessing at
 * it from outside; three separate options and three expression wrappers walked through it
 * before anyone noticed. The worst an unfamiliar option can do now is produce a report, and
 * a report is a developer writing one line to silence it with a reason. The other direction
 * is the store provider in the production bundle with every gate green. See
 * `GlobOptionsVerdict` for the list and what is deliberately left off it.
 *
 * WHAT REMAINS BEYOND IT, stated rather than implied by silence: a specifier that is not a
 * literal. `import(name)` and `import('../store/' + name)` are reported as computed by the
 * parser but cannot be resolved by it, and ESLint's `no-restricted-imports` is equally blind
 * to the static equivalents. That is why the store keeps the provider off its public surface
 * rather than relying on this rule alone.
 */
export function noStoreContextOutsideStore(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];

  for (const file of files) {
    if (!matches(file, WEBVIEW)) continue;
    if (matchesAny(file, STORE_CONTEXT_ALLOWED)) continue;

    const extension = file.slice(file.lastIndexOf('.'));
    if (!SOURCE_EXTENSIONS.includes(extension) || extension === '.rs') continue;

    const report = (line: number, message: string): void => {
      violations.push({ rule: 'no-store-context-outside-store', file, line, message });
    };

    for (const reference of moduleReferences(file, read(root, file))) {
      if (reference.kind === 'import' || reference.kind === 'export') continue;

      if (reference.kind !== 'glob') {
        if (reference.specifier === undefined) continue;
        if (!namesStoreContext(reference.specifier)) continue;
        report(
          reference.line,
          'reaches StoreContext through a call; components use useCommands() from ' +
            `store/hooks. Only ${STORE_CONTEXT_ALLOWED_FOR_HUMANS} may touch the provider`,
        );
        continue;
      }

      // The default is closed. A glob is reported unless the call is *provably* incapable of
      // handing back a module, and the only proofs accepted are the ones executed against
      // the pinned Vite — see `GlobOptionsVerdict`.
      if (reference.options.kind === 'source-text') continue;

      if (reference.options.kind === 'undecidable') {
        report(
          reference.line,
          `glob-imports with ${reference.options.because}, which can change what it ` +
            'reaches or what it returns; this rule reports what it cannot prove harmless, ' +
            `because only ${STORE_CONTEXT_ALLOWED_FOR_HUMANS} may touch the store provider. ` +
            "Read the files as text with query: '?raw', or silence this with a reason",
        );
        continue;
      }

      const from = posix.dirname(file);
      // Nothing in the options moves the pattern, so the tree can answer. A pattern that is
      // not a literal says nothing about what it can return and is reported; one that
      // reaches a module is enough to report the call.
      const reaches = reference.patterns.some(
        (pattern) => pattern === undefined || globReachesModule(pattern, from, files),
      );
      if (!reaches) continue;

      report(
        reference.line,
        'glob-imports modules it does not name, so it may hand back the store provider; ' +
          `only ${STORE_CONTEXT_ALLOWED_FOR_HUMANS} may touch it. Read the files as text ` +
          "with query: '?raw', or write a pattern that reaches no module",
      );
    }
  }

  return violations;
}

/** Which crate a dependency is, spelled for a message — `ui (package = "tauri")` renamed. */
function describe(edge: CargoDependencyEdge): string {
  return edge.rename === null
    ? `\`${edge.name}\``
    : `\`${edge.rename}\` (package = "${edge.name}")`;
}

/**
 * Rule (b): no Rust crate outside `apps/desktop` may depend on tauri.
 *
 * This is the load-bearing rule of the three. If no crate outside `apps/desktop` depends on
 * tauri then `use tauri::…` in those crates cannot compile, which makes the Rust text scan
 * belt and braces rather than the guard. It is kept because it reports a file and a line
 * that a developer can act on in ten seconds, where this rule reports a manifest.
 *
 * Every dependency kind counts — normal, dev and build alike. A runtime crate has no reason
 * to link the UI toolkit even in a test. So does either half of a rename: see `isTauriEdge`.
 */
export function noTauriInRustCrates(workspace: CargoWorkspace): Violation[] {
  const violations: Violation[] = [];

  for (const member of workspace.members) {
    if (TAURI_ALLOWLIST.some((prefix) => member.manifestPath.startsWith(prefix))) continue;
    for (const edge of member.dependencies) {
      if (!isTauriEdge(edge)) continue;
      violations.push({
        rule: 'no-tauri-in-rust-crates',
        file: member.manifestPath,
        line: 0,
        message: `declares ${describe(edge)}; only apps/desktop may link the UI toolkit (D-1)`,
      });
    }
  }

  return violations;
}

/**
 * Rule (c): no crate outside `apps/desktop` may *reach* tauri through anything.
 *
 * Rule (b) names the crate that declares the dependency. This one says what it costs, by
 * walking the resolved graph and reporting the shortest chain — so the reviewer of a
 * `nysia-proto` change can see that it has just put the UI toolkit inside the runtime.
 *
 * Every non-allowlisted member is a seed, not `nysia-core` alone. Seeding from core only
 * left the daemon binary able to reach tauri through `nysia-desktop`, or through any
 * non-member crate, with nothing tripping: rule (b) sees only direct declarations, and this
 * rule was not looking. That is the same hole round one found, one hop further out.
 *
 * A chain through an allowlisted crate still counts. `apps/desktop` may link the UI
 * toolkit; a crate that depends on `apps/desktop` has linked it too.
 */
export function noTauriReachingRustCrates(workspace: CargoWorkspace): Violation[] {
  const violations: Violation[] = [];

  for (const seed of workspace.members) {
    if (TAURI_ALLOWLIST.some((prefix) => seed.manifestPath.startsWith(prefix))) continue;

    const chain = shortestChainToTauri(workspace, seed.id);
    // `[seed, tauri]` is a direct declaration, which is rule (b)'s to report. This rule
    // exists for everything longer.
    if (chain === undefined || chain.length < 3) continue;

    const names = chain.map((id) => workspace.packages.get(id)?.name ?? id).join(' -> ');
    violations.push({
      rule: 'no-tauri-reaching-rust-crates',
      file: seed.manifestPath,
      line: 0,
      message: `${names}; a crate outside apps/desktop must not link the UI toolkit (D-1)`,
    });
  }

  return violations;
}

/**
 * The shortest path from `seedId` to any tauri package, or `undefined` if there is none.
 *
 * Breadth-first, and with its own `seen` set per seed. Sharing one set across seeds would
 * be faster and wrong: a tauri package first reached by a crate that declares it directly
 * would be marked seen, and the crate that reaches the same package through two hops would
 * then be passed over in silence.
 */
function shortestChainToTauri(
  workspace: CargoWorkspace,
  seedId: string,
): readonly string[] | undefined {
  const queue: string[][] = [[seedId]];
  const seen = new Set<string>([seedId]);

  while (queue.length > 0) {
    const chain = queue.shift();
    if (chain === undefined) break;
    const tail = chain[chain.length - 1];
    if (tail === undefined) continue;

    for (const nextId of workspace.edges.get(tail) ?? []) {
      if (seen.has(nextId)) continue;
      seen.add(nextId);
      const next = workspace.packages.get(nextId);
      if (next === undefined) continue;

      if (isTauriPackage(next.name)) return [...chain, nextId];
      queue.push([...chain, nextId]);
    }
  }

  return undefined;
}

/**
 * The rules that read source text: rule (a), over the tree at `root`.
 *
 * Separate from the cargo rules because it needs no workspace — it runs against the fixture
 * trees, which are deliberately not buildable crates.
 */
export function runSourceRules(root: string, includeFixtures = false): Violation[] {
  const files = walk(root, includeFixtures);
  return [...noTauriOutsideDesktop(root, files), ...noStoreContextOutsideStore(root, files)];
}

/**
 * Rule (b), continued: every crate on disk must be one cargo knows about.
 *
 * Reading the dependency graph instead of every `Cargo.toml` on disk is what makes rules
 * (b) and (c) trustworthy, but it narrows them in one way: a crate directory that is not a
 * workspace member never appears in `cargo metadata`, so nothing inspects it. Today such a
 * crate compiles into nothing, which is why it is a small risk — but "the rule stopped
 * looking and said nothing" is the failure class this tool exists to remove, so a manifest
 * cargo does not account for is reported rather than skipped.
 */
export function noUnknownCrates(
  files: readonly string[],
  workspace: CargoWorkspace,
): Violation[] {
  const known = new Set(workspace.members.map((m) => m.manifestPath));
  // The workspace root manifest may be virtual — no `[package]`, so never a member.
  known.add('Cargo.toml');

  return files
    .filter((file) => file === 'Cargo.toml' || file.endsWith('/Cargo.toml'))
    .filter((file) => !known.has(file))
    .filter((file) => !TAURI_ALLOWLIST.some((prefix) => file.startsWith(prefix)))
    .map((file) => ({
      rule: 'no-tauri-in-rust-crates',
      file,
      line: 0,
      message:
        'is not a workspace member, so cargo never resolves it and the dependency rules ' +
        'cannot see inside it; add it to [workspace] members or delete it',
    }));
}

/**
 * The rules that read cargo's resolved dependency graph: rules (b) and (c).
 *
 * @throws {CargoMetadataError} if cargo cannot describe the workspace. Deliberately not
 * caught here: a dependency rule that could not run must not report zero violations.
 */
export function runCargoRules(root: string, includeFixtures = false): Violation[] {
  const workspace = loadCargoWorkspace(root);
  return [
    ...noTauriInRustCrates(workspace),
    ...noTauriReachingRustCrates(workspace),
    ...noUnknownCrates(walk(root, includeFixtures), workspace),
  ];
}

/**
 * Every architecture rule, for a tree that is a cargo workspace.
 *
 * @throws {CargoMetadataError} see {@link runCargoRules}.
 */
export function runAllRules(root: string, includeFixtures = false): Violation[] {
  return [
    ...runSourceRules(root, includeFixtures),
    ...runCargoRules(root, includeFixtures),
  ];
}
