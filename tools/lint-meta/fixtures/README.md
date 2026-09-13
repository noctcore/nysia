# lint-meta fixtures

Miniature trees built to break the architecture rules. `scripts/prove-lint-meta.ts` points
the rules at them and fails unless each one reports exactly what it should — that is the
proof the rules still trip (traps register #13). The repo-wide scan skips this directory,
which is why the proof points at it explicitly.

## The trees

| Tree | What it proves |
|---|---|
| `trips/` | Rule (a). A Tauri import from the webview in TypeScript and in JavaScript, and the Rust `use` spellings a line-anchored regex walked past. |
| `clean/` | Rule (a)'s carve-outs: `apps/desktop` and `apps/web/src/transport` may import Tauri, and a `nysia-core` whose doc comments, nested block comment and `crate::tauri_helpers` import must all stay silent. |
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
