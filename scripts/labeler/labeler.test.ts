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
 * Asserting outcomes rather than spellings removes that whole class — but not for free, and
 * an earlier version of this header claimed it did. "A test over outcomes cannot be fooled
 * by a spelling" was wrong: a spelling can still defeat it by making the *transcription*
 * disagree with the action, and one did. Writing `changed-files:` as a mapping rather than a
 * list left this suite green while the real action applied nothing, because upstream's guard
 * rejects a value with no `length` and the transcription's did not. The protection is only
 * ever as good as the transcription's fidelity, which is why the deviations are enumerated
 * in `upstream.ts` and why `describe('spellings that have defeated a guard before')` below
 * pins every one of them.
 *
 * See `upstream.ts` for the transcription, the commit it came from, and the three places it
 * deliberately fails closed where upstream carries on with a dead rule.
 */

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import yaml from 'js-yaml';
import { describe, expect, it } from 'vitest';

import {
  DOT,
  PINNED_SHA,
  UnsupportedLabelerConfig,
  getLabelConfigMapFromObject,
  labelsFor,
} from './upstream.ts';
import { components, deadComponents, type Sample } from './liveness.ts';

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
/**
 * The sampled pull requests, hoisted so the dead-rule invariant below can read them.
 * `[what, changed files, the exact labels expected]`.
 */
