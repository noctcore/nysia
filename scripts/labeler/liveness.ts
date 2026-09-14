/**
 * Is every rule in `.github/labeler.yml` doing something?
 *
 * The invariant added in #23 is per *label*: a label that the file declares must survive
 * parsing, and must appear in at least one sampled expectation. Both are necessary and
 * neither can see inside a label. A dead rule sitting beside a live one is invisible to
 * them, and three shapes proved it — a live entry beside one whose `any:` is an empty
 * mapping, a live glob key beside a second key with an empty glob list, and a live entry
 * beside a second whose path is misspelled. Each left the suite green while the second rule
 * could never fire (#24).
 *
 * The check here is a mutation: take the config apart into the smallest pieces a person
 * writes — a rule entry, a match entry, a glob-key mapping, one glob — remove each in turn,
 * and re-run the sample table. If the labels come out the same for every sample, that piece
 * decided nothing, and the reader who wrote it believes it did. Removing it may also make
 * the config unparseable, which counts as load-bearing: it is still holding something up.
 *
 * This is a coverage requirement as much as a correctness one, and deliberately so. It can
 * be satisfied by fixing the rule *or* by sampling the pull request that the rule was
 * written for, and the second is usually what is missing — which is the point, because the
 * sample table is the thing that does the catching.
 */

import { getLabelConfigMapFromObject, labelsFor } from './upstream.ts';

/** A labeler config, and the piece of it that was taken out. */
export interface Component {
  /** The path to it, spelled the way a reader would find it in the file. */
  readonly where: string;
  /** The whole config with that piece removed. */
  readonly without: unknown;
}

/** `[what it is, the files it touches, the labels it must produce]`. */
export type Sample = readonly [string, readonly string[], readonly string[]];

type Unknown = Record<string, unknown>;

function isMapping(value: unknown): value is Unknown {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

/** Delete the array element or object key at `path`, in place. */
function removeAt(root: unknown, path: readonly (string | number)[]): void {
  let parent: unknown = root;
  for (const step of path.slice(0, -1)) {
    if (Array.isArray(parent)) parent = parent[step as number];
    else if (isMapping(parent)) parent = parent[step as string];
    else return;
  }
  const last = path[path.length - 1];
  if (last === undefined) return;
  if (Array.isArray(parent)) parent.splice(last as number, 1);
  else if (isMapping(parent)) delete parent[last as string];
}

/** The value at `path`, or undefined if the shape does not go that deep. */
function valueAt(root: unknown, path: readonly (string | number)[]): unknown {
  let here: unknown = root;
  for (const step of path) {
    if (Array.isArray(here)) here = here[step as number];
    else if (isMapping(here)) here = here[step as string];
    else return undefined;
  }
  return here;
}

/**
 * Every piece of the config that can be removed on its own, deepest last.
 *
 * The shapes walked into are the ones a labeler config has: a label holds a list of rule
 * entries; an entry holds `changed-files` directly or an `any:`/`all:` list of match entries
 * that do; a `changed-files` list holds glob-key mappings; a glob key holds globs. A value
 * of the wrong shape is not walked into — it is still yielded as a component, which is what
 * catches `any:` written as a mapping.
 */
export function components(config: unknown): Component[] {
  const found: Component[] = [];
  if (!isMapping(config)) return found;

  const take = (path: readonly (string | number)[], where: string): void => {
    const without = clone(config);
    removeAt(without, path);
    found.push({ where, without });
  };

  const walkChangedFiles = (base: readonly (string | number)[], label: string): void => {
    const changedFiles = valueAt(config, base);
    if (!Array.isArray(changedFiles)) return;

    changedFiles.forEach((globMap, k) => {
      take([...base, k], `${label} > changed-files[${k}]`);
      if (!isMapping(globMap)) return;

      for (const globKey of Object.keys(globMap)) {
        take([...base, k, globKey], `${label} > changed-files[${k}] > ${globKey}`);

        const globs = globMap[globKey];
        if (!Array.isArray(globs)) continue;
        globs.forEach((glob, m) => {
          take([...base, k, globKey, m], `${label} > ${globKey} > ${JSON.stringify(glob)}`);
        });
      }
    });
  };

  for (const [label, rules] of Object.entries(config)) {
    if (!Array.isArray(rules)) continue;

    rules.forEach((entry, i) => {
      take([label, i], `${label} > entry ${i}`);
      if (!isMapping(entry)) return;

      for (const key of Object.keys(entry)) {
        take([label, i, key], `${label} > entry ${i} > ${key}`);

        if (key === 'changed-files') {
          walkChangedFiles([label, i, key], label);
          continue;
        }
        if (key !== 'any' && key !== 'all') continue;

        const matchEntries = entry[key];
        if (!Array.isArray(matchEntries)) continue;
        matchEntries.forEach((_matchEntry, j) => {
          take([label, i, key, j], `${label} > ${key}[${j}]`);
          walkChangedFiles([label, i, key, j, 'changed-files'], label);
        });
      }
    });
  }

  return found;
}

/** What a config labels each sample, or the error it refuses to parse with. */
function outcomes(config: unknown, samples: readonly Sample[]): string {
  let labelConfigs;
  try {
    labelConfigs = getLabelConfigMapFromObject(config);
  } catch {
    // A config the transcription refuses is not the same config, which is all this needs to
    // know. Whatever was removed was holding the file together.
    return 'unparseable';
  }
  return samples
    .map(([, files]) => labelsFor(labelConfigs, [...files]).join('+'))
    .join('|');
}

/**
 * The pieces of `config` that no sample notices the loss of.
 *
 * A piece is dead when the whole sample table produces the same labels without it. That is
 * either a rule that can never fire or a rule nothing exercises; both are the same defect
 * from the far side, because an unexercised rule is one nobody would notice breaking.
 */
export function deadComponents(config: unknown, samples: readonly Sample[]): string[] {
  const baseline = outcomes(config, samples);
  return components(config)
    .filter((component) => outcomes(component.without, samples) === baseline)
    .map((component) => component.where);
}
