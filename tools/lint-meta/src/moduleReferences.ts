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
      /** The options say every module comes back as source text. */
      readonly raw: boolean;
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

/** The cooked text of a string or no-substitution template literal, if that is what it is. */
function literalText(node: ts.Node | undefined): string | undefined {
  return node !== undefined && ts.isStringLiteralLike(node) ? node.text : undefined;
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
  const name = expression.name.text;
  if (name !== 'glob' && name !== 'globEager') return false;

  const target = expression.expression;
  return (
    ts.isMetaProperty(target) &&
    target.keywordToken === ts.SyntaxKind.ImportKeyword &&
    target.name.text === 'meta'
  );
}

/**
 * Do a glob call's options say the modules come back as source text?
 *
 * Every spelling Vite has for it, read off the options object rather than searched for in
 * the call's text: `query: '?raw'`, the same without the `?`, the object form
 * `query: { raw: '' }`, and the older `as: 'raw'`. There is no per-pattern spelling — a
 * query cannot be written into a glob pattern, where `?` is the single-character wildcard —
 * and an earlier version of this rule documented one that does not exist, which is how it
 * came to read `?` as a query separator in patterns too.
 */
function optionsSayRaw(options: ts.Expression | undefined): boolean {
  if (options === undefined || !ts.isObjectLiteralExpression(options)) return false;

  for (const property of options.properties) {
    if (!ts.isPropertyAssignment(property)) continue;
    const key = propertyName(property);
    const value = property.initializer;

    if (key === 'as' && literalText(value) === 'raw') return true;
    if (key !== 'query') continue;

    const text = literalText(value);
    if (text !== undefined && text.replace(/^\?/, '') === 'raw') return true;
    if (
      ts.isObjectLiteralExpression(value) &&
      value.properties.some((entry) => propertyName(entry) === 'raw')
    ) {
      return true;
    }
  }
  return false;
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
          raw: optionsSayRaw(node.arguments[1]),
        });
      }
    }

    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
  return found;
}
