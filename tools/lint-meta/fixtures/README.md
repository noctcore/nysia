# lint-meta fixtures

Two miniature repositories. `trips/` breaks both architecture rules and `clean/` satisfies
them while exercising every carve-out the rules allow.

`scripts/prove-lint-meta.ts` runs the rules against both and fails unless `trips/` reports
exactly the expected violations and `clean/` reports none. That is the proof that the rules
still trip (traps register #13) — without it, `pnpm lint` passing would mean nothing.

The repo-wide scan skips this directory, which is why the proof points the runner at it
explicitly.

## The trees

| Tree | What it proves |
|---|---|
| `trips/` | Each rule broken directly: a Tauri import from the webview in TypeScript and in JavaScript, the three Rust `use` spellings, a crate declaring tauri, and the same declared under a rename. |
| `trips-transitive/` | The indirect branch — `nysia-core` is clean and the violation arrives through `nysia-proto`. Without a committed fixture that branch is code nobody has watched fail. |
| `clean/` | Every carve-out the rules allow: `apps/desktop` linking tauri in both its manifest and its source, `apps/web/src/transport` importing it, and a `nysia-core` whose doc comments, nested block comment and `crate::tauri_helpers` import must all stay silent. |

Line numbers in `trips/crates/nysia/src/leak.rs` are asserted by `scripts/prove-lint-meta.ts`,
so reformatting that file will fail the proof. That is deliberate: asserting a boolean would
pass even if the rule reported the wrong place.
