/**
 * What `.github/labeler.yml` actually labels, asserted by running the labeler's own logic.
 *
 * This replaces a hand-written scanner that checked the config's *spelling*. That scanner
 * was rewritten three times and was blind to a new spelling each round — a flow sequence, a
 * trailing comment on a key line, a multi-line flow sequence — and every time it reported
 * success while the real action applied an area label to nearly every pull request. The
 * reviewer found each one by transcribing the action and running it, which is what this
 * file now does in the repository, on every `pnpm test`.
 *
 * A test over outcomes cannot be fooled by a spelling, because it never looks at one: the
 * config is parsed by the same `js-yaml` the action uses and matched by the same
 * `minimatch`. Whatever a future editor writes, if it over-labels, these cases go red.
 *
 * See `upstream.ts` for the transcription and the commit it came from.
 */

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import yaml from 'js-yaml';
import { describe, expect, it } from 'vitest';

import { DOT, PINNED_SHA, getLabelConfigMapFromObject, labelsFor } from './upstream.ts';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (relative: string): string => readFileSync(join(repoRoot, relative), 'utf8');

const labelerSource = read('.github/labeler.yml');
const labelConfigs = getLabelConfigMapFromObject(yaml.load(labelerSource));
const labels = (...files: string[]): string[] => labelsFor(labelConfigs, files);

/**
 * Representative pull requests, one per area the config claims to know about, plus the
 * shapes that have actually broken it: a pull request touching nothing, and pull requests
 * touching only an excluded path.
 *
 * Each asserts the EXACT set. An over-labelling config fails not because a particular
 * negation was spelled wrongly but because a label came out that should not have.
 */
describe('the labels .github/labeler.yml produces', () => {
  it.each([
    ['a pty-only change', ['crates/nysia-core/src/pty/mod.rs'], ['area:pty']],
    ['a vt-only change', ['crates/nysia-core/src/vt/mod.rs'], ['area:vt']],
    ['a git flat-module change', ['crates/nysia-core/src/git.rs'], ['area:git']],
    ['a worktree change', ['crates/nysia-core/src/worktree.rs'], ['area:git']],
    ['an rpc change', ['crates/nysia-core/src/rpc/mod.rs'], ['area:daemon']],
    ['a daemon binary change', ['crates/nysia/src/main.rs'], ['area:daemon']],
    ['a core module no narrower area owns', ['crates/nysia-core/src/store.rs'], ['area:daemon']],
    ['a proto change', ['crates/nysia-proto/src/lib.rs'], ['area:proto']],
    ['a generated-bindings-only change', ['apps/web/src/generated/SessionId.ts'], ['area:proto']],
    ['a transport-only change', ['apps/web/src/transport/channel.ts'], ['area:desktop']],
    ['a desktop shell change', ['apps/desktop/src-tauri/src/main.rs'], ['area:desktop']],
    ['a web-only change', ['apps/web/src/App.tsx'], ['area:web']],
    ['a docs-only change', ['docs/plans/v0.1-delivery-plan.md'], []],
    ['a design-doc change', ['docs/design/design-spec.md'], ['design-system']],
    ['a Cargo.lock-only change', ['Cargo.lock'], ['area:build', 'dependencies']],
    ['a root manifest change', ['package.json'], ['area:build', 'dependencies']],
    ['a crate manifest change', ['crates/nysia-core/Cargo.toml'], ['area:daemon', 'dependencies']],
    ['a gate change', ['scripts/prove-lint-meta.ts'], ['area:build', 'gate']],
    ['a workflow change', ['.github/workflows/ci.yml'], ['area:build']],
    [
      'a multi-area change',
      ['crates/nysia-core/src/pty/mod.rs', 'apps/web/src/App.tsx', 'docs/design/design-spec.md'],
      ['area:pty', 'area:web', 'design-system'],
    ],
    // The web exclusions, per file rather than across the pull request: a hand-written
    // component beside a generated binding is still a web change.
    [
      'web code beside a generated binding',
      ['apps/web/src/App.tsx', 'apps/web/src/generated/SessionId.ts'],
      ['area:proto', 'area:web'],
    ],
    [
      'web code beside a transport file',
      ['apps/web/src/App.tsx', 'apps/web/src/transport/channel.ts'],
      ['area:desktop', 'area:web'],
    ],
    // The shape that every over-labelling bug has produced. A pull request touching
    // nothing must be labelled nothing; each of the three scanner defects labelled it.
    ['an empty pull request', [], []],
  ])('%s -> %j', (_what, files, expected) => {
    expect(labels(...files)).toEqual(expected);
  });
});

/*
 * Two invariants over the whole config, rather than over the sampled file sets.
 */
describe('the labeler config as a whole', () => {
  const declared = ((): Set<string> => {
    const parsed = yaml.load(read('.github/labels.yml'));
    if (!Array.isArray(parsed)) throw new Error('.github/labels.yml is not a list of labels');
    return new Set(parsed.map((entry) => (entry as { name: string }).name));
  })();

  it('writes no type label', async () => {
    // The list lives in .github/scripts/facets.mjs so there is one source of truth, and is
    // imported dynamically because tsc does not resolve across into that tree.
    const facets = (await import(
      pathToFileURL(join(repoRoot, '.github/scripts/facets.mjs')).href
    )) as { TYPE_LABELS: readonly string[] };

    // Types are editorial — only the author knows whether a change is a bug or a chore —
    // and pr-facets.yml requires exactly one, so a type written here collides with the
    // author's own and fails the pull request intermittently.
    const written = [...labelConfigs.keys()].filter((l) => facets.TYPE_LABELS.includes(l));
    expect(written).toEqual([]);
  });

  it('writes only labels that exist in the repository', () => {
    const undeclared = [...labelConfigs.keys()].filter((l) => !declared.has(l));
    expect(undeclared).toEqual([]);
  });

  it('knows about every label it can write', () => {
    // Guards the guard: if the config parsed to nothing, every assertion above would pass
    // vacuously. It has eleven labels today and cannot legitimately reach zero.
    expect(labelConfigs.size).toBeGreaterThan(5);
  });
});

/*
 * The transcription can only be trusted while it describes the action that actually runs.
 */
describe('the transcription matches what the workflow pins', () => {
  const workflow = read('.github/workflows/pr-triage.yml');

  it('is pinned to the SHA scripts/labeler/upstream.ts was transcribed from', () => {
    const pinned = /actions\/labeler@([0-9a-f]{40})/.exec(workflow);
    expect(pinned?.[1]).toBe(PINNED_SHA);
  });

  it('runs with the dot default this simulation assumes', () => {
    // `dot: true` is v5's default and is what makes `.github/**` matchable. If a future
    // edit sets it explicitly, this simulation's constant has to be revisited with it.
    expect(workflow).not.toMatch(/^\s*dot:/m);
    expect(DOT).toBe(true);
  });
});
