# lint-meta fixtures

Miniature trees built to break the architecture rules. `scripts/prove-lint-meta.ts` points
the rules at them and fails unless each one reports exactly what it should — that is the
proof the rules still trip (traps register #13). The repo-wide scan skips this directory,
which is why the proof points at it explicitly.

## The trees

| Tree | What it proves |
|---|---|
| `trips/` | Rule (a). A Tauri import from the webview in TypeScript and in JavaScript, and the Rust `use` spellings a line-anchored regex walked past. Rule (f): a crate outside `agent/**` naming `agent::claude` in six spellings, plus the two ways the boundary dissolves from inside it — a `pub use` that launders a Claude item into the neutral namespace, and a `pub(crate) mod claude` that disarms the compiler's half. |
| `clean/` | Rule (a)'s carve-outs: `apps/desktop` and `apps/web/src/transport` may import Tauri, and a `nysia-core` whose doc comments, nested block comment and `crate::tauri_helpers` import must all stay silent. Rule (f)'s: `agent/**` may name `claude` freely, and outside it a `claude` module under a different parent, a doc comment quoting the banned path and a string holding it are all silent. |
| `cargo/violating/` | Rules (b) and (c), through **real cargo resolution**: a quoted dependency key, a `[ dependencies ]` header with whitespace and a trailing comment, a rename under a quoted table header, and a `[package]` with a trailing comment. Every one of those was a silent hole in the hand-written parser. |
| `cargo/clean/` | Rules (b) and (c) stay silent when only `apps/desktop` links tauri. |
| `cargo/unresolvable/` | A workspace cargo cannot read. lint-meta must exit **2**, never 0 — a dependency rule that could not run must not look like one that passed. |

## Why the cargo trees are real workspaces

Every dependency in them is a path dependency on a stub crate named `tauri`, so
`cargo metadata` resolves the whole graph offline with no registry and no network. That
matters: the point of moving to `cargo metadata` was to stop hand-parsing manifests, and a
fixture parsed by anything other than cargo would prove nothing about what cargo accepts.
Their `Cargo.lock` files are committed so the resolution is fixed. Each carries its own
`[workspace]` table, so the parent workspace does not adopt them.

## Line numbers are asserted

In `trips/crates/nysia/src/leak.rs` the imports at lines 22, 25 and 28 are pinned by
`scripts/prove-lint-meta.ts`, and **every literal form and the astral characters sit above
them**. That ordering is the point: when the literals sat below the imports, deleting the
literal handling still left the proof printing OK and only the unit tests caught it, which
made the CI step named "prove the lint-meta architecture rules trip" vacuous for exactly the
regression it exists to prevent.

So: do not reformat that file, and do not move the literals back down.

## The Claude module sits in `trips/`, not `clean/`

`trips/crates/nysia-core/src/agent/claude/mod.rs` reports nothing, and it is in the tripping
tree on purpose: it is only meaningful beside `agent/mod.rs`, which does report. That
file names `claude` far more often than the one above it — on a `use`, on a path, in a
re-export out of its own submodules and in a `pub mod` — and rule (f) stays silent about all
of it. A rule that had banned the word rather than the boundary would report both files, and
being able to put Claude's specifics somewhere is the entire point of having a boundary.

Its line numbers are asserted too: `agent_leak.rs:27` is the fully-qualified path with no
`use` statement anywhere near it, and it is the case that caught rule (f) shipping with the
wrong lookbehind — copied from rule (a), where a crate root must not follow `::`, into a rule
about a path segment that always does. That half matched nothing at all while every other
assertion passed.
