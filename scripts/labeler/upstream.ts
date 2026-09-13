/**
 * The matching algorithm of `actions/labeler`, transcribed.
 *
 * ---------------------------------------------------------------------------------------
 * SOURCE: https://github.com/actions/labeler
 * COMMIT: 8558fd74291d67161a8a78ce36a881fa63b766a9  (v5.0.0)
 * FILES:  src/api/get-label-configs.ts, src/changedFiles.ts, src/utils.ts, src/labeler.ts
 * LICENCE: MIT — Copyright (c) 2018 GitHub, Inc. and contributors.
 *
 * Transcribed rather than vendored because the originals import `@actions/core` and
 * `@actions/github` for logging and API access, neither of which this repository has or
 * wants. Only the pure decision logic is reproduced, and it is reproduced structurally —
 * the same functions, the same order of checks, the same early returns — so that a reader
 * can diff it against upstream.
 *
 * `js-yaml@4.1.0` and `minimatch@9.0.3` are pinned to the versions the action's own
 * **lockfile** resolves at this commit, which is what its bundle ships. Its `package.json`
 * declares a caret range for minimatch, so the manifest alone does not pin it; the lockfile
 * is the thing to check this against. The parse and the glob semantics are the behaviour
 * under test, so these two are pinned exactly and not by range.
 *
 * DEVIATIONS FROM UPSTREAM. Two are cosmetic and cannot change a result:
 * `m.toUpperCase().charAt(1)` for `m.toUpperCase()[1]`, and a `find` bound to a local
 * instead of a `findIndex` plus an index access — both forced by this repository's
 * `noUncheckedIndexedAccess`, both noted at the line.
 *
 * Five are deliberate and DO change a result, every one of them in the fail-closed
 * direction. Upstream's behaviour in each of these cases is to carry on with a label that
 * can never be applied — a dead rule that reads as working config — and this file throws
 * instead, so a config the action would quietly ignore is a loud error here:
 *
 *   1. `changed-files:` that upstream would swallow (a mapping, an empty list, a scalar
 *      with no length). Upstream returns an empty match config. See below.
 *   2. An unrecognised top-level key under a label. Upstream logs it with `core.info` and
 *      continues, leaving the label with no conditions. See `getLabelConfigMapFromObject`.
 *   3. `head-branch` / `base-branch`, which are not transcribed at all. See `toMatchConfig`.
 *   4. A top-level config that is not a mapping. Upstream iterates a list's indices as if
 *      they were label names, and throws its own error for a scalar. See
 *      `getLabelConfigMapFromObject`.
 *   5. A match entry that is not a mapping. Upstream reaches `in` on it and dies with a
 *      bare TypeError; this throws a named error naming the entry. See `toMatchConfig`.
 *
 * A reader diffing this against upstream will find those five; they are here so that the
 * diff is expected rather than a surprise. None can make this file accept a config the
 * action would reject.
 *
 * WHAT THIS FILE STILL CANNOT SEE. Nine spellings leave a declared label silently dead
 * without diverging from upstream at all — `any:`/`all:` written as a mapping, a scalar or
 * null, an empty `any: []`/`all: []`, an empty rule list, entries that are only null or an
 * empty mapping, an empty glob list. Upstream is equally silent on every one, so there is
 * nothing to fail closed *against*; the transcription is faithful and the label is still
 * dead. That gap is closed in `labeler.test.ts` by two invariants over the config as a
 * whole rather than here — see `describe('no declared label is silently dead')`.
 *
 * `PINNED_SHA` below is checked against `.github/workflows/pr-triage.yml` by
 * `labeler.test.ts`. Bumping the action without revisiting this file fails that test,
 * which is the only thing stopping the simulation from drifting away from what runs.
 * ---------------------------------------------------------------------------------------
 *
 * WHY THIS EXISTS. `.github/labeler.yml` was guarded by a hand-written scanner that
 * asserted things about the *spelling* of the config. Three rounds of review found three
 * more spellings it was blind to — a flow sequence, a trailing comment on a key line, a
 * multi-line flow sequence — each of which let a negated glob through and made the labeler
 * apply an area label to nearly every pull request. Every round the scanner reported
 * success. What matters is not how the config is spelled but which labels come out, so
 * this asserts that instead, by running the same code the action runs.
 */

import { Minimatch } from 'minimatch';

/** The commit `.github/workflows/pr-triage.yml` must pin `actions/labeler` to. */
export const PINNED_SHA = '8558fd74291d67161a8a78ce36a881fa63b766a9';

/**
 * The action's `dot` input defaults to `true` in v5, and pr-triage.yml does not set it —
 * which is what makes `.github/**` matchable at all. `labeler.test.ts` asserts the
 * workflow still leaves it unset.
 */
