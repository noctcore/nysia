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
 * can diff it against upstream. `js-yaml` and `minimatch` are pinned to the versions the
 * action itself depends on (`js-yaml@4.1.0`, `minimatch@9.0.5`, from its package.json at
 * the same commit), because the parse and the glob semantics are the behaviour under test.
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
  const raw = config['changed-files'];
  if (raw === undefined || raw === null || (Array.isArray(raw) && raw.length === 0)) return {};

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
  if (!isObject(configObject)) return labelMap;

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
          throw new UnsupportedLabelerConfig(`An unknown config option was under ${label}: ${key}`);
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
    if (changedFiles.find((f) => matcher.match(f)) !== undefined) return true;
  }
  return false;
}

function checkIfAllGlobsMatchAnyFile(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const changedFile of changedFiles) {
    const mismatched = matchers.find((m) => !m.match(changedFile));
    if (mismatched !== undefined) continue;
    return true;
  }
  return false;
}

function checkIfAnyGlobMatchesAllFiles(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const matcher of matchers) {
    const mismatched = changedFiles.find((f) => !matcher.match(f));
    if (mismatched !== undefined) continue;
    return true;
  }
  return false;
}

function checkIfAllGlobsMatchAllFiles(changedFiles: string[], globs: string[]): boolean {
  const matchers = globs.map((g) => new Minimatch(g, { dot: DOT }));
  for (const changedFile of changedFiles) {
    const mismatched = matchers.find((m) => !m.match(changedFile));
    if (mismatched !== undefined) return false;
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
