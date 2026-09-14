/**
 * Every way a JavaScript or TypeScript file reaches another module, read from a syntax tree.
 *
 * This replaces a hand-written scanner, and the reason is worth recording because three
 * rounds of review reached it independently. The scanner blanked comments, stepped over
 * string literals, guessed at regex-versus-division and tracked template substitutions by
 * hand — and every round found a spelling it mishandled, with each fix introducing the next:
 *
 * - a specifier on its own line, and one written as a template literal;
 * - a line opening with a block comment, which the guard threw away whole;
 * - a string holding a comment opener, which opened one that never closed;
 * - a regex holding a backtick, which started a template that ran to the next backtick;
 * - a substitution holding a backtick, which ended its own template early;
 * - and finally an ordinary component line — `<Icon n={i} /><Tag c={`w-1/2`} />` — where the
 *   self-closing slash opened a regex scan that swallowed the template's opening backtick
 *   and blanked a dynamic store import out of existence, with every gate green.
 *
 * The ingredients were always the same: a quote, a backtick, a slash, a brace. A scanner
 * cannot decide between them without being a JavaScript parser, and writing one badly, one
 * bug at a time, is what the last three rounds were. TypeScript is already a dependency and
 * its parser is the thing the rest of the toolchain trusts, so the file is parsed and walked
 * instead. Node kinds are unambiguous: a comment is trivia, a string is a `StringLiteral`,
 * and `import(…)` is a `CallExpression` whose callee is the `import` keyword whatever else
 * is on the line.
 *
 * Nothing here decides policy. It reports what the file reaches; `rules.ts` decides which of
 * those are allowed.
 */

import ts from 'typescript';

/** How a module was reached. */
export type ReferenceKind = 'import' | 'export' | 'dynamic-import' | 'require' | 'glob';

/**
 * One reference to another module.
 *
 * `specifier` is the cooked text of a literal — escapes resolved, quotes gone — and
 * `undefined` when the specifier is not a literal at all. `import(name)`,
 * `import('../store/' + name)` and a template with a substitution in it are all
 * `undefined`: a parser can say that the specifier is computed, which is more than the
 * scanner could, but not what it will compute to. A caller that must fail closed has the
 * `undefined` to key on.
 */
export type ModuleReference =
  | {
      readonly kind: Exclude<ReferenceKind, 'glob'>;
      readonly line: number;
      readonly specifier: string | undefined;
    }
  | {
      readonly kind: 'glob';
      readonly line: number;
      /** One entry per pattern; `undefined` where the pattern is not a literal. */
      readonly patterns: readonly (string | undefined)[];
      /** What the options do to the question — see {@link GlobOptionsVerdict}. */
      readonly options: GlobOptionsVerdict;
    };

/**
 * How to parse a file, from its extension.
 *
 * `.ts` must not be parsed as TSX: `<T>value` is a type assertion there and JSX here, and
 * getting that backwards turns valid code into a parse error. `.js` is parsed as JSX
 * because that is strictly more permissive — a `.js` file without JSX parses the same
 * either way, and one with it would otherwise be a parse error.
 */
function scriptKindFor(file: string): ts.ScriptKind {
  if (file.endsWith('.tsx')) return ts.ScriptKind.TSX;
  if (file.endsWith('.ts') || file.endsWith('.mts') || file.endsWith('.cts')) {
    return ts.ScriptKind.TS;
  }
  return ts.ScriptKind.JSX;
}

/**
 * The expression inside whatever is wrapped around it.
 *
 * `('x')`, `'x' as string`, `'x' satisfies string`, `<string>'x'` and `'x'!` are all the
 * string `'x'` to the bundler, and all of them used to read as "not a literal" and be
 * dropped as computed. Three of those resolve to the store in dev and in build, and ESLint
 * cannot see a dynamic import at all, so dropping them was the rule's own blind spot.
 *
 * The compiler has `skipOuterExpressions` for exactly this, but it is not in the public
 * typings — only the `OuterExpressionKinds` enum and `restoreOuterExpressions` are — and
 * reaching into an internal to avoid five public predicates is the sort of guess this rule
 * has been punished for. These are the public ones, and a new wrapper kind fails closed:
 * it reads as computed, which is reported.
 */
