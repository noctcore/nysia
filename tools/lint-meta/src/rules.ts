import { readFileSync, readdirSync } from 'node:fs';
import { join, posix, relative, sep } from 'node:path';

import { Minimatch } from 'minimatch';

import {
  AGENT,
  AGENT_CLAUDE,
  BUNDLED_EXTENSIONS,
  STORE_CONTEXT_ALLOWED,
  TAURI_ALLOWED,
  WEBVIEW,
  describe as describeBoundaries,
  directoryPrefixes,
  matches,
  matchesAny,
} from './boundaries.ts';
import {
  callSites,
  importedValues,
  moduleReferences,
  reExports,
} from './moduleReferences.ts';
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
 * Because they are approximations, and two earlier versions claimed otherwise. "Same
 * matcher, same options" was not true: the matcher here is `minimatch` with `dot: true` and
 * no ignore list, matching every file in the scan including the importing file itself, where
 * Vite runs `picomatch` with its own dot and extglob settings, its own ignores, and the
 * importing file excluded.
 *
 * The replacement — that every one of those differences makes this side match MORE, so none
 * could be a missed one — was also wrong, and wrong in the dangerous direction. Anchoring is
 * a difference too, and it narrows: joining an unanchorable pattern onto the importing
 * directory made this side match LESS than Vite, and a double-star glob reaching the store
 * was allowed through. So the honest statement is the guard above, not a claim about the
 * matcher: what this function compares is only ever a pattern it has already established it
 * can anchor. Using Vite's own matcher would mean declaring it in the root manifest, which
 * is coordinator-owned.
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
  // Anchoring is the whole of this function, so a pattern that cannot be anchored cannot be
  // judged here. Only a relative pattern resolves against the importing file's directory.
  // `/x` resolves against Vite's root; a pattern starting `**` is handed to the globber
  // untouched and walked from the filesystem root; an alias or a subpath import goes through
  // the resolver first. Joining any of those onto the importing directory does not merely
  // get the answer wrong, it gets it wrong in the direction that stays silent — it anchors a
  // pattern Vite left loose, so the rule looks in one folder while the glob walks the disk.
  if (!positive.startsWith('./') && !positive.startsWith('../')) return true;

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
 * it from outside; three separate options, three expression wrappers and an unanchorable
 * pattern walked through it before anyone noticed. The worst an unfamiliar option can do now
 * is produce a report; the other direction is the store provider in the production bundle
 * with every gate green. See `GlobOptionsVerdict` for the list and what is deliberately left
 * off it — including the reason an over-report costs more here than it would in ESLint,
 * since lint-meta has no way to suppress one in place.
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
            "Read the files as text with query: '?raw', or reach the modules some other way",
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

/** The package a `Terminal` comes from. Its stylesheet is not one. */
const XTERM_PACKAGE = '@xterm/xterm';

/** The call that displaces every responder on a terminal's parser. */
const MUTE_CALL = 'muteTerminalReplies';

/** Whether a specifier hands the importer something that can construct a terminal. */
function buildsATerminal(specifier: string): boolean {
  if (specifier === XTERM_PACKAGE) return true;
  if (!specifier.startsWith(`${XTERM_PACKAGE}/`)) return false;
  // `@xterm/xterm/css/xterm.css` is the stylesheet, and a stylesheet constructs nothing.
  // Every other subpath is a build of the library, `lib/xterm.mjs` included.
  return !specifier.endsWith('.css');
}

/** The class a renderer constructs. Binding it as a value is what makes a module one. */
const TERMINAL_CLASS = 'Terminal';

/** Whether a name bound from a build of the library is the terminal class itself. */
function isTheTerminalClass(imported: string): boolean {
  // `*` is the namespace, which carries `Terminal` along with everything else. `default` is
  // conservative: no build of this library has one today, and one that grew a default export
  // would rather be reported wrongly than be missed.
  return imported === TERMINAL_CLASS || imported === '*' || imported === 'default';
}