export const DOT = true;

/**
 * utils.ts.
 *
 * Upstream writes the callback as `m.toUpperCase()[1]`. `charAt` is the same operation
 * with a total return type, which this repository's `noUncheckedIndexedAccess` requires;
 * the match is always two characters, so the two cannot differ.
 */
const kebabToCamel = (str: string): string => str.replace(/-./g, (m) => m.toUpperCase().charAt(1));

/** utils.ts */
function isObject(obj: unknown): obj is object {
  return obj !== null && typeof obj === 'object' && !Array.isArray(obj);
}

interface ChangedFilesGlobPatternsConfig {
  anyGlobToAnyFile?: string[];
  anyGlobToAllFiles?: string[];
  allGlobsToAnyFile?: string[];
  allGlobsToAllFiles?: string[];
}

interface BaseMatchConfig {
  changedFiles?: ChangedFilesGlobPatternsConfig[];
}

interface MatchConfig {
  all?: BaseMatchConfig[];
  any?: BaseMatchConfig[];
}

const ALLOWED_FILES_CONFIG_KEYS = [
  'any-glob-to-any-file',
  'any-glob-to-all-files',
  'all-globs-to-any-file',
  'all-globs-to-all-files',
];

const ALLOWED_CONFIG_KEYS = ['changed-files', 'head-branch', 'base-branch'];

/** A config this transcription cannot faithfully simulate. Never swallowed. */
export class UnsupportedLabelerConfig extends Error {}

type Unknown = Record<string, unknown>;

/**
 * changedFiles.ts — `toChangedFilesMatchConfig`.
 *
 * The subtlety that the old scanner never captured: every key becomes its **own** entry in
 * the list. So a single mapping with both `any-glob-to-any-file` and
 * `all-globs-to-all-files` is two entries, and whether they are AND-ed or OR-ed is then
 * decided by `any:`/`all:` above them — not by their sharing a mapping.
 */
function toChangedFilesMatchConfig(config: Unknown): BaseMatchConfig {
  // The key being absent is ordinary — an entry can carry other conditions — and upstream
  // and this agree that it contributes nothing.
  if (!('changed-files' in config)) return {};

  const raw = config['changed-files'];

  // DEVIATION (fail-closed). Upstream guards with `!config['changed-files'] ||
  // !config['changed-files'].length`. A YAML *mapping* is truthy and has no `length`, so
  // upstream silently returns an empty match config and the label is never applied — a
  // dead rule that looks like working config. An earlier version of this file rejected
  // only undefined, null and the empty array, so a mapping fell through and was evaluated
  // normally: writing `area:web`'s globs under a mapping instead of a list left this
  // suite 31 of 31 green while the real action labelled nothing.
  //
  // Throwing rather than mirroring the swallow is the deliberate choice. Mirroring would
  // make the exact-set assertions go red, which is enough for a label this test covers;
  // throwing also catches the case for a label it does not, and turns "this rule quietly
  // does nothing" into a message naming the label. It can only be more strict than the
  // action, never less.
  const length = (raw as { length?: unknown } | null | undefined)?.length;
  if (!raw || length === undefined || length === 0) {
    const describe = (): string => {
      if (Array.isArray(raw)) return 'an empty list';
      if (raw === null || raw === undefined) return 'empty';
      if (typeof raw === 'object') return 'a mapping';
      return `a ${typeof raw}`;
    };
    throw new UnsupportedLabelerConfig(
      `A "changed-files:" value must be a non-empty list of glob-key mappings; this one is ${describe()}. ` +
        'actions/labeler silently treats it as no condition at all, so the label would never ' +
        'be applied and nothing would say so. Write it as a list: "- any-glob-to-any-file: [...]".',
    );
  }

  const changedFilesConfigs: unknown[] = Array.isArray(raw) ? raw : [raw];
  const valid: ChangedFilesGlobPatternsConfig[] = [];

  for (const entry of changedFilesConfigs) {
    if (!isObject(entry)) {
      throw new UnsupportedLabelerConfig(
        'The "changed-files" section must have a valid config structure.',
      );
    }
    const keys = Object.keys(entry);
    const invalid = keys.filter((k) => !ALLOWED_FILES_CONFIG_KEYS.includes(k));
    if (invalid.length > 0) {
      throw new UnsupportedLabelerConfig(
        `Unknown config options were under "changed-files": ${invalid.join(', ')}`,
      );
    }
    for (const key of keys) {
      const value = (entry as Unknown)[key];
      valid.push({
        [kebabToCamel(key)]: Array.isArray(value) ? value : [value],
      } as ChangedFilesGlobPatternsConfig);
    }
  }

  return { changedFiles: valid };
}