function unwrap(node: ts.Expression): ts.Expression {
  let current = node;
  for (;;) {
    if (
      ts.isParenthesizedExpression(current) ||
      ts.isAsExpression(current) ||
      ts.isSatisfiesExpression(current) ||
      ts.isTypeAssertionExpression(current) ||
      ts.isNonNullExpression(current)
    ) {
      current = current.expression;
      continue;
    }
    const skipped = ts.skipPartiallyEmittedExpressions(current);
    if (skipped === current) return current;
    current = skipped;
  }
}

/** The cooked text of a string or no-substitution template literal, if that is what it is. */
function literalText(node: ts.Node | undefined): string | undefined {
  if (node === undefined || !ts.isExpression(node)) return undefined;
  const inner = unwrap(node);
  return ts.isStringLiteralLike(inner) ? inner.text : undefined;
}

/** The property name of an object-literal member, however it is written. */
function propertyName(node: ts.ObjectLiteralElementLike): string | undefined {
  const name = node.name;
  if (name === undefined) return undefined;
  if (ts.isIdentifier(name) || ts.isStringLiteralLike(name)) return name.text;
  return undefined;
}

/**
 * Is `import.meta.glob` being called, rather than some other `.glob`?
 *
 * `import.meta` is a `MetaProperty`, which no identifier can impersonate — so this cannot be
 * fooled by a local named `importMeta`, and it does not need to be spelled defensively
 * against whitespace the way a regex did.
 */
function isGlobCall(expression: ts.Expression): boolean {
  if (!ts.isPropertyAccessExpression(expression)) return false;
  // `globEager` was removed before Vite 8 and the pinned version does not have it. A test
  // for a spelling the toolchain lacks asserts nothing, which this rule has now done three
  // times; the eager glob is `{ eager: true }`, which is an option like any other.
  if (expression.name.text !== 'glob') return false;

  const target = expression.expression;
  return (
    ts.isMetaProperty(target) &&
    target.keywordToken === ts.SyntaxKind.ImportKeyword &&
    target.name.text === 'meta'
  );
}

/**
 * What a glob call's options do to the question this tool is asking.
 *
 * READ THIS BEFORE ADDING A CASE. The default is closed. This does not enumerate the ways a
 * call might be dangerous — it enumerates the very short list of things that have been
 * *verified against the pinned Vite* to make a call harmless, and everything else is
 * `undecidable`, which the rule reports.
 *
 * That inversion is the property that makes the rule sound rather than merely current.
 * Vite's option surface belongs to Vite: `base` moves the directory a pattern resolves
 * against, `caseSensitive` changes which files it matches, `exhaustive` widens it into
 * `node_modules`, and the next release may add another. Enumerating the dangerous ones means
 * every option nobody has thought of fails OPEN, which is unbounded. Enumerating the safe
 * ones means the worst an unfamiliar option can do is produce a report. The other direction
 * puts the store provider in the production bundle with every gate green.
 *
 * That trade is real but it is not free, and an earlier version of this comment undersold
 * it: lint-meta has NO suppression mechanism, so an over-report cannot be silenced in place
 * the way an ESLint one can. It has to be fixed, argued with in review, or the rule has to
 * change. That raises the price of a needless report, which is why the query narrowing below
 * is taken and why the list is kept to forms this repository actually writes — not why the
 * default is opened again.
 *
 * So the list is short, and each entry was executed rather than read:
 *
 * - `query: '?raw'` — the modules come back as source text. Verified by running the pinned
 *   Vite over it in `scripts/prove-vite-glob.test.ts`, which also pins that a plain glob and
 *   the `{ raw: true }` spelling both hand back the real module.
 * - `import` and `eager` — neither changes which files are reached nor whether what comes
 *   back is source text. `eager` decides static versus lazy; `import` picks an export off
 *   the module that came back either way.
 *
 * Everything else is undecidable, deliberately, including spellings that may well be safe.
 * `query: 'raw'` and `query: { raw: '' }` both produced source text when executed here, and
 * they are still reported: a second spelling of a thing already expressible is not worth an
 * exemption whose correctness has to be re-verified on every Vite bump. `as: 'raw'` is
 * deprecated upstream and reported for the same reason.
 */
export type GlobOptionsVerdict =
  /** Verified to hand back source text rather than modules. */
  | { readonly kind: 'source-text' }
  /** Nothing here changes what the call reaches or what it returns. */
  | { readonly kind: 'transparent' }
  /** Something here decides that, and it is not on the verified list. */
  | { readonly kind: 'undecidable'; readonly because: string };