/**
 * Rule (e): a module that builds a terminal must mute the replies it would otherwise send.
 *
 * D-7 puts terminal state in Rust and makes the webview a display cache. The daemon's virtual
 * terminal answers every query the child writes — it is built with a reply sink and the pump
 * drains it into the pty — so a renderer that answers the same query a second time is not
 * adding a service, it is typing into the child on the user's behalf. On Windows ConPTY reads
 * the end of a cursor-position report as F3, which is `cmd`'s recall-previous-command, and
 * that is §12 q7: a pane that came back after a relaunch with a command nobody typed sitting
 * in the line editor.
 *
 * `apps/web/src/transport/surface/muteReplies.ts` closes it by registering a handler ahead of
 * every built-in responder, and one line in `surface/xterm.ts` applies it. **That line is what
 * this rule exists for.** Deleting it left `pnpm test` entirely green: `xterm.ts` needs a DOM
 * and a canvas to construct, v0.1's tests are node-only (D-18), and so the one module that
 * builds the real terminal is the one module nothing executes. The mute table itself is
 * covered — deleting a line from it reddens two tests — but the call site that makes the table
 * matter was guarded by nothing, which is the shape traps register #12 is about.
 *
 * So the invariant is written as an architecture fact rather than as a pin on one file: **a
 * module that binds the `Terminal` class from the terminal library must call the mute.** That
 * survives a second such module, which is the case a pin would have missed.
 *
 * `Terminal`, and as a **value**, and that scoping is about **static imports**, where the name
 * a module binds is written down. It is there because the first spelling of this rule asked
 * only whether a module reached the library at all, and obliged two kinds of module that build
 * nothing:
 *
 * - `import type { Terminal } from '@xterm/xterm'` and `import { type Terminal }`. Both erase;
 *   neither can construct anything. Type-ness lives at the clause and at each specifier, and
 *   {@link importedValues} reads both, so neither form is a binding here.
 * - A module that imports some other export — `import { EscapeSequenceParser } from
 *   '@xterm/xterm/src/common/parser/EscapeSequenceParser'`, which is a value, and constructed.
 *   That is not hypothetical: it is the executed proof of this parser's handler ordering,
 *   considered for this PR and dropped for unrelated reasons. Had it shipped, a rule with no
 *   suppression mechanism would have reported a script that builds no terminal, and there
 *   would have been no honest way out.
 *
 * A dynamic `import()` and a `require()` are obliged on the reach alone, because what they
 * hand over is the whole module and which name comes out of it is a data-flow question rather
 * than a syntactic one. That fails closed: a lazily loaded renderer is an ordinary way to split
 * a bundle, and a terminal that arrives late answers queries exactly like one that does not.
 * Reaching for the parser alone *through* a dynamic import would be reported, but the form that
 * was actually written is the static one, and the static one is exact.
 *
 * What it cannot see, said rather than implied.
 *
 * **It checks that a call by that name exists, not that it ran.** A locally defined stub named
 * `muteTerminalReplies`, a call sitting in dead code, and a second unmuted `Terminal` in a
 * module that mutes its first one all satisfy this rule. Reaching past that means deciding
 * which code runs, which a scanner cannot do; `muteReplies.test.ts` is what covers the call
 * actually taking effect, and this rule covers the call existing at all.
 *
 * **A call reached through an alias or a computed member** is the boundary every rule in this
 * file has — a parser can say a name is computed but not what it computes to. Note which way
 * that fails: an aliased import (`import { Terminal as T }`) is still bound from the library
 * under the name the library exports, so it is still obliged; indirection hides the mute, not
 * the terminal, and hiding the mute reports.
 *
 * **A re-export launders the specifier**, so it is reported outright rather than obliged:
 * whoever imports `Terminal` from a module that re-exported it names *that* module, and this
 * rule, which matches on specifiers, stops seeing the library at all.
 *
 * **A terminal class reached under another name** is the one thing this narrowing gives up.
 * `Terminal` is the library's public facade; `CoreBrowserTerminal` behind it would build the
 * same thing, and a deep import of that is now silent where the first spelling would have
 * caught it. Traded knowingly: nothing in this repository imports past the facade, the cost of
 * being wrong the other way was a report with no way out, and widening this back to every
 * exported name whose spelling ends in `Terminal` is a one-line change if a second facade ever
 * appears. Nothing else the first spelling closed is open — an alias, an indirection through a
 * variable, a wrapper that passes the class on, a bare re-export, a dynamic import and a
 * `require` all still report.
 *
 * **A renderer that is not `@xterm/xterm`** is real and deliberate: §7.3 keeps `ghostty-web`
 * swappable, and when it arrives this rule needs its package name added rather than being
 * quietly correct about a library nobody uses any more.
 */