/**
 * get-label-configs.ts — `toMatchConfig`, minus the branch half.
 *
 * Upstream also merges `toBranchMatchConfig`. This repository's config uses no branch
 * keys, and rather than reproduce that half untested, a branch key throws: a silently
 * ignored condition would make the simulation disagree with the action, which is the one
 * failure this file exists to prevent.
 */
function toMatchConfig(config: Unknown): BaseMatchConfig {
  // `in` on a string or a number is a TypeError, so an entry like `any: ['nonsense']`
  // used to fail closed by accident with a message naming neither the problem nor this
  // file. Fail closed on purpose instead.
  if (!isObject(config)) {
    throw new UnsupportedLabelerConfig(
      `A match entry must be a mapping; found ${config === null ? 'null' : typeof config}.`,
    );
  }
  if ('head-branch' in config || 'base-branch' in config) {
    throw new UnsupportedLabelerConfig(
      'head-branch / base-branch matching is not transcribed; extend scripts/labeler/upstream.ts before using it.',
    );
  }
  return toChangedFilesMatchConfig(config);
}

/**
 * get-label-configs.ts — `getLabelConfigMapFromObject`.
 *
 * Note the `indexOfAny` behaviour, reproduced exactly: several bare `changed-files` items
 * written as separate list entries all collapse into a single `any` array, so they are
 * OR-ed. That is why a negation written as its own entry needs an `all:` wrapper, and it
 * is decided here rather than anywhere a line scanner could see.
 */
export function getLabelConfigMapFromObject(configObject: unknown): Map<string, MatchConfig[]> {
  const labelMap = new Map<string, MatchConfig[]>();

  // DEVIATION (fail-closed), deviation 4. Upstream runs `for (const label in configObject)`:
  // a top-level list iterates its indices and can label a pull request under numeric label
  // names, and a scalar throws its own "unexpected type" error. This used to return an empty
  // map for both, which is the loosening direction and was caught only by the size guard
  // below. A labeler config is a mapping of label to rules; anything else is a mistake.
  if (!isObject(configObject)) {
    throw new UnsupportedLabelerConfig(
      `A labeler config must be a mapping of label name to rules; found ${
        Array.isArray(configObject) ? 'a list' : configObject === null ? 'null' : typeof configObject
      }.`,
    );
  }

  for (const [label, configOptions] of Object.entries(configObject as Unknown)) {
    if (!Array.isArray(configOptions) || !configOptions.every((o) => typeof o === 'object')) {
      throw new UnsupportedLabelerConfig(
        `found unexpected type for label '${label}' (should be array of config options)`,
      );
    }

    const matchConfigs: MatchConfig[] = [];
    for (const configValue of configOptions) {
      if (!configValue) continue;

      for (const [key, value] of Object.entries(configValue as Unknown)) {
        if (key === 'any' || key === 'all') {
          if (Array.isArray(value)) {
            matchConfigs.push({ [key]: value.map((v) => toMatchConfig(v as Unknown)) });
          }
        } else if (ALLOWED_CONFIG_KEYS.includes(key)) {
          const newMatchConfig = toMatchConfig({ [key]: value });
          // Upstream indexes with `findIndex` and pushes straight into the result. Bound
          // to a local here only because `noUncheckedIndexedAccess` types an index access
          // as possibly undefined; `>= 0` already guarantees it is not.
          const existingAny = matchConfigs.find((mc) => Boolean(mc.any));
          if (existingAny !== undefined) {
            existingAny.any?.push(newMatchConfig);
          } else {
            matchConfigs.push({ any: [newMatchConfig] });
          }
        } else {
          // DEVIATION (fail-closed), deviation 2 of the three listed in the file header.
          // Upstream calls `core.info` here and carries on, which leaves the label with no
          // conditions at all — so a typo in a key name produces a rule that can never
          // apply and says nothing. A misspelled key is never intentional, and a loud
          // error costs one line to fix where a dead rule costs a release to notice.
          throw new UnsupportedLabelerConfig(
            `An unknown config option was under ${label}: ${key}. actions/labeler would log ` +
              'this and continue, leaving the label with no conditions and no warning.',
          );
        }
      }
    }

    if (matchConfigs.length > 0) labelMap.set(label, matchConfigs);
  }

  return labelMap;
}

/* changedFiles.ts — the four glob primitives, each kept in upstream's shape. */

