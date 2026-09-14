# Nysia

A terminal-first agentic development environment. Tauri v2 + Rust.

Every tab is a session; a session is either an agent or a shell. Projects and their
worktrees are the spine. Tasks come from GitHub Issues.

**Headless core, detachable UI** — a long-lived `nysiad` daemon owns the PTYs, the store,
the orchestration state and the agent-status endpoint. The window is a client. Updating or
crashing the UI never interrupts a running agent.

**Status: v0.1, the walking skeleton.** The daemon, its socket protocol, PTY shell sessions,
the Rust VT state, the multiplexed output channel and the app chrome are built and running.
The acceptance criterion holds: close the window and the shell survives; relaunch and the tab
comes back with its scrollback replayed and the session still taking input. What v0.1
deliberately does **not** have is agent sessions — Claude, hooks and status detection are
v0.2. See [CHANGELOG.md](CHANGELOG.md) for what shipped, and §12 of the architecture for the
two defects this milestone found and left open.

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
```

`nysia` is one binary that is both the daemon and the CLI, selected by argv (D-11). There is
no `nysiad` on disk: `nysia --daemon` *is* `nysiad`.

```
cargo run -p nysia -- --help             # the CLI surface
cargo run -p nysia -- --daemon           # become the daemon
nysia session create --profile pwsh      # start a shell, print its handle
nysia session list --json
nysia terminal send <handle> --text 'git status' --enter
nysia terminal read <handle> --screen    # the rendered grid, which is what an agent reads
nysia terminal read <handle> --stream --cursor 0   # the raw scrollback from the start
nysia session close <handle>
```

Every verb takes `--json`, and the contract is the same everywhere: **the result on stdout,
the error envelope on stderr, and the exit code says which happened.** Any verb starts a
daemon if none is listening; `--no-spawn` makes that an error instead, which is what a
supervisor wants.

#### Running the app against a daemon

**The window does not start a daemon yet** — see §12 question 6 of the architecture. Start one
first, or the `+` menu will tell you to:

```
cargo build --release -p nysia          # target/release/nysia[.exe]

# Windows (PowerShell)
Start-Process target\release\nysia.exe -ArgumentList '--daemon' -WindowStyle Hidden
# macOS
target/release/nysia --daemon &
```

Then launch the app. `pnpm dev` and `pnpm build:app` both produce a window that finds that
daemon through `NYSIA_RUNTIME_DIR` (or the platform default), so the CLI above and the window
are looking at the same sessions — which is the quickest way to see D-1 working.

> **Building the window with plain cargo?** Pass `--features custom-protocol`:
>
> ```
> cd apps/desktop/src-tauri && cargo build --release --features custom-protocol
> ```
>
> Without it the webview loads `devUrl` rather than the bundled frontend, so a release build
> with no vite server behind it shows a WebView2 connection error instead of Nysia. `tauri
> build` (via `pnpm build:app`) sets the feature for you.

### Proving the walking skeleton

The v0.1 acceptance criterion is one sentence — *kill the UI and the shell survives; reattach
and the scrollback replays* — and it is proved at three levels, each covering what the one
below it cannot:

| | What it drives | Where it runs |
|---|---|---|
| `crates/nysia/tests/survival.rs` | the daemon, CLI-only, no GUI anywhere near it | `cargo test`, both CI legs |
| `interop::a_relaunched_window_finds_the_session_and_replays_its_scrollback` | the window's own client against a real daemon over a real socket | `cargo test`, both CI legs |
| `scripts/e2e/walking-skeleton.ps1` | the real app: a real process exit, a genuinely new client id, a webview that paints | by hand, Windows |

The script is stepped rather than one run, because opening a tab and closing a window are
things a person does and a script that pretended otherwise would be synthesising input and
calling it a GUI test. It runs on a daemon of its own, so it cannot disturb yours.

```
powershell -File scripts/e2e/walking-skeleton.ps1 -Step start -Shell pwsh
#   ... open a terminal tab and run the lines it prints ...
powershell -File scripts/e2e/walking-skeleton.ps1 -Step typed
#   ... close the window with the X in its title bar ...
powershell -File scripts/e2e/walking-skeleton.ps1 -Step closed
powershell -File scripts/e2e/walking-skeleton.ps1 -Step relaunched
powershell -File scripts/e2e/walking-skeleton.ps1 -Step upgraded -NewAppBinary <a different build>
powershell -File scripts/e2e/walking-skeleton.ps1 -Step finish
```

`scripts/e2e/measure-memory.ps1` is the other half: what a daemon costs with 1, 5 and 10
sessions, children counted. Its numbers are recorded in §12 question 3 rather than left to be
re-derived.

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
scripts/e2e/           the end-to-end proofs a test runner cannot run for you
```