export function noUnmutedRenderer(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];

  for (const file of files) {
    const extension = file.slice(file.lastIndexOf('.'));
    if (!SOURCE_EXTENSIONS.includes(extension) || extension === '.rs') continue;

    const source = read(root, file);

    const handedOn = reExports(file, source).filter(
      (reExport) =>
        buildsATerminal(reExport.specifier) && reExport.names.some(isTheTerminalClass),
    );
    if (handedOn.length > 0) {
      violations.push({
        rule: 'renderer-must-mute-replies',
        file,
        line: handedOn[0]?.line ?? 0,
        message:
          `re-exports ${TERMINAL_CLASS} straight from ${XTERM_PACKAGE}; whoever builds one ` +
          'then names this module rather than the library, which is where this rule stops ' +
          `seeing it — import it here, ${MUTE_CALL}() it, and hand on the muted one`,
      });
      continue;
    }

    const bound = importedValues(file, source).filter(
      (value) => buildsATerminal(value.specifier) && isTheTerminalClass(value.imported),
    );
    const lazily = moduleReferences(file, source).filter(
      (reference) =>
        (reference.kind === 'dynamic-import' || reference.kind === 'require') &&
        reference.specifier !== undefined &&
        buildsATerminal(reference.specifier),
    );
    if (bound.length === 0 && lazily.length === 0) continue;
    if (callSites(file, source, MUTE_CALL).length > 0) continue;

    if (bound.length > 0) {
      violations.push({
        rule: 'renderer-must-mute-replies',
        file,
        line: bound[0]?.line ?? 0,
        message:
          `binds ${TERMINAL_CLASS} from ${XTERM_PACKAGE} but never calls ${MUTE_CALL}(); a ` +
          'terminal answers the queries it parses and the daemon has already answered them, ' +
          'so an unmuted one types into the child as though somebody had (D-7, §12 q7)',
      });
      continue;
    }

    violations.push({
      rule: 'renderer-must-mute-replies',
      file,
      line: lazily[0]?.line ?? 0,
      message:
        `loads ${XTERM_PACKAGE} at runtime and never calls ${MUTE_CALL}(); a dynamic import ` +
        'hands over the whole module, so this is obliged on the reach alone — a terminal that ' +
        'arrives lazily answers the queries it parses like any other (D-7, §12 q7)',
    });
  }

  return violations;
}

/** The module segment that holds Claude's specifics. */
const CLAUDE_SEGMENT = 'claude';

/** The module segment that is allowed to name it. */
const AGENT_SEGMENT = 'agent';

/** Rule (f)'s name, written once because the proof asserts on it. */
const CLAUDE_RULE = 'no-claude-specifics-outside-agent';

/**
 * A fully-qualified `agent::claude` path used without a `use` statement.
 *
 * The lookbehind excludes an identifier character and **not** a colon, which is the
 * difference between this and the tauri patterns above. `tauri` has to be a crate root, so
 * one preceded by `::` is somebody else's module; `agent::claude` is always in the middle of
 * a path — `crate::agent::claude`, `nysia_core::agent::claude` — so excluding a preceding
 * colon here matched nothing at all. The fixture caught that; nothing else would have,
 * because the use-tree half was reporting the same files for its own reasons.
 */
const RUST_AGENT_CLAUDE_PATH = new RegExp(
  `(?<![A-Za-z0-9_])${AGENT_SEGMENT}\\s*::\\s*${CLAUDE_SEGMENT}(?![A-Za-z0-9_])`,
  'g',
);

/** `pub mod claude;` in any visibility spelling — `pub`, `pub(crate)`, `pub(super)`. */
const RUST_PUBLISHED_CLAUDE_MOD = new RegExp(
  `(?<![\\w:])pub(?:\\s*\\([^)]*\\))?\\s+mod\\s+${CLAUDE_SEGMENT}(?![\\w])`,
  'g',
);

/** One leaf of an expanded use-tree: where it points, and what it is called here. */
interface UseLeaf {
  /** The full path, whitespace stripped and any `as` alias dropped. */
  readonly path: string;
  /** The name it binds in this file — the alias if there was one, else the last segment. */
  readonly local: string;
}

/** One path a `use` statement actually names, after its tree has been expanded. */
interface UsePath extends UseLeaf {
  /** 1-indexed line of the statement it came from. */
  readonly line: number;
  /** The visibility it is re-exported at, or `undefined` for a plain `use`. */
  readonly visibility: string | undefined;
}

/** The index of the brace closing the one at `open`. */
function matchingBrace(text: string, open: number): number {
  let depth = 0;
  for (let at = open; at < text.length; at += 1) {
    if (text[at] === '{') depth += 1;
    else if (text[at] === '}') {
      depth -= 1;
      if (depth === 0) return at;
    }
  }
  return -1;
}

/** Split on the commas that are not inside a nested group. */
function splitTopLevel(text: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let at = 0; at < text.length; at += 1) {
    if (text[at] === '{') depth += 1;
    else if (text[at] === '}') depth -= 1;
    else if (text[at] === ',' && depth === 0) {
      parts.push(text.slice(start, at));
      start = at + 1;
    }
  }
  parts.push(text.slice(start));
  return parts;
}