/** Options that cannot change which files are reached, nor whether modules come back. */
const INERT_OPTIONS = new Set(['import', 'eager']);

function readGlobOptions(options: ts.Expression | undefined): GlobOptionsVerdict {
  if (options === undefined) return { kind: 'transparent' };

  const literal = unwrap(options);
  if (!ts.isObjectLiteralExpression(literal)) {
    return { kind: 'undecidable', because: 'its options are not an object literal' };
  }

  let sourceText = false;

  for (const property of literal.properties) {
    if (!ts.isPropertyAssignment(property)) {
      // A shorthand, a spread or a method: the options are assembled elsewhere.
      return { kind: 'undecidable', because: 'an option it cannot read' };
    }
    const key = propertyName(property);
    if (key === undefined) {
      return { kind: 'undecidable', because: 'an option whose name is computed' };
    }
    if (INERT_OPTIONS.has(key)) continue;

    if (key === 'query') {
      // Verified by execution: Vite appends the query to each import path only AFTER the
      // files have been globbed, so a query — whatever its value, however it is spelled,
      // even one this cannot evaluate — cannot change WHICH files are reached. Only `?raw`
      // proves what comes back is harmless; every other query falls through to the tree
      // check, which reports if the pattern reaches a module and stays quiet if it does not.
      // That takes the asset, worker and url globs out of the over-report set without
      // reopening anything, because the file set is still decided by the tree.
      if (literalText(property.initializer) === '?raw') sourceText = true;
      continue;
    }
    return { kind: 'undecidable', because: `the \`${key}\` option` };
  }

  return sourceText ? { kind: 'source-text' } : { kind: 'transparent' };
}

/** The patterns a glob call was given, `undefined` where one is not a literal. */
function globPatterns(argument: ts.Expression | undefined): (string | undefined)[] {
  if (argument === undefined) return [undefined];
  if (ts.isArrayLiteralExpression(argument)) {
    return argument.elements.map((element) => literalText(element));
  }
  return [literalText(argument)];
}

/**
 * Every module reference in a source file.
 *
 * The file is parsed, never executed, and a parse it cannot complete still yields a tree —
 * TypeScript recovers rather than throwing. A file broken enough for that to lose a call is
 * a file `pnpm typecheck` rejects, so nothing here needs to guess at malformed input.
 */
export function moduleReferences(file: string, source: string): ModuleReference[] {
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    scriptKindFor(file),
  );

  const found: ModuleReference[] = [];
  const lineOf = (node: ts.Node): number =>
    sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;

  const visit = (node: ts.Node): void => {
    if (ts.isImportDeclaration(node)) {
      found.push({
        kind: 'import',
        line: lineOf(node),
        specifier: literalText(node.moduleSpecifier),
      });
    } else if (ts.isExportDeclaration(node) && node.moduleSpecifier !== undefined) {
      found.push({
        kind: 'export',
        line: lineOf(node),
        specifier: literalText(node.moduleSpecifier),
      });
    } else if (
      ts.isImportEqualsDeclaration(node) &&
      ts.isExternalModuleReference(node.moduleReference)
    ) {
      // `import x = require('…')`, which is still legal in a `.cts` file.
      found.push({
        kind: 'require',
        line: lineOf(node),
        specifier: literalText(node.moduleReference.expression),
      });
    } else if (ts.isCallExpression(node)) {
      if (node.expression.kind === ts.SyntaxKind.ImportKeyword) {
        found.push({
          kind: 'dynamic-import',
          line: lineOf(node),
          specifier: literalText(node.arguments[0]),
        });
      } else if (ts.isIdentifier(node.expression) && node.expression.text === 'require') {
        found.push({
          kind: 'require',
          line: lineOf(node),
          specifier: literalText(node.arguments[0]),
        });
      } else if (isGlobCall(node.expression)) {
        found.push({
          kind: 'glob',
          line: lineOf(node),
          patterns: globPatterns(node.arguments[0]),
          options: readGlobOptions(node.arguments[1]),
        });
      }
    }

    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return found;
}

