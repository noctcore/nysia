# Contributing

Thanks for looking. Nysia is early (v0.3) and moves fast, so please open an issue before writing
anything larger than a small fix, and we can agree on the shape first. Contributions are accepted
under the [MIT License](LICENSE).

## Setup and the gates

The [README](README.md#development) covers the prerequisites (the pinned Rust toolchain, Node
22.18 or newer and pnpm 10), running the app and the daemon, and the full list of gates. Every one
of them must pass before a pull request opens, and CI runs the same list on Windows and macOS:

```sh
pnpm build          # first: tauri::generate_context! reads apps/web/dist at compile time
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm typecheck
pnpm lint
pnpm test
pnpm ts-drift
```

A green run on one platform proves nothing about the other, so check CI after you push.

## Commits and pull requests

[CLAUDE.md](CLAUDE.md) section 1 is the authority; in short:

- Conventional commits, one logical change each (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`,
  `test:`, `ci:`, `build:`, `perf:`), scope optional, subject lower case and imperative.
- No attribution lines in commits, titles or bodies.
- Every pull request carries exactly one type label, exactly one priority label and at least one
  area label from `gh label list`. `pr-facets` fails a pull request without them.
- Pull requests are rebase-merged, so every commit stays in `main`'s history.

The [architecture document](docs/design/2026-09-13-nysia-architecture.md) records the founding
decisions (D-1 to D-18); a change that reverses one starts as an issue.

## Reporting

- A bug or a feature idea: use the [issue forms](https://github.com/noctcore/nysia/issues/new/choose).
- A vulnerability: never in a public issue; follow [SECURITY.md](SECURITY.md).

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