/**
 * Expand a use-tree into every path it names.
 *
 * `crate::agent::{claude::{a, b}, other}` is three paths, and only two of them are about
 * Claude. A rule that matched the statement text instead would report the third, and one
 * that looked for the literal `agent::claude` would report none of them — the grouped form
 * never spells those two segments next to each other. Rule (a) learned the same thing about
 * `use {tauri, serde};`, which is the shape a line-anchored regex walks straight past.
 */
function expandUseTree(tree: string): UseLeaf[] {
  const text = tree.trim();
  if (text === '') return [];

  const open = text.indexOf('{');
  if (open === -1) {
    // A leaf, with its local name kept rather than thrown away. Both halves are needed now:
    // the path, to see where it points, and the name, because a later `pub use` in the same
    // file can re-export through it.
    const alias = /\s+as\s+(r#)?([A-Za-z_][A-Za-z0-9_]*)$/.exec(text);
    const path = text.replace(/\s+as\s+(?:r#)?[A-Za-z_][A-Za-z0-9_]*$/, '').replace(/\s+/g, '');
    const last = path.split('::').pop() ?? '';
    // `use crate::agent::{self}` binds the module under its own last segment.
    const bound = last === 'self' ? (path.split('::').at(-2) ?? '') : last;
    return [{ path, local: alias?.[2] ?? bound }];
  }

  const close = matchingBrace(text, open);
  // Unbalanced braces mean the statement was not what this parser thought it was. Keep the
  // raw text so the caller still sees whatever it names rather than silently nothing.
  if (close === -1) return [{ path: text.replace(/\s+/g, ''), local: '' }];

  const prefix = text.slice(0, open).replace(/\s+/g, '').replace(/::$/, '');
  return splitTopLevel(text.slice(open + 1, close)).flatMap((part) =>
    expandUseTree(part).map((leaf) => ({
      path: prefix === '' ? leaf.path : `${prefix}::${leaf.path}`,
      local: leaf.local,
    })),
  );
}

/** Every path every `use` statement in `code` names. `code` must already be blanked. */
function rustUsePaths(code: string): UsePath[] {
  const found: UsePath[] = [];
  for (const match of code.matchAll(RUST_USE_STATEMENT)) {
    if (match.index === undefined) continue;
    const statement = match[0];
    const line = lineAt(code, match.index);
    const visibility = /^\s*(pub(?:\s*\([^)]*\))?)\s+use\b/.exec(statement)?.[1];
    const body = statement
      .replace(/^\s*(?:pub(?:\s*\([^)]*\))?\s+)?use\s/, '')
      .replace(/;\s*$/, '');
    for (const leaf of expandUseTree(body)) {
      found.push({ line, path: leaf.path, local: leaf.local, visibility });
    }
  }
  return found;
}

/**
 * The crate these rules are about, as a `use` statement outside it spells the root.
 *
 * `nysia_core::agent::claude` and `crate::agent::claude` are the same module; normalising one
 * onto the other is what lets a single check answer both.
 */
const CRATE_NAME = 'nysia_core';

/**
 * The module path of a Rust source file — `crate::agent::launch`.
 *
 * Needed because `self`, `super` and a bare first segment all mean something different
 * depending on which file they are written in. `super::claude` in `agent/launch.rs` is the
 * Claude module; the same words in `agent/mod.rs` are a module beside `agent` in the crate
 * root, and a rule that treated them alike would be wrong in one direction or the other.
 */
function rustModulePath(file: string): string {
  const at = file.indexOf('/src/');
  const tail = at === -1 ? file : file.slice(at + '/src/'.length);
  const parts = tail.replace(/\.rs$/, '').split('/');
  if (parts.at(-1) === 'mod') parts.pop();
  if (parts.length === 1 && (parts[0] === 'lib' || parts[0] === 'main')) parts.pop();
  return ['crate', ...parts].join('::');
}

/** Every name a file's `use` statements bind, mapped to the path it stands for. */
function useBindings(code: string): Map<string, string> {
  const bindings = new Map<string, string>();
  for (const { path, local } of rustUsePaths(code)) {
    if (local === '' || local === '*') continue;
    if (!bindings.has(local)) bindings.set(local, path);
  }
  return bindings;
}

/** How many alias hops to follow before giving up; `use a as b; use b as a;` terminates. */
const ALIAS_HOPS = 8;

/**
 * Resolve a Rust path to an absolute `crate::…` one, following this file's own aliases.
 *
 * This is the fix for the hole that made rule (f) walkable from the inside. `use claude as
 * c;` followed by `pub use c::Probe as NeutralProbe;` re-exports a Claude type under a
 * neutral name, and the path in that second statement — `c::Probe` — contains no segment
 * called `claude` at all. Matching on spelling could never see it; resolving the binding
 * first does.
 *
 * A bare first segment that is not a known alias is treated as naming something in *this*
 * module, which is what Rust does. That is what keeps a bare `claude::X` in `agent/mod.rs`
 * (where `mod claude;` is declared) apart from the same words in `agent/launch.rs`, where
 * they would not compile without a `use` that this function would then have followed.
 */
function absolutePath(
  path: string,
  modulePath: string,
  bindings: ReadonlyMap<string, string>,
  hops = 0,
): string {
  const segments = path.split('::').filter((segment) => segment !== '');
  if (segments.length === 0) return '';
  const [head, ...rest] = segments;
  if (head === undefined) return '';
  const bare = head.startsWith('r#') ? head.slice(2) : head;

  if (bare === 'crate' || bare === CRATE_NAME) return ['crate', ...rest].join('::');
  if (bare === 'self') return [modulePath, ...rest].join('::');
  if (bare === 'super') {
    const parent = modulePath.split('::').slice(0, -1).join('::');
    return [parent === '' ? 'crate' : parent, ...rest].join('::');
  }

  const bound = bindings.get(bare) ?? bindings.get(head);
  if (bound !== undefined && hops < ALIAS_HOPS) {
    return absolutePath([bound, ...rest].join('::'), modulePath, bindings, hops + 1);
  }
  return [modulePath, bare, ...rest].join('::');
}

/**
 * Whether an absolute path names the agent's Claude module.
 *
 * The `agent::claude` pair, on both sides of the boundary. Inside `agent/` this used to ask
 * only whether *any* segment was called `claude`, which reported
 * `pub use crate::vendor::claude::Whatever;` — somebody else's module, with the same name.
 * lint-meta has no suppression mechanism, so a file reported like that has no way out.
 */
function namesAgentClaude(absolute: string): boolean {
  const segments = absolute.split('::').filter((segment) => segment !== '');
  return segments.some(
    (segment, at) => segment === AGENT_SEGMENT && segments[at + 1] === CLAUDE_SEGMENT,
  );
}

/**
 * Whether a visibility lets a name out of `agent/`.
 *
 * Only a name that escapes the module can launder anything, so the restricted spellings are
 * not reports: `pub(self)` is private, and `pub(in crate::agent)` is exactly as private as a
 * bare `mod` for this purpose — both were reported before, and both are false.
 *
 * `pub(super)` is the one that depends on the file. In `agent/mod.rs` its parent is the crate
 * root, so it escapes; in `agent/launch.rs` the parent is `agent` itself, so it does not.
 */
function escapesAgent(visibility: string, file: string): boolean {
  const restriction = visibility.replace(/^pub/, '').trim();
  if (restriction === '') return true;
  const inner = restriction
    .replace(/^\(/, '')
    .replace(/\)$/, '')
    .replace(/^\s*in\s+/, '')
    .replace(/\s+/g, '');
  if (inner === 'self') return false;
  if (inner === 'crate') return true;

  const agentModule = `crate::${AGENT_SEGMENT}`;
  const absolute = absolutePath(inner, rustModulePath(file), new Map());
  return !(absolute === agentModule || absolute.startsWith(`${agentModule}::`));
}

/** A `pub`-marked item other than a `use`, and the text of its signature. */
interface PubItem {
  readonly line: number;
  readonly visibility: string;
  /** `type`, `fn`, `struct`, a field name — whatever followed the visibility. */
  readonly what: string;
  /** From the visibility to the body, so a claude path in a *body* is not a signature. */
  readonly signature: string;
}


/** The text of one item's signature, from `from` to its body or its semicolon. */
function signatureFrom(code: string, from: number): string {
  let depth = 0;
  let end = from;
  while (end < code.length) {
    const c = code[end];
    if (c === '(' || c === '[' || c === '<') depth += 1;
    else if (c === ')' || c === ']' || c === '>') depth = Math.max(0, depth - 1);
    else if (c === ';' || c === '{' || c === '}') break;
    else if (c === ',' && depth === 0) break;
    end += 1;
  }
  return code.slice(from, end);
}

/** The keywords that open a block whose contents can still be named from outside it. */
const RUST_ITEM_CONTAINERS = new Set(['mod', 'struct', 'enum', 'trait', 'union']);

/**
 * Whether a `{` opened by `header` encloses anything nameable from outside `agent/`.
 *
 * A function body is never one: an item declared inside `fn f()` cannot be reached from
 * anywhere, however many `pub`s it carries. A `mod`, `struct`, `enum`, `trait` or `union`
 * passes its own visibility down. An `impl` block is governed by the visibility of the type
 * it is for, which is looked up in this same file and **assumed to escape when it is not
 * found** — an `impl` on a type declared elsewhere is the one case here where guessing wrong
 * quietly is worse than guessing wrong loudly.
 */
function opensAnEscapingBlock(header: string, file: string, code: string): boolean {
  const text = header.trim();
  const visibility = /^(pub(?:\s*\([^)]*\))?)\s/.exec(text)?.[1];
  const rest = text.replace(/^pub(?:\s*\([^)]*\))?\s+/, '');
  const keyword = /^([A-Za-z_][A-Za-z0-9_]*)/.exec(rest)?.[1];

  if (keyword !== undefined && RUST_ITEM_CONTAINERS.has(keyword)) {
    return visibility !== undefined && escapesAgent(visibility.replace(/\s+/g, ''), file);
  }
  if (keyword === 'impl') {
    // `impl Thing`, `impl Trait for Thing`, `impl<'a> Trait<'a> for Thing<'a>` — the type is
    // the last name before the brace either way.
    const named = /([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^<>]*>)?\s*$/.exec(rest)?.[1];
    if (named === undefined) return true;
    const declared = new RegExp(
      `(?:^|\\n)\\s*(pub(?:\\s*\\([^)]*\\))?\\s+)?(?:struct|enum|union|trait|type)\\s+${named}\\b`,
    ).exec(code);
    if (declared === null) return true;
    const own = declared[1]?.replace(/\s+/g, '');
    return own !== undefined && escapesAgent(own, file);
  }
  return false;
}