/**
 * Every line on which `name` is *called* in a source file.
 *
 * The same discipline as {@link moduleReferences} and for the same reason: a rule that wants
 * to know whether a file performs a particular call must not decide it by searching the text.
 * `muteTerminalReplies` appears in comments in this repository — including in a sentence
 * explaining why the call must exist — and a scan would count those and pass a module that
 * never makes the call. A `CallExpression` whose callee is that identifier is unambiguous.
 *
 * Matches a bare call and a qualified one, `x.muteTerminalReplies()` included, because what
 * matters to a caller is that the function ran and not which binding reached it. What it
 * cannot see is a call through a computed member or an alias — `const m = mute; m()` — and
 * that is the same boundary the module-reference reader has: a parser can say a name is
 * computed but not what it computes to. Nothing here decides policy; `rules.ts` does.
 */
export function callSites(file: string, source: string, name: string): number[] {
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    scriptKindFor(file),
  );

  const lines: number[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node)) {
      const callee = node.expression;
      const called =
        (ts.isIdentifier(callee) && callee.text === name) ||
        (ts.isPropertyAccessExpression(callee) && callee.name.text === name);
      if (called) {
        lines.push(sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1);
      }
    }
    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return lines;
}

/** One value a module binds from another, and the local name it binds it to. */
export interface ImportedValue {
  /** The module it came from, as written. */
  readonly specifier: string;
  /** The exported name, `default` for a default import, or `*` for a namespace. */
  readonly imported: string;
  /** What the importing module calls it. `Terminal as T` binds `T`. */
  readonly local: string;
  readonly line: number;
}

/**
 * Every value a module imports, with the local name each is bound to.
 *
 * Narrower than {@link moduleReferences} on purpose, and narrower in the one direction that
 * matters: **a type-only binding is not here.** `import type { Terminal } from '@xterm/xterm'`
 * and `import { type Terminal }` both erase, so neither can construct anything, and a rule
 * that asked "does this module reach the library" reported them. Type-ness lives at two
 * levels — the clause (`import type { … }`) and each specifier (`import { type X, Y }`) — and
 * both are read, because `verbatimModuleSyntax` keeps the second form's import statement at
 * runtime while binding no value from it.
 *
 * A bare `import '@xterm/xterm'` binds nothing and is likewise absent. Re-exports are not
 * imports and are {@link reExports}.
 */
export function importedValues(file: string, source: string): ImportedValue[] {
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    scriptKindFor(file),
  );

  const found: ImportedValue[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isImportDeclaration(node)) {
      const specifier = literalText(node.moduleSpecifier);
      const clause = node.importClause;
      if (specifier !== undefined && clause !== undefined && !clause.isTypeOnly) {
        const line = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
        if (clause.name !== undefined) {
          found.push({ specifier, imported: 'default', local: clause.name.text, line });
        }
        const bindings = clause.namedBindings;
        if (bindings !== undefined && ts.isNamespaceImport(bindings)) {
          found.push({ specifier, imported: '*', local: bindings.name.text, line });
        } else if (bindings !== undefined && ts.isNamedImports(bindings)) {
          for (const element of bindings.elements) {
            if (element.isTypeOnly) continue;
            found.push({
              specifier,
              imported: (element.propertyName ?? element.name).text,
              local: element.name.text,
              line,
            });
          }
        }
      }
    }
    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return found;
}

/** One re-export that hands another module's values straight on. */
export interface ReExport {
  /** The module the values come from, as written. */
  readonly specifier: string;
  /** The exported names, or `*` for `export * from`. */
  readonly names: readonly string[];
  readonly line: number;
}

/**
 * Every re-export of another module's values, type-only ones excluded.
 *
 * Reported apart from {@link importedValues} because a re-export binds nothing locally and
 * yet is the more interesting half for a rule that matches on specifiers: it **launders**
 * one. Whoever imports the re-exporting module names *it*, not the module it came from, so a
 * rule keyed on the original specifier stops seeing anything.
 */
export function reExports(file: string, source: string): ReExport[] {
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    scriptKindFor(file),
  );

  const found: ReExport[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isExportDeclaration(node) && node.moduleSpecifier !== undefined && !node.isTypeOnly) {
      const specifier = literalText(node.moduleSpecifier);
      if (specifier !== undefined) {
        const clause = node.exportClause;
        const names =
          clause === undefined
            ? ['*']
            : ts.isNamedExports(clause)
              ? clause.elements
                  .filter((element) => !element.isTypeOnly)
                  .map((element) => (element.propertyName ?? element.name).text)
              : [clause.name.text];
        if (names.length > 0) {
          found.push({
            specifier,
            names,
            line: sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1,
          });
        }
      }
    }
    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return found;
}