const SAMPLES: readonly Sample[] = [
  ['a pty-only change', ['crates/nysia-core/src/pty/mod.rs'], ['area:pty']],
  ['a vt-only change', ['crates/nysia-core/src/vt/mod.rs'], ['area:vt']],
  ['a git flat-module change', ['crates/nysia-core/src/git.rs'], ['area:git']],
  ['a worktree change', ['crates/nysia-core/src/worktree.rs'], ['area:git']],
  ['an rpc change', ['crates/nysia-core/src/rpc/mod.rs'], ['area:daemon']],
  ['a daemon binary change', ['crates/nysia/src/main.rs'], ['area:daemon']],
  ['a core module no narrower area owns', ['crates/nysia-core/src/store.rs'], ['area:daemon']],
  ['a proto change', ['crates/nysia-proto/src/lib.rs'], ['area:proto']],
  ['a generated-bindings-only change', ['apps/web/src/generated/SessionKind.ts'], ['area:proto']],
  ['a transport-only change', ['apps/web/src/transport/bridge.ts'], ['area:desktop']],
  ['a desktop shell change', ['apps/desktop/src-tauri/src/main.rs'], ['area:desktop']],
  ['a web-only change', ['apps/web/src/App.tsx'], ['area:web']],
  ['a docs-only change', ['docs/plans/v0.1-delivery-plan.md'], []],
  ['a design-spec change', ['docs/design/design-spec.md'], ['design-system']],

  // `design-system` means design spec adoption, and it used to match all of
  // `docs/design/**` — which is also where the architecture document lives. PR #38 was a
  // daemon-launch change that edited the architecture and came out labelled as adopting a
  // spec it had never read (#52). The glob now names the spec, and these are the two halves
  // of that: the architecture document on its own fires nothing, and neither does the rest
  // of the directory. The row above and the multi-area row below are the other half — the
  // rule still fires for the file it is about, which is also what keeps it off the dead-rule
  // list.
  ['an architecture-document-only change', ['docs/design/2026-09-13-nysia-architecture.md'], []],
  [
    'the design sources beside the spec, without the spec',
    ['docs/design/README.md', 'docs/design/Nysia-ADE.dc.html'],
    [],
  ],
  ['a Cargo.lock-only change', ['Cargo.lock'], ['area:build', 'dependencies']],
  ['a root manifest change', ['package.json'], ['area:build', 'dependencies']],
  ['a crate manifest change', ['crates/nysia-core/Cargo.toml'], ['area:daemon', 'dependencies']],
  ['a gate change', ['scripts/prove-lint-meta.ts'], ['area:build', 'gate']],
  ['a workflow change', ['.github/workflows/ci.yml'], ['area:build']],

  // The other spelling of every Rust module the config matches twice. A module is either
  // `foo.rs` or `foo/mod.rs` and changes over its life, so each area names both — and until
  // both were sampled, half of those globs and every exclusion that mirrors them decided
  // nothing, which is what `no rule in the config is dead` below now refuses (#24).
  ['a pty flat-module change', ['crates/nysia-core/src/pty.rs'], ['area:pty']],
  ['a vt flat-module change', ['crates/nysia-core/src/vt.rs'], ['area:vt']],
  ['a git directory-module change', ['crates/nysia-core/src/git/mod.rs'], ['area:git']],
  ['a worktree directory-module change', ['crates/nysia-core/src/worktree/mod.rs'], ['area:git']],

  // Every root file `area:build` enumerates, one pull request each. Enumerated rather than
  // matched with '*' because README.md and CLAUDE.md sit at the root too, and a list nobody
  // exercises is a list that can quietly lose an entry.
  ['a root cargo manifest change', ['Cargo.toml'], ['area:build', 'dependencies']],
  ['a pnpm lockfile change', ['pnpm-lock.yaml'], ['area:build', 'dependencies']],
  ['a pnpm workspace change', ['pnpm-workspace.yaml'], ['area:build', 'dependencies']],
  ['a toolchain pin change', ['rust-toolchain.toml'], ['area:build']],
  ['an editorconfig change', ['.editorconfig'], ['area:build']],
  ['a gitattributes change', ['.gitattributes'], ['area:build']],
  ['a gitignore change', ['.gitignore'], ['area:build']],
  ['an npmrc change', ['.npmrc'], ['area:build']],
  ['a clippy config change', ['clippy.toml'], ['area:build']],
  ['a rustfmt config change', ['rustfmt.toml'], ['area:build']],
  ['an eslint config change', ['eslint.config.js'], ['area:build']],
  ['a tsconfig change', ['tsconfig.json'], ['area:build']],
  ['a base tsconfig change', ['tsconfig.base.json'], ['area:build']],
  ['a vitest config change', ['vitest.config.ts'], ['area:build']],

  // The manifests `dependencies` matches that no root-file glob reaches, and the tooling
  // tree, which is `gate`'s other half.
  ['a lint-meta source change', ['tools/lint-meta/src/rules.ts'], ['area:build', 'gate']],
  [
    'a lint-meta manifest change',
    ['tools/lint-meta/package.json'],
    ['area:build', 'dependencies', 'gate'],
  ],
  ['a web package manifest change', ['apps/web/package.json'], ['area:web', 'dependencies']],
  [
    'the tauri crate manifest',
    ['apps/desktop/src-tauri/Cargo.toml'],
    ['area:desktop', 'dependencies'],
  ],

  [
    'a multi-area change',
    ['crates/nysia-core/src/pty/mod.rs', 'apps/web/src/App.tsx', 'docs/design/design-spec.md'],
    ['area:pty', 'area:web', 'design-system'],
  ],
  // The web exclusions, per file rather than across the pull request: a hand-written
  // component beside a generated binding is still a web change.
  [
    'web code beside a generated binding',
    ['apps/web/src/App.tsx', 'apps/web/src/generated/SessionKind.ts'],
    ['area:proto', 'area:web'],
  ],
  [
    'web code beside a transport file',
    ['apps/web/src/App.tsx', 'apps/web/src/transport/bridge.ts'],
    ['area:desktop', 'area:web'],
  ],
  // The shape that every over-labelling bug has produced. A pull request touching
  // nothing must be labelled nothing; each of the three scanner defects labelled it.
  ['an empty pull request', [], []],
];

describe('the labels .github/labeler.yml produces', () => {
  it.each(SAMPLES)('%s -> %j', (_what, files, expected) => {
    expect(labels(...files)).toEqual([...expected]);
  });
});

/*
 * Every spelling that has defeated a guard in this repository, pinned.
 *
 * The first five mean the same thing and must produce the same labels — that is the
 * spelling-invariance the outcome test is for. The last two are the ones where upstream
 * itself does something surprising, and where a transcription can silently disagree with it.
 */