/**
 * Every `pub`-marked item in `code` that can actually be named from outside `agent/`.
 *
 * The signature runs from the visibility to the first `;`, `{` or `}`, or to a `,` outside
 * any bracket. Stopping at `{` is the load-bearing part: it is what separates a Claude type
 * in a *signature*, which escapes the module, from a call to `super::claude::hooks::install`
 * in a *body*, which is the ordinary way `agent/` uses the module it owns.
 *
 * The walk tracks the blocks it is inside, because `pub` on its own says nothing about reach:
 * `mod private { pub type T = claude::Probe; }` and a `pub` field on a private struct hand
 * nothing to anybody. Reporting those would be the same defect as the `vendor::claude` report
 * this rule already had once — and in a checker with no suppression mechanism, a file it
 * reports wrongly has no way out but to stop writing the code it needs.
 */
function pubItems(code: string, file: string): PubItem[] {
  const items: PubItem[] = [];
  const frames: boolean[] = [];
  let at = 0;
  let headerFrom = 0;

  while (at < code.length) {
    const c = code[at];
    if (c === '{') {
      frames.push(opensAnEscapingBlock(code.slice(headerFrom, at), file, code));
      at += 1;
      headerFrom = at;
      continue;
    }
    if (c === '}') {
      frames.pop();
      at += 1;
      headerFrom = at;
      continue;
    }
    if (c === ';' || c === ',') {
      at += 1;
      headerFrom = at;
      continue;
    }
    if (code.startsWith('pub', at) && !/[\w:]/.test(code[at - 1] ?? '') && !/\w/.test(code[at + 3] ?? '')) {
      const visibility = /^pub(\s*\([^)]*\))?/.exec(code.slice(at))?.[0] ?? 'pub';
      if (frames.every(Boolean)) {
        const signature = signatureFrom(code, at + visibility.length);
        items.push({
          line: lineAt(code, at),
          visibility: visibility.replace(/\s+/g, ''),
          what: /^\s*([A-Za-z_][A-Za-z0-9_]*)/.exec(signature)?.[1] ?? '',
          signature,
        });
      }
      at += visibility.length;
      continue;
    }
    at += 1;
  }
  return items;
}

