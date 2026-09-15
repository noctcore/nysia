# lint-meta fixtures

Miniature trees built to break the architecture rules. `scripts/prove-lint-meta.ts` points
the rules at them and fails unless each one reports exactly what it should — that is the
proof the rules still trip (traps register #13). The repo-wide scan skips this directory,
which is why the proof points at it explicitly.

## The trees

| Tree | What it proves |
|---|---|
| `trips/` | Rule (a). A Tauri import from the webview in TypeScript and in JavaScript, and the Rust `use` spellings a line-anchored regex walked past. Rule (f): a crate outside `agent/**` naming `agent::claude` in six spellings, plus the ways the boundary dissolves from inside it — a `pub use` that launders a Claude item into the neutral namespace, the same laundering through an alias that never writes the word, a `pub type` and a `pub fn` signature that do it without a `use` at all, and a `pub(crate) mod claude` that disarms the compiler's half. The same file also holds the shapes the rule must stay silent about, each pinned by line in the proof. |
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

## The two halves of rule (f) are not equally defended

Outside `agent/**`, Rust privacy is the load-bearing half: `mod claude` carries no visibility
modifier, so an import the rule misses does not compile either. Inside `agent/**` the compiler
permits everything — it is the module that is *allowed* to name `claude` — so the rule is the
only guard, and a hole there is worth much more than a hole outside.

That is what `agent/mod.rs` lines 34-47 are for. `use claude as c;` binds an alias, and the
statements that hand Claude types out through it never write the word `claude` at all:
`pub use c::Probe as NeutralProbe;`, `pub use c::*;`, and a second hop through
`use self::claude::hooks as h; pub use h::EVENTS;`. A `pub type` alias and a `pub fn` return
type do the same thing without a `use`. Every one of those reported nothing until paths were
resolved through the file's own bindings instead of being matched on spelling.

Lines 52-55 are the other side of it, asserted in the tripping tree rather than the clean one
because they sit in a file that reports: a `claude` module under a different parent,
two visibilities that cannot leave `agent/` (`pub(self)`, `pub(in crate::agent)`), a plain
`use` that binds a Claude name without handing it anywhere, and — further down — a `pub` item
inside a private module and a `pub` field on a private struct, neither of which hands anything
to anybody. lint-meta has no suppression mechanism, so each of those would be a report a file
could not get out of, which is why the proof pins them by line rather than trusting the clean
tree to cover them.