describe('spellings that have defeated a guard before', () => {
  const WEB = ['apps/web/src/App.tsx'];
  const labelsOf = (source: string): string[] =>
    labelsFor(getLabelConfigMapFromObject(yaml.load(source)), WEB);

  it.each([
    ['a block list', "'area:web':\n  - changed-files:\n      - any-glob-to-any-file:\n          - 'apps/web/**'\n"],
    ['a single-line flow sequence', "'area:web':\n  - changed-files:\n      - any-glob-to-any-file: ['apps/web/**']\n"],
    [
      'a multi-line flow sequence',
      "'area:web':\n  - changed-files:\n      - any-glob-to-any-file: [\n          'apps/web/**'\n        ]\n",
    ],
    [
      'a trailing comment on the key line',
      "'area:web':\n  - changed-files:\n      - any-glob-to-any-file: # the web app\n          - 'apps/web/**'\n",
    ],
    [
      'the any-wrapper form',
      "'area:web':\n  - any:\n      - changed-files:\n          - any-glob-to-any-file:\n              - 'apps/web/**'\n",
    ],
    [
      'the legacy all-wrapper form',
      "'area:web':\n  - all:\n      - changed-files:\n          - any-glob-to-any-file:\n              - 'apps/web/**'\n",
    ],
  ])('%s labels a web file', (_what, source) => {
    expect(labelsOf(source)).toEqual(['area:web']);
  });

  // The one that got through. A mapping is truthy and has no `length`, so upstream's guard
  // turns it into no condition at all and the label is silently never applied. Rejected
  // loudly here rather than mirrored, so it fails for a label this file does not sample too.
  it('rejects a changed-files mapping instead of quietly labelling nothing', () => {
    const mapping =
      "'area:web':\n  - changed-files:\n      any-glob-to-any-file:\n        - 'apps/web/**'\n";
    expect(() => labelsOf(mapping)).toThrow(/silently treats it as no condition/);
  });

  // Not all of these are swallowed upstream, and the earlier title claimed they all were.
  // A NON-EMPTY string has a length, so upstream carries on and throws its own "valid
  // config structure" error for it; only a number, a boolean or the empty string is
  // swallowed, which is what upstream.ts's header says. All four are rejected here — what
  // differs is whether upstream would have been silent about it.
  it.each([
    ['an empty list, which upstream swallows', "'area:web':\n  - changed-files: []\n"],
    ['a bare scalar, which upstream rejects too', "'area:web':\n  - changed-files: 'apps/web/**'\n"],
    ['an explicit null, which upstream swallows', "'area:web':\n  - changed-files:\n"],
    ['a number, which upstream swallows', "'area:web':\n  - changed-files: 3\n"],
  ])('rejects %s', (_what, source) => {
    expect(() => labelsOf(source)).toThrow(UnsupportedLabelerConfig);
  });

  it('drops a label whose only entry is an empty mapping, leaving it dead', () => {
    // This case used to be titled "still ignores a changed-files key that is simply
    // absent" and called the legitimate case. It is not. Upstream, an entry with no
    // changed-files can carry a branch key instead, so emptiness there means "matched by
    // something else". In this transcription every other key throws, so an entry with
    // nothing in it is ALWAYS dead — and the test was enshrining a dead rule as correct.
    //
    // The behaviour is unchanged and faithful; what changed is that it is no longer
    // described as desirable, and `no declared label is silently dead` below now fails on
    // a real config written this way.
    expect(getLabelConfigMapFromObject({ 'area:web': [{}] }).size).toBe(0);
  });
});

/*
 * No declared label is silently dead.
 *
 * Nine spellings leave a label that a reader would call declared unable to ever fire, and
 * upstream is silent on every one of them — so `upstream.ts` has nothing to fail closed
 * against, and being faithful is not enough. Two halves cover them:
 *
 *   (1) every top-level key of the parsed YAML survives into the parsed label configs.
 *       Catches the seven where the label vanishes during parsing: `any:`/`all:` as a
 *       mapping, a scalar or null, an empty rule list, and entries that are only null or
 *       an empty mapping.
 *
 *   (2) every label the config can write appears in at least one sampled expected set.
 *       Catches the other two — `any: []` and `all: []` — plus an empty glob list, because
 *       a label that must appear in an expected set is a label the exact-set assertions
 *       above will notice failing to fire. It also closes the general case: a label nobody
 *       sampled cannot quietly stop working, because it cannot exist unsampled.
 *
 * Half (2) is the one that makes this finite test cover an infinite space of spellings. It
 * is a coverage requirement, not a behaviour assertion: it says the table above must keep
 * pace with the config, and the table is what does the catching.
 */