/** Every identifier or `::`-joined path in a fragment of Rust. */
const RUST_PATH_LIKE = /(?<![\w:])((?:r#)?[A-Za-z_][A-Za-z0-9_]*(?:\s*::\s*(?:(?:r#)?[A-Za-z_][A-Za-z0-9_]*|\*))*)/g;

/** The paths a signature names, resolved against this file. */
function signaturePaths(
  signature: string,
  modulePath: string,
  bindings: ReadonlyMap<string, string>,
): string[] {
  const found: string[] = [];
  for (const match of signature.matchAll(RUST_PATH_LIKE)) {
    const raw = match[1];
    if (raw === undefined) continue;
    found.push(absolutePath(raw.replace(/\s+/g, ''), modulePath, bindings));
  }
  return found;
}

/**
 * Rule (f): nothing outside `agent/**` may import Claude's specifics.
 *
 * D-3 and D-4: Claude is the only agent, and there is **no provider trait**. §7.4 gives the
 * reason and nightcore is the evidence — its seam was designed against one implementation,
 * and when a second arrived `StartSessionParams` fields were silently dropped, scans
 * hardcoded an autonomy level the new provider refuses, and every scan of the new kind
 * failed on first run from the UI. A trait extracted from one implementation encodes that
 * implementation's assumptions and calls them universal. So the protection now is a
 * boundary, and the trait gets extracted from two real implementations in v0.6+.
 *
 * **Rust privacy is the load-bearing half of this boundary**, the way rule (b) is to rule
 * (a): `agent/mod.rs` declares `mod claude` with no visibility modifier, so nothing outside
 * `agent/` can name it and code that tries does not compile. This rule is the half that
 * reports a file and a line a developer can act on in ten seconds — and the half that covers
 * what the compiler happily permits. It checks three things:
 *
 * 1. **Outside `agent/**`: naming `agent::claude`.** Through a `use` in any spelling — the
 *    tree is expanded, so `use crate::agent::{claude, other}` and
 *    `use crate::agent::{claude::{a, b}}` both report although neither writes those two
 *    segments next to each other, and `use crate::agent::claude as c` reports although the
 *    local name is `c`. Through a fully-qualified path with no `use` at all, too.
 * 2. **Inside `agent/**` but outside `agent/claude/**`: handing a Claude specific out under
 *    a neutral name.** This is the one the compiler allows and the one that matters most,
 *    because privacy does not help here — this is the module that *is* allowed to name
 *    `claude`. A `pub use claude::ClaudeLaunch;` in `agent/mod.rs` publishes a Claude type as
 *    `agent::ClaudeLaunch`, and everything downstream then depends on Claude's shape with the
 *    word nowhere in the path it writes. That is §7.4's seam, built by accident.
 *
 *    Every spelling of it is resolved rather than matched, because the laundering statement
 *    need not contain the word at all. `use claude as c;` followed by
 *    `pub use c::Probe as NeutralProbe;` is the shape that walked around the first version of
 *    this rule; so are `pub use c::*`, a second hop through
 *    `use self::claude::hooks as h; pub use h::EVENTS;`, a `pub type AliasProbe =
 *    claude::Probe;` and a `pub fn f() -> claude::launch::T`. A path is resolved through this
 *    file's own `use` bindings and onto an absolute `crate::…` path first, and the check is
 *    then the same `agent::claude` pair the outside check uses.
 *
 *    It applies to any public item, not only a `use`: a `pub` type alias, a `pub fn`
 *    signature and a `pub` struct field put the dependency in exactly the same place. The
 *    signature is read up to the body, so a call to `super::claude::hooks::install` *inside*
 *    a function is not a report — that is the ordinary way `agent/` uses the module it owns.
 *
 *    Only a visibility that actually leaves `agent/` counts. `pub(self)` and
 *    `pub(in crate::agent)` hand nothing out and are silent; `pub(super)` depends on the file
 *    it is written in, and escapes only from `agent/mod.rs`.
 *
 *    The module is arranged so none of this has to happen: neutral types are declared in
 *    `agent/`'s own files and `claude/` imports them *upward*.
 * 3. **Inside `agent/**` but outside `agent/claude/**`: `mod claude` at a visibility that
 *    escapes the module.** `pub` and `pub(crate)` disarm the privacy half. A bare
 *    `mod claude;` keeps it, and so does a restricted visibility that stays inside `agent/`.
 *
 * Files under `agent/claude/**` are exempt from all three: the module may import whatever it
 * likes, which is the point of having one.
 *
 * # What it cannot see, said rather than implied
 *
 * - **An alias whose target is in another file.** Resolution is per-file: a `use` in *this*
 *   file is followed, and a name that arrives through a `pub use` somewhere else is not.
 *   Inside `agent/` that is bounded by the fact that the re-export doing the laundering would
 *   itself be reported in the file that wrote it.
 * - **The reach of an `impl` on a type declared in another file** is unknown, and it is
 *   **assumed to escape**. That direction is deliberate: the alternative is going quiet about
 *   a `pub fn` that really does publish a Claude type, and this is the side of the boundary
 *   the compiler does not guard. An `impl` on a type declared in the same file is resolved
 *   properly, which is every one in `agent/` today.
 * - **A path reached through an aliased ancestor from outside `agent/`.** `use crate::agent
 *   as a;` then `a::claude::foo()` names no `agent::claude` in a file this rule resolves
 *   aliases for only when the alias is local. Note which half is still standing there:
 *   privacy does not care how a path is spelled, and `claude` is private, so that code does
 *   not compile. That is the difference between this hole and the one inside `agent/`, where
 *   the compiler permits everything and this rule is the only guard.
 * - **Anything that only exists after macro expansion**, `include!` included, the same blind
 *   spot rule (a) documents.
 * - **A Claude specific that is not in the `claude` module.** The rule is about a module
 *   boundary, not about the word: a constant spelling `"claude"` in `rpc/` is invisible to
 *   it. That is the right scope — D-4 is about where the implementation lives — but it is a
 *   scope, not a guarantee.
 * - **`agent.rs` instead of `agent/mod.rs`.** The boundary is a directory, so a single-file
 *   spelling of the module would sit outside it. Nothing in the repository is written that
 *   way and the layout is `agent/mod.rs`; it is listed because "the rule stopped looking"
 *   is the failure class this tool exists to remove.
 */
export function noClaudeSpecificsOutsideAgent(
  root: string,
  files: readonly string[],
): Violation[] {
  const violations: Violation[] = [];

  for (const file of files) {
    if (!file.endsWith('.rs')) continue;
    // The module itself may import whatever it likes. That is what a boundary is for.
    if (matches(file, AGENT_CLAUDE)) continue;

    const code = blankRustComments(read(root, file));
    const insideAgent = matches(file, AGENT);
    const modulePath = rustModulePath(file);
    const bindings = useBindings(code);

    if (!insideAgent) {
      const lines = new Set<number>();
      for (const { line, path } of rustUsePaths(code)) {
        if (namesAgentClaude(path)) lines.add(line);
      }
      for (const match of code.matchAll(RUST_AGENT_CLAUDE_PATH)) {
        if (match.index !== undefined) lines.add(lineAt(code, match.index));
      }
      for (const line of [...lines].sort((a, b) => a - b)) {
        violations.push({
          rule: CLAUDE_RULE,
          file,
          line,
          message:
            `imports Claude specifics from \`${AGENT.path}\`; only that module may name ` +
            'them (D-3, D-4). Reach the agent through the neutral surface it exports, or ' +
            'add one — a trait gets extracted from two implementations, not from one (§7.4)',
        });
      }
      continue;
    }

    const launders = (line: number, what: string, path: string): void => {
      violations.push({
        rule: CLAUDE_RULE,
        file,
        line,
        message:
          `${what} \`${path}\`, which hands a Claude specific out of \`${AGENT.path}\` under ` +
          'a neutral name — callers then depend on Claude\'s shape while the word ' +
          `\`${CLAUDE_SEGMENT}\` is nowhere in the path they write. Declare the neutral type ` +
          'here and have the Claude module import it instead (D-4, §7.4)',
      });
    };

    // A re-export, at a visibility that leaves the module. The path is resolved through this
    // file's own aliases first: `use claude as c; pub use c::Probe as NeutralProbe;` names no
    // `claude` anywhere in the statement that does the laundering.
    for (const { line, path, visibility } of rustUsePaths(code)) {
      if (visibility === undefined || !escapesAgent(visibility, file)) continue;
      const absolute = absolutePath(path, modulePath, bindings);
      if (namesAgentClaude(absolute)) launders(line, 're-exports', absolute);
    }

    // And every other public item whose *signature* names one — a `pub type` alias, a `pub fn`
    // returning or taking a Claude type, a `pub` struct field holding one. Each of those puts
    // the same dependency in the same place as a `pub use`; only the spelling differs.
    for (const { line, visibility, what, signature } of pubItems(code, file)) {
      if (what === 'use' || what === 'mod') continue;
      if (!escapesAgent(visibility, file)) continue;
      const named = signaturePaths(signature, modulePath, bindings).find(namesAgentClaude);
      if (named !== undefined) launders(line, `exposes`, named);
    }

    for (const match of code.matchAll(RUST_PUBLISHED_CLAUDE_MOD)) {
      if (match.index === undefined) continue;
      const visibility = /^pub(?:\s*\([^)]*\))?/.exec(match[0])?.[0] ?? 'pub';
      if (!escapesAgent(visibility.replace(/\s+/g, ''), file)) continue;
      violations.push({
        rule: CLAUDE_RULE,
        file,
        line: lineAt(code, match.index),
        message:
          `publishes \`mod ${CLAUDE_SEGMENT}\` outside \`${AGENT.path}\`, which is the half ` +
          'of this boundary the compiler enforces; a bare `mod claude;` — or a visibility ' +
          'that stays inside the module — is what keeps it',
      });
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
  return [
    ...noTauriOutsideDesktop(root, files),
    ...noStoreContextOutsideStore(root, files),
    ...noUnmutedRenderer(root, files),
    ...noClaudeSpecificsOutsideAgent(root, files),
  ];
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