function checkIfAnyGlobMatchesAnyFile(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const matcher of matchers) {
    // Upstream's own truthiness test, not `!== undefined`. They differ only when a changed
    // file is the empty string, which the API cannot return — but mirroring costs nothing
    // and an exactly-faithful line needs no caveat in the header.
    const matchedFile = changedFiles.find((f) => matcher.match(f));
    if (matchedFile) return true;
  }
  return false;
}

function checkIfAllGlobsMatchAnyFile(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const changedFile of changedFiles) {
    const mismatchedGlob = matchers.find((m) => !m.match(changedFile));
    if (mismatchedGlob) continue;
    return true;
  }
  return false;
}

function checkIfAnyGlobMatchesAllFiles(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const matcher of matchers) {
    const mismatchedFile = changedFiles.find((f) => !matcher.match(f));
    if (mismatchedFile) continue;
    return true;
  }
  return false;
}

function checkIfAllGlobsMatchAllFiles(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const changedFile of changedFiles) {
    const mismatchedGlob = matchers.find((m) => !m.match(changedFile));
    if (mismatchedGlob) return false;
  }
  return true;
}

const PRIMITIVES: [keyof ChangedFilesGlobPatternsConfig, (f: string[], g: string[]) => boolean][] = [
  ['anyGlobToAnyFile', checkIfAnyGlobMatchesAnyFile],
  ['anyGlobToAllFiles', checkIfAnyGlobMatchesAllFiles],
  ['allGlobsToAnyFile', checkIfAllGlobsMatchAnyFile],
  ['allGlobsToAllFiles', checkIfAllGlobsMatchAllFiles],
];

/** changedFiles.ts — true as soon as one glob config matches. */
function checkAnyChangedFiles(changedFiles: string[], configs: ChangedFilesGlobPatternsConfig[]): boolean {
  for (const config of configs) {
    for (const [key, fn] of PRIMITIVES) {
      const globs = config[key];
      if (globs !== undefined && fn(changedFiles, globs)) return true;
    }
  }
  return false;
}

/** changedFiles.ts — false on the first glob config that does not match. */
function checkAllChangedFiles(changedFiles: string[], configs: ChangedFilesGlobPatternsConfig[]): boolean {
  for (const config of configs) {
    for (const [key, fn] of PRIMITIVES) {
      const globs = config[key];
      if (globs !== undefined && !fn(changedFiles, globs)) return false;
    }
  }
  return true;
}

/* labeler.ts — checkAny / checkAll / checkMatch / checkMatchConfigs. */

function checkAny(matchConfigs: BaseMatchConfig[], changedFiles: string[]): boolean {
  if (matchConfigs.length === 0 || !matchConfigs.some((c) => Object.keys(c).length > 0)) return false;
  for (const matchConfig of matchConfigs) {
    if (matchConfig.changedFiles !== undefined) {
      if (checkAnyChangedFiles(changedFiles, matchConfig.changedFiles)) return true;
    }
  }
  return false;
}

function checkAll(matchConfigs: BaseMatchConfig[], changedFiles: string[]): boolean {
  if (matchConfigs.length === 0 || !matchConfigs.some((c) => Object.keys(c).length > 0)) return false;
  for (const matchConfig of matchConfigs) {
    if (matchConfig.changedFiles !== undefined) {
      // An empty pull request matches nothing under `all`. Upstream checks this before
      // the globs, which is why an empty PR cannot be labelled through an `all:` block.
      if (changedFiles.length === 0) return false;
      if (!checkAllChangedFiles(changedFiles, matchConfig.changedFiles)) return false;
    }
  }
  return true;
}

function checkMatch(changedFiles: string[], matchConfig: MatchConfig): boolean {
  if (Object.keys(matchConfig).length === 0) return false;
  if (matchConfig.all !== undefined && !checkAll(matchConfig.all, changedFiles)) return false;
  if (matchConfig.any !== undefined && !checkAny(matchConfig.any, changedFiles)) return false;
  return true;
}

/** labeler.ts — every top-level match object must hold. */
function checkMatchConfigs(changedFiles: string[], matchConfigs: MatchConfig[]): boolean {
  for (const config of matchConfigs) {
    if (!checkMatch(changedFiles, config)) return false;
  }
  return true;
}

/**
 * The labels this config produces for a pull request touching `changedFiles`.
 *
 * `sync-labels` is off in pr-triage.yml, so upstream unions these with the labels already
 * on the pull request and removes nothing. This returns only what the config *adds*, which
 * is the part the config decides; sorted so a test can compare sets directly.
 */
export function labelsFor(labelConfigs: Map<string, MatchConfig[]>, changedFiles: string[]): string[] {
  const applied: string[] = [];
  for (const [label, configs] of labelConfigs) {
    if (checkMatchConfigs(changedFiles, configs)) applied.push(label);
  }
  return applied.sort();
}