describe('no declared label is silently dead', () => {
  const rawKeys = Object.keys(yaml.load(labelerSource) as Record<string, unknown>);

  it('parses every label declared in the file', () => {
    const lost = rawKeys.filter((key) => !labelConfigs.has(key));
    expect(lost).toEqual([]);
  });

  it('samples every label the config can write', () => {
    const expected = new Set(SAMPLES.flatMap(([, , labels_]) => labels_));
    const unsampled = [...labelConfigs.keys()].filter((label) => !expected.has(label));
    expect(unsampled).toEqual([]);
  });

  it('has a sample table that cannot go stale silently', () => {
    // Both halves compare against the real file, so neither can pass on an empty input.
    expect(rawKeys.length).toBeGreaterThan(5);
    expect(SAMPLES.length).toBeGreaterThan(5);
  });

  it('samples no path containing a backslash', () => {
    // CI runs Windows and macOS; the action runs on Linux. minimatch's only platform
    // branch converts backslashes in the path being matched when `process.platform` is
    // win32, so a sampled path containing one would mean the Windows job and the action
    // disagree, and the macOS job alone would be carrying the fidelity claim. No sample
    // contains one today. This keeps that true rather than leaving it to be remembered.
    const withBackslash = SAMPLES.flatMap(([, files]) => files).filter((f) => f.includes('\\'));
    expect(withBackslash).toEqual([]);
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

/*
 * No rule in the config is dead — including one sitting beside a live one.
 *
 * The invariant above is per *label*, and #24 is what that misses. Three probes stayed
 * green while a second rule in the same label could never fire: an entry whose `any:` is an
 * empty mapping, a second glob key holding an empty list, and a second entry with a
 * misspelled path. In each the label kept firing through its live rule, so every per-label
 * check passed. `liveness.ts` takes the config apart instead — one rule entry, one glob key,
 * one glob at a time — removes each and re-runs the whole sample table. A piece nothing
 * notices the loss of is either dead or unexercised, and from the far side those are the
 * same defect: a rule nobody samples is one nobody would notice breaking.
 *
 * This is satisfied by fixing the rule *or* by sampling the pull request it was written for,
 * and the second is usually what is missing. The table above grew by twenty rows when this
 * invariant first ran, which is the honest measure of how much of that config was decorative.
 */
describe('no rule in the config is dead', () => {
  /**
   * The globs that decide nothing today, each because a catch-all beside them already does.
   *
   * `area:daemon` matches `crates/nysia-core/**` minus the four modules that have their own
   * label, so every rpc path is already a daemon path; naming rpc again cannot change an
   * outcome. Kept as documentation of which area owns the module rather than deleted, and
   * listed here so that a reader meets the fact rather than a green check.
   *
   * Asserted in BOTH directions below. If the catch-all ever narrows, these globs start
   * deciding something and this list goes stale in the direction that matters — so the
   * second test fails and the entry has to come out.
   */
  const SUBSUMED_BY_A_CATCH_ALL: readonly string[] = [
    'area:daemon > any-glob-to-any-file > "crates/nysia-core/src/rpc/**"',
    'area:daemon > any-glob-to-any-file > "crates/nysia-core/src/rpc.rs"',
  ];

  const parsed = yaml.load(labelerSource);
  const dead = deadComponents(parsed, SAMPLES);

  it('has no rule the sample table does not notice the loss of', () => {
    expect(dead.filter((d) => !SUBSUMED_BY_A_CATCH_ALL.includes(d))).toEqual([]);
  });

  it('lists nothing as subsumed that is actually load-bearing', () => {
    expect(SUBSUMED_BY_A_CATCH_ALL.filter((d) => !dead.includes(d))).toEqual([]);
  });

  it('takes the config apart into more than a handful of pieces', () => {
    // Guards the guard. If `components` stopped walking into the config, everything above
    // would pass vacuously — which is the failure this whole file exists to refuse.
    expect(components(parsed).length).toBeGreaterThan(40);
  });
});

/*
 * The three shapes from #24, each as its own synthetic config.
 *
 * The invariant is relative to a sample table, so a probe has to carry its own. Each is a
 * live rule plus a dead one in the same label — the arrangement every per-label check is
 * blind to — and each is paired with the same config minus the dead half, which must come
 * back clean. A probe that reports dead components for both would be measuring nothing.
 */
describe('the shapes a per-label invariant cannot see', () => {
  const WEB: readonly Sample[] = [['a web change', ['apps/web/src/App.tsx'], ['area:web']]];
  const live = { 'any-glob-to-any-file': ['apps/web/**'] };

  it('finds an entry whose any: is an empty mapping', () => {
    const config = { 'area:web': [{ 'changed-files': [live] }, { any: {} }] };
    expect(deadComponents(config, WEB)).toContain('area:web > entry 1');
    expect(deadComponents({ 'area:web': [{ 'changed-files': [live] }] }, WEB)).toEqual([]);
  });

  it('finds a second glob key holding an empty list', () => {
    const config = {
      'area:web': [{ 'changed-files': [live, { 'any-glob-to-all-files': [] }] }],
    };
    expect(deadComponents(config, WEB)).toContain('area:web > changed-files[1]');
    expect(deadComponents({ 'area:web': [{ 'changed-files': [live] }] }, WEB)).toEqual([]);
  });

  it('finds a second glob whose path is misspelled', () => {
    const config = {
      'area:web': [{ 'changed-files': [{ 'any-glob-to-any-file': ['apps/web/**', 'apps/wbe/**'] }] }],
    };
    expect(deadComponents(config, WEB)).toContain(
      'area:web > any-glob-to-any-file > "apps/wbe/**"',
    );
    expect(
      deadComponents(
        { 'area:web': [{ 'changed-files': [{ 'any-glob-to-any-file': ['apps/web/**'] }] }] },
        WEB,
      ),
    ).toEqual([]);
  });
});

/*
 * A sampled path is a path this repository has.
 *
 * Nothing checked that, so a glob matching no real file passed whenever its sample copied
 * the same wrong path — and two already did: `apps/web/src/generated/SessionId.ts` and
 * `apps/web/src/transport/channel.ts` were both fictional, and the suite was green on them
 * for as long as they had been there. Checking against the tree is what makes the liveness
 * invariant above mean something: a misspelled glob can only be proven live by a sample that
 * matches it, and such a sample now has to name a file that exists.
 */
describe('the sample table describes real pull requests', () => {
  /**
   * A Rust module is either `foo.rs` or `foo/mod.rs`, and the config matches both spellings
   * on purpose so a module can change shape without the label dying. Sampling only the
   * spelling on disk today would leave the other glob permanently dead, so the counterpart
   * of an existing module counts as real. Computed, not a hand-kept exception list: there is
   * nothing to forget to remove.
   */
  const counterpart = (path: string): string | undefined => {
    if (path.endsWith('/mod.rs')) return `${path.slice(0, -'/mod.rs'.length)}.rs`;
    if (path.endsWith('.rs')) return `${path.slice(0, -'.rs'.length)}/mod.rs`;
    return undefined;
  };

  const sampledFiles = [...new Set(SAMPLES.flatMap(([, files]) => files))];

  it.each(sampledFiles)('%s is a file, or the other spelling of one', (file) => {
    const other = counterpart(file);
    const real = existsSync(join(repoRoot, file));
    expect([file, real || (other !== undefined && existsSync(join(repoRoot, other)))]).toEqual([
      file,
      true,
    ]);
  });

  it('samples enough paths for that to mean something', () => {
    expect(sampledFiles.length).toBeGreaterThan(20);
  });
});
