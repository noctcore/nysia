import { readFileSync, readdirSync } from 'node:fs';
import { join, posix, relative, sep } from 'node:path';

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

/**
 * The import forms that actually pull Tauri into a module.
 *
 * Anchored at the start of the line on purpose: an unanchored `from '@tauri-apps/...'`
 * also matches the sample code inside `scripts/prove-eslint-bans.ts`, where those strings
 * are test input rather than imports. A rule that fires on its own proof is a rule people
 * switch off.
 */
const TS_TAURI_IMPORTS = [
  /^\s*import\s[^'"]*from\s*['"]@tauri-apps[^'"]*['"]/,
  /^\s*import\s*['"]@tauri-apps[^'"]*['"]/,
  /^\s*export\s[^'"]*from\s*['"]@tauri-apps[^'"]*['"]/,
  // The closing line of a multi-line named import: `}` then `from '@tauri-apps/...'`.
  /^\s*\}\s*from\s*['"]@tauri-apps[^'"]*['"]/,
  /(?:^|[^\w.$])import\s*\(\s*['"]@tauri-apps[^'"]*['"]/,
  /(?:^|[^\w.$])require\s*\(\s*['"]@tauri-apps[^'"]*['"]/,
];

/** The carve-outs as a reader expects to see them in a message. */
const STORE_CONTEXT_ALLOWED_FOR_HUMANS = describeBoundaries(STORE_CONTEXT_ALLOWED);

/**
 * A specifier naming `StoreContext`, in any of the three ways JavaScript can quote one.
 *
 * The backreference is what makes the template-literal case safe: an apostrophe inside a
 * backticked specifier cannot close it. Any path ending in `StoreContext`, with or without
 * a file extension — the same reach as the ESLint glob patterns, which deliberately do not
 * require `store/` next to the filename, so `../store/./StoreContext` and
 * `../store//StoreContext` are caught here too.
 */
const STORE_CONTEXT_SPECIFIER = String.raw`(['"\`])[^'"\`]*StoreContext(?:\.[A-Za-z0-9]+)?\1`;

/**
 * Reaching `StoreContext` through a call rather than an `import` statement.
 *
 * ESLint's `no-restricted-imports` owns `import` and `export … from`; it does not see
 * `require()` or dynamic `import()`, and `no-restricted-modules` was removed in ESLint 9.
 * The config header in `eslint.config.js` assigns those two to lint-meta. A top-level
 * `await import('../store/StoreContext')` followed by `useContext` hands a component the
 * raw provider back, whose commands return promises that `void` silently drops.
 *
 * Matched over the whole file with comments blanked, not line by line. Line by line the
 * rule claimed to match any path ending in `StoreContext` and matched only a single-line,
 * single- or double-quoted one; four spellings of the same call passed lint-meta, ESLint,
 * `tsc` **and** the Vite build (#19): a template literal, the specifier on its own line, a
 * line whose trim started with `/*` — which the guard skipped wholesale, comment or not —
 * and `import.meta.glob`, which is handled separately below because it need not name the
 * file at all.
 *
 * WHAT THIS STILL CANNOT SEE, stated rather than implied by silence: a specifier that is
 * not a literal. `import(specifier)`, `import('../store/' + name)` and a template with a
 * `${…}` in it are all invisible here, and ESLint's `no-restricted-imports` is equally
 * blind to the static equivalents. That is an honest limit of reading source text, not an
 * oversight — and it is why the store module keeps the provider unexported from its public
 * surface rather than relying on this rule alone.
 *
 * Lookbehind rather than a consumed delimiter: `matchAll` reports the match index, and a
 * consumed leading newline would put the reported line one above the call.
 */
const TS_STORE_CONTEXT_CALLS = [
  new RegExp(String.raw`(?<![\w.$])import\s*\(\s*${STORE_CONTEXT_SPECIFIER}`, 'g'),
  new RegExp(String.raw`(?<![\w.$])require\s*\(\s*${STORE_CONTEXT_SPECIFIER}`, 'g'),
];

/**
 * Vite's glob import, which returns modules the specifier never names.
 *
 * `import.meta.glob('../store/*.ts')` hands back the provider without the string
 * `StoreContext` appearing anywhere, so no specifier test can decide it. Reported by
 * default, with two exits that can be read off the call:
 *
 * - `query: '?raw'` in the options — every module comes back as source text, which has no
 *   commands on it. Read from the options alone, because that is where it applies to the
 *   whole call; the per-pattern `'…?raw'` spelling exempts only its own pattern;
 * - **every** pattern restricted to extensions outside {@link BUNDLED_EXTENSIONS} — a glob
 *   that can only return stylesheets cannot return the provider.
 *
 * The first argument may be an array, which Vite documents as first-class, and every literal
 * in it is read. Reading only the first was the defect: a stylesheet, or a negation naming
 * one, in front of the store exempted a call that returned the provider anyway. One pattern
 * that could return a module is enough to report the call, and a first argument with no
 * literal in it at all — built at runtime — is reported too.
 *
 * Both exits are what `apps/web` already uses the glob for, and both are proven by the clean
 * fixture, in the array form as well as the single-pattern one, rather than merely observed
 * on today's tree.
 */
const TS_GLOB_IMPORT = /(?<![\w.$])import\s*\.\s*meta\s*\.\s*glob(?:Eager)?\s*\(/g;

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

/**
 * The end of the string or template literal opening at `at`, or `undefined` if one does not.
 *
 * Two rules, and both of them are about false *negatives*:
 *
 * - `'` and `"` must close on the same line. They cannot legally span one, and treating an
 *   unterminated quote as a literal lets a quote inside a regex — `/['"]/` — swallow
 *   everything up to the next quote in the file.
 * - a backtick may span lines, so it is scanned to its terminator; if there is none, it was
 *   not a literal either.
 *
 * A backslash escapes the next character in both, which is why `"he said \"/*\""` is one
 * literal and not two with a comment opener loose between them.
 */
function endOfJsLiteral(source: string, at: number): number | undefined {
  const quote = source[at];
  if (quote !== '"' && quote !== "'" && quote !== '`') return undefined;

  for (let k = at + 1; k < source.length; k += 1) {
    const c = source[k];
    if (c === '\\') {
      k += 1;
      continue;
    }
    if (c === quote) return k + 1;
    if (quote === '`') {
      // A `${…}` substitution is code, and code can hold a backtick — in a string, in a
      // nested template, in a regex. Running to the first unescaped backtick ended the
      // literal inside the substitution, and every quote after that was read one out of
      // step, which is how a later template leaked a `/*` into the scan.
      if (c === '$' && source[k + 1] === '{') {
        const close = endOfSubstitution(source, k + 2);
        if (close === undefined) return undefined;
        k = close - 1;
      }
      continue;
    }
    if (c === '\n') return undefined;
  }
  return undefined;
}

/**
 * The index just past the `}` closing a `${` substitution, or `undefined` if it never closes.
 *
 * Braces are counted and literals inside are stepped over by {@link endOfJsLiteral}, which
 * is why the two call each other. An unbalanced substitution means the template was not a
 * template, and refusing it is the safe direction: the scan then reads less as comment.
 */
function endOfSubstitution(source: string, from: number): number | undefined {
  let depth = 1;
  let k = from;

  while (k < source.length) {
    const literalEnd = endOfJsLiteral(source, k);
    if (literalEnd !== undefined) {
      k = literalEnd;
      continue;
    }
    const c = source[k];
    if (c === '{') depth += 1;
    else if (c === '}') {
      depth -= 1;
      if (depth === 0) return k + 1;
    }
    k += 1;
  }
  return undefined;
}

/**
 * Characters after which a `/` opens a regular expression rather than dividing.
 *
 * The standard disambiguation, which needs the previous token: after a value, `/` divides;
 * after an operator, a delimiter or nothing at all, it opens a literal.
 */
const REGEX_MAY_FOLLOW = new Set([
  '(', ',', '=', ':', '[', '!', '&', '|', '?', '{', '}', ';', '+', '-', '*', '%', '^', '~',
  '<', '>',
]);

/** Keywords a regex may follow, for the cases an operator character does not cover. */
const REGEX_MAY_FOLLOW_KEYWORD = new Set([
  'return',
  'typeof',
  'instanceof',
  'in',
  'of',
  'new',
  'delete',
  'void',
  'throw',
  'do',
  'else',
  'case',
  'yield',
  'await',
]);

/**
 * The index just past a regular expression literal opening at `at`, or `undefined`.
 *
 * A regex cannot span lines, and inside a character class a `/` needs no escape — both are
 * why this is scanned rather than guessed at. The point of scanning it at all is that a
 * regex may hold any quoting character: `` /[`]/ `` used to start a template literal that
 * ran to the next backtick in the file, and the `/*` it left behind opened a block comment
 * that blanked every line below it.
 */
function endOfRegexLiteral(source: string, at: number): number | undefined {
  let inClass = false;

  for (let k = at + 1; k < source.length; k += 1) {
    const c = source[k];
    if (c === '\\') {
      k += 1;
      continue;
    }
    if (c === '\n') return undefined;
    if (inClass) {
      if (c === ']') inClass = false;
      continue;
    }
    if (c === '[') {
      inClass = true;
      continue;
    }
    if (c === '/') {
      let end = k + 1;
      while (end < source.length && /[a-z]/i.test(source[end] ?? '')) end += 1;
      return end;
    }
  }
  return undefined;
}

/**
 * Blank out JavaScript and TypeScript comment bodies, preserving every byte position.
 *
 * Rule (d) scans whole files rather than single lines, so it has to know where a comment
 * ends. The previous rule skipped any line whose trim started with `//`, `*` or `/*`, which
 * is why a line that opened with a block comment, closed it again and then called
 * `await import('../store/StoreContext')` passed (#19): the guard threw the whole line
 * away without reading past the comment.
 *
 * Bodies become spaces and newlines are kept, so a match index still maps to the right line
 * — the same contract as {@link blankRustComments}, and the same reason: a rule that
 * silently stops scanning keeps reporting success.
 *
 * The direction that matters is over-blanking. String literals are stepped over as units
 * rather than blanked, because a specifier *is* a literal; failing to recognise one lets the
 * `//` or `/*` inside it open a comment that blanks real code below. Mistaking code for a
 * literal is the safe direction — the scan reads less as comment, never more — which is why
 * {@link endOfJsLiteral} refuses anything it cannot terminate rather than guessing.
 *
 * Three things are scanned that an earlier version of this waved at, and the claim it made
 * — that a regex could only ever hold a quote, which the same-line rule defuses — was simply
 * wrong. The same-line rule covers `'` and `"`. A backtick is the third quoting character
 * and a template may span lines, so a regex holding one started a literal that ran to the
 * next backtick in the file and left a `/*` loose behind it. So:
 *
 * - **regex literals are scanned**, using the previous significant token to tell a literal
 *   from a division. Misreading a division as a literal only skips text, which reads less as
 *   comment, never more;
 * - **`${…}` substitutions are scanned**, because they are code and code holds quotes. Running
 *   to the first unescaped backtick was only correct while no substitution contained one;
 * - **an unterminated block comment is not a comment.** It is a syntax error, so reading it
 *   as code costs nothing real, and it bounds every future mis-scan of this kind: a leaked
 *   `/*` that is never closed can no longer blank the rest of the file.
 *
 * What is left, named rather than claimed away: a mis-scan that leaks a `/*` and finds a
 * closer later in the file still blanks what lies between the two. Both trips fixtures carry
 * such a closer on purpose — without one the third defence alone rescues them, and neither
 * fixture could then tell whether the first two defences were doing anything at all.
 */
export function blankJsComments(source: string): string {
  // `split('')`, deliberately not `[...source]`: every index below is a UTF-16 code unit,
  // and code-point iteration would drift one slot per astral character. See
  // `blankRustComments` for what that costs.
  const out = source.split('');
  let i = 0;
  /** The last significant character of code, for telling a regex from a division. */
  let previous: string | undefined;

  const blankTo = (end: number): void => {
    for (let k = i; k < end && k < source.length; k += 1) {
      if (out[k] !== '\n') out[k] = ' ';
    }
    i = Math.min(end, source.length);
  };

  const opensRegex = (): boolean => {
    if (previous === undefined) return true;
    if (REGEX_MAY_FOLLOW.has(previous)) return true;
    const word = /([A-Za-z_$]+)\s*$/.exec(source.slice(Math.max(0, i - 16), i))?.[1];
    return word !== undefined && REGEX_MAY_FOLLOW_KEYWORD.has(word);
  };

  while (i < source.length) {
    if (source.startsWith('/*', i)) {
      // JavaScript block comments do not nest; the first `*/` ends it. One that never ends
      // is not a comment but a syntax error — the file does not compile — and blanking to
      // end of file on it is precisely the over-blanking that hides real code while the
      // rule reports success. Treated as ordinary characters instead.
      const close = source.indexOf('*/', i + 2);
      if (close !== -1) {
        blankTo(close + 2);
        continue;
      }
    } else if (source.startsWith('//', i)) {
      const newline = source.indexOf('\n', i);
      blankTo(newline === -1 ? source.length : newline);
      // A comment is not a token, so it does not change what the next `/` may be.
      continue;
    }

    const literalEnd = endOfJsLiteral(source, i);
    if (literalEnd !== undefined) {
      i = literalEnd;
      previous = source[literalEnd - 1];
      continue;
    }

    if (source[i] === '/' && opensRegex()) {
      const regexEnd = endOfRegexLiteral(source, i);
      if (regexEnd !== undefined) {
        i = regexEnd;
        previous = '/';
        continue;
      }
    }

    const character = source[i];
    if (character !== undefined && !/\s/.test(character)) previous = character;
    i += 1;
  }

  const blanked = out.join('');
  if (blanked.length !== source.length) {
    throw new Error(
      `blankJsComments changed the length of its input (${source.length} -> ` +
        `${blanked.length}); every index in this function must be a UTF-16 code unit`,
    );
  }
  return blanked;
}

/**
 * The text between the parenthesis at `open` and its match, literals stepped over.
 *
 * Used to read a glob import's arguments, which may span lines and hold parentheses of
 * their own. Comments are already blanked by the time this runs; literals are not, because
 * the pattern and the `?raw` query are both literals and both are the point.
 */
function callArguments(code: string, open: number): string {
  let depth = 0;
  let i = open;

  while (i < code.length) {
    const literalEnd = endOfJsLiteral(code, i);
    if (literalEnd !== undefined) {
      i = literalEnd;
      continue;
    }
    const c = code[i];
    if (c === '(') depth += 1;
    else if (c === ')') {
      depth -= 1;
      if (depth === 0) return code.slice(open + 1, i);
    }
    i += 1;
  }
  return code.slice(open + 1);
}

/**
 * Could a glob with this pattern return a module that holds the provider?
 *
 * False only when the pattern itself says it cannot: a `?raw` query, which yields source
 * text, or an extension pinned to something that cannot carry commands. Anything else
 * returns true — a pattern that does not say what it matches can match anything, and the
 * rule fails closed on it.
 *
 * A negation is judged by its extension like any other pattern, which can over-report: a
 * `!`-prefixed glob naming a bundled extension reads as "could be a module" even though
 * excluding a file returns nothing. Over-reporting is the safe direction and the message
 * says how to fix it. The alternative — working out which patterns cancel which — is a glob
 * engine, and getting that wrong is a silent exemption, which is the failure this rule is.
 */
function globYieldsModule(pattern: string): boolean {
  const parts = pattern.split('?');
  const path = parts[0] ?? '';
  // `'../x/*.ts?raw'` — the per-pattern spelling of the query. The options-object spelling
  // is read at the call site, because it applies to every pattern in the call.
  if (parts.slice(1).some((query) => query.includes('raw'))) return false;

  const braced = /\.\{([^}]*)\}$/.exec(path);
  if (braced !== null) {
    const listed = braced[1];
    if (listed === undefined) return true;
    return listed.split(',').some((e) => BUNDLED_EXTENSIONS.includes(e.trim()));
  }

  const single = /\.([A-Za-z0-9]+)$/.exec(path);
  if (single !== null) {
    const extension = single[1];
    return extension === undefined || BUNDLED_EXTENSIONS.includes(extension);
  }

  return true;
}

/**
 * The first argument of a call, given the text between its parentheses.
 *
 * Split at the first top-level comma, with brackets and braces counted and literals stepped
 * over, so an array of patterns stays whole and the options object stays out of it. Reading
 * the arguments as one blob is what let the options decide a pattern's fate and the reverse.
 */
function firstArgument(argumentText: string): string {
  let depth = 0;
  let i = 0;

  while (i < argumentText.length) {
    const literalEnd = endOfJsLiteral(argumentText, i);
    if (literalEnd !== undefined) {
      i = literalEnd;
      continue;
    }
    const c = argumentText[i];
    if (c === '(' || c === '[' || c === '{') depth += 1;
    else if (c === ')' || c === ']' || c === '}') depth -= 1;
    else if (c === ',' && depth === 0) return argumentText.slice(0, i);
    i += 1;
  }
  return argumentText;
}

/**
 * Every string and template literal in a stretch of code, contents only.
 *
 * The first version of the glob check read *one* literal out of the whole call and decided
 * the call on it. Vite documents an array of patterns as first-class, so a stylesheet — or a
 * negation naming one — in front of the store exempted a glob that returned the provider
 * anyway. A rule written to close "narrower than its words" was narrower than its words, and
 * its proof never noticed because it only ever passed a single-literal call. Every pattern
 * is read now, and one that could return a module is enough to report the call.
 */
function literalsIn(text: string): string[] {
  const found: string[] = [];
  let i = 0;

  while (i < text.length) {
    const literalEnd = endOfJsLiteral(text, i);
    if (literalEnd !== undefined) {
      found.push(text.slice(i + 1, literalEnd - 1));
      i = literalEnd;
      continue;
    }
    i += 1;
  }
  return found;
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

    // TypeScript and JavaScript stay line-based. ESLint owns this boundary properly with
    // an AST; lint-meta is the backstop for the files ESLint's ignores exclude, and a
    // statement-level scan here would fire on the sample code inside
    // `scripts/prove-eslint-bans.ts`, where those imports are test input.
    source.split(/\r?\n/).forEach((text, index) => {
      const trimmed = text.trim();
      // Comments talk about the rule constantly; only code breaks it.
      if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) {
        return;
      }
      if (TS_TAURI_IMPORTS.some((pattern) => pattern.test(text))) report(file, index + 1);
    });
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
 */
export function noStoreContextOutsideStore(root: string, files: readonly string[]): Violation[] {
  const violations: Violation[] = [];

  for (const file of files) {
    if (!matches(file, WEBVIEW)) continue;
    if (matchesAny(file, STORE_CONTEXT_ALLOWED)) continue;

    const extension = file.slice(file.lastIndexOf('.'));
    if (!SOURCE_EXTENSIONS.includes(extension) || extension === '.rs') continue;

    // Scanned whole, with comment bodies blanked. Rule (a) stays line-based because it also
    // reads `scripts/**`, where `prove-eslint-bans.ts` carries tauri imports as test input;
    // rule (d) never leaves `apps/web`, so it has no such sample to fire on and can afford
    // to see a call that spans lines.
    const code = blankJsComments(read(root, file));

    const report = (index: number, message: string): void => {
      violations.push({
        rule: 'no-store-context-outside-store',
        file,
        line: lineAt(code, index),
        message,
      });
    };

    for (const pattern of TS_STORE_CONTEXT_CALLS) {
      for (const match of code.matchAll(pattern)) {
        if (match.index === undefined) continue;
        report(
          match.index,
          'reaches StoreContext through a call; components use useCommands() from ' +
            `store/hooks. Only ${STORE_CONTEXT_ALLOWED_FOR_HUMANS} may touch the provider`,
        );
      }
    }

    for (const match of code.matchAll(TS_GLOB_IMPORT)) {
      if (match.index === undefined) continue;
      const args = callArguments(code, match.index + match[0].length - 1);
      const patterns = firstArgument(args);
      // `{ query: '?raw' }` applies to every pattern in the call, so it is read from the
      // options rather than from the patterns — and only from the options, so a `?raw`
      // written inside one pattern cannot exempt the others beside it.
      if (args.slice(patterns.length).includes('?raw')) continue;

      const literals = literalsIn(patterns);
      // No literal at all is a pattern built at runtime, which says nothing about what it
      // can return; one literal that could return a module is enough to report the call.
      if (literals.length > 0 && !literals.some(globYieldsModule)) continue;

      report(
        match.index,
        'glob-imports modules it does not name, so it may hand back the store provider; ' +
          `only ${STORE_CONTEXT_ALLOWED_FOR_HUMANS} may touch it. Read the files as text ` +
          "with query: '?raw', or pin the glob to an extension that cannot be a module",
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
