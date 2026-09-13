# Nysia

A terminal-first agentic development environment. Tauri v2 + Rust.

Every tab is a session; a session is either an agent or a shell. Projects and their
worktrees are the spine. Tasks come from GitHub Issues.

**Headless core, detachable UI** — a long-lived `nysiad` daemon owns the PTYs, the store,
the orchestration state and the agent-status endpoint. The window is a client. Updating or
crashing the UI never interrupts a running agent.

Status: wave 0 scaffold. The workspaces, the gates and the wire types are real; the
daemon, the PTYs and the chrome are not built yet.

- [Architecture and founding decisions](docs/design/2026-09-13-nysia-architecture.md)
- [Design system spec](docs/design/design-spec.md)
- [Design sources and provenance](docs/design/README.md)
- [v0.1 delivery plan](docs/plans/v0.1-delivery-plan.md)
- [Working rules](CLAUDE.md)

## Development

### Prerequisites

- **Rust** — the toolchain is pinned in `rust-toolchain.toml`; rustup installs it, with
  `rustfmt` and `clippy`, the first time you run cargo.
- **Node 22.18+** and **pnpm 10** (`corepack enable`). The version is pinned by
  `packageManager` in `package.json`.
- **Windows:** the MSVC build tools and WebView2 (present on Windows 11).
  **macOS:** Xcode command line tools.
- `rusqlite` is built with the bundled SQLite amalgamation, so no system library is needed.

```
pnpm install
```

### Running it

```
pnpm dev          # the app: tauri dev, which starts the vite server for you
pnpm dev:web      # just the webview, at http://localhost:5173
pnpm build        # the web frontend into apps/web/dist
pnpm build:app    # the bundled desktop app

cargo run -p nysia -- --help      # the CLI surface
cargo run -p nysia -- --daemon    # what will become nysiad
```

`nysia` is one binary that is both the daemon and the CLI, selected by argv (D-11). In this
scaffold nothing is implemented yet, so every mode — `--daemon` included — writes to stderr
and exits **3**. That is deliberate: a stub daemon that exited 0 would read as success to
any supervisor that spawned it and checked the status. Clap's usage errors stay at 2, so
"you typed it wrong" and "this build cannot do that yet" remain distinguishable. The socket
server lands in wave 2.

### Gates

All of these must pass before a PR is opened. CI runs exactly this list on `windows-latest`
and `macos-latest`.

```
pnpm build          # first: see the note below
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm typecheck
pnpm lint
pnpm test
pnpm ts-drift
```

> **`pnpm build` comes first.** `tauri::generate_context!` reads `frontendDist` at compile
> time, so `apps/web/dist` has to exist before any cargo command that touches
> `apps/desktop/src-tauri`.

Every gate ships with a proof that it trips — a check that passes without exercising
anything is worse than no check:

```
pnpm prove:ts-drift      # mutates nysia-proto, asserts the drift guard exits 1, restores it
pnpm prove:lint-meta     # runs every architecture rule against fixtures that break it
pnpm prove:eslint-bans   # lints virtual files to prove no-restricted-imports still bites
pnpm prove               # all three
```

### Regenerating the TypeScript bindings

Wire types are generated one way, Rust → TypeScript (D-13), and the output is committed
under `apps/web/src/generated/`.

```
cd crates/nysia-proto && cargo test export_bindings
```

**Run it from the crate directory.** Cargo finds `.cargo/config.toml` by walking up from
the working directory; from the repo root the ts-rs environment is unset, the bindings land
in a gitignored `crates/nysia-proto/bindings/`, and `pnpm ts-drift` passes without having
compared anything.

### Layout

```
crates/nysia/          the daemon and the CLI, one binary (D-11)
crates/nysia-core/     PTY, VT state, store, git, worktree, rpc — never depends on tauri
crates/nysia-proto/    wire types; the ts-rs export lives here
apps/desktop/          the Tauri v2 window, a client with no privileged path (D-2)
apps/web/              React 19 + Vite + Tailwind 4
tools/lint-meta/       architecture rules (D-14)
```

Status: wave 0. The daemon, the PTYs and the app chrome are scaffolding — see
[the delivery plan](docs/plans/v0.1-delivery-plan.md) for who builds what next.
