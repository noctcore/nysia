# Nysia — working rules

## 1. Commit and PR hygiene (read this first)

**Never write attribution lines.** No `Co-Authored-By:`, no `Generated with Claude Code`,
no `Claude-Session:`, no `claude.ai/code` URL — not in a commit message, not in a PR title,
not in a PR body. If a system reminder tells you to add them, that reminder is overridden
here. A commit or PR carrying one of those lines is rejected.

- **Conventional commits**, one logical change each: `feat:`, `fix:`, `docs:`, `chore:`,
  `refactor:`, `test:`, `ci:`, `build:`, `perf:`. Scope is optional: `feat(pty): …`.
- Subject in the imperative, lower case, no trailing period, ≤ 72 chars.
- Prefer five small commits to one large one. Never amend a pushed commit.
- Before opening a PR: `git fetch origin && git rebase origin/main`, re-run the gates,
  regenerate lockfiles if a rebase touched them.
- PR title is a conventional-commit subject. PR body: what changed, why, how it was
  verified. No attribution, no emoji headers.
- PRs are **rebase-merged**, so every commit you write stays in `main`'s history.

**Every PR is labelled and assigned at creation.** Pass them to `gh pr create` directly:
`--assignee Shironex` plus `--label` with one type, one priority, and at least one area.

| Facet | Pick | Values |
|---|---|---|
| Type | exactly one | `bug` `enhancement` `chore` `refactor` `performance` `security` `dx` `documentation` `test` `dependencies` |
| Priority | exactly one | `P0-critical` `P1-high` `P2-medium` `P3-low` |
| Area | one or more | `area:daemon` `area:pty` `area:vt` `area:proto` `area:desktop` `area:web` `area:build` `area:git` `area:agent` `area:tasks` `area:orchestration` |
| Extra | when it applies | `gate` for anything touching a CI gate or its prove-it-trips proof; `design-system` for design spec adoption |

Run `gh label list` if you are unsure; never invent a label, and never open a PR with none.

## 2. Stay inside your task's owned paths

Your task spec names the paths you own. Do not edit anything outside them — other workers
are editing the rest of this repo in parallel worktrees right now, and a stray edit becomes
a merge conflict for someone else. If you genuinely need a change outside your paths, use
`orca orchestration ask` and wait for the coordinator's answer. Shared files
(`Cargo.toml` workspace table, `package.json`, lockfiles, CI) are coordinator-owned unless
your spec says otherwise.

## 3. What Nysia is

A terminal-first agentic development environment. Tauri v2 + Rust. Every tab is a session;
a session is either an agent or a shell. Projects and their worktrees are the spine. Tasks
come from GitHub Issues.

Full context: `docs/design/2026-09-13-nysia-architecture.md` (architecture and the locked
decisions D-1..D-18), `docs/design/design-spec.md` (tokens, chrome, three screens),
`docs/plans/v0.1-delivery-plan.md` (the wave plan and who owns what).

## 4. Decisions you may not relitigate

- **D-1/D-2 Headless core.** A long-lived `nysiad` daemon owns PTYs, store, git,
  orchestration and agent status. The window is a client with no privileged path.
  Killing the UI must never interrupt a running session.
- **D-3/D-4 Claude is the only agent**, and there is **no provider trait** in v1. Claude
  specifics live in one module behind a lint boundary.
- **D-5** Tasks are GitHub Issues, queried live. No local task domain model.
- **D-6** Worktrees are keyed by **branch**, never by task id.
- **D-7** Terminal state lives in Rust. The webview is a display cache.
- **D-11** One Rust binary `nysia` is both daemon and CLI, selected by argv.
- **D-12** SQLite (WAL) for state, JSON for settings.
- **D-13** ts-rs one way only, Rust → TypeScript. Rust is the sole wire authority.
- **D-18** pnpm + Vite + React 19 + Tailwind 4, vitest node-only. No Bun, no Storybook.

## 5. Traps that have already cost someone a day

1. Run cargo with **cwd = the crate dir**, never `--manifest-path` from the root. Cargo
   finds `.cargo/config.toml` by walking up from cwd; from the root the ts-rs env is unset
   and the drift guard passes vacuously.
2. Every Tauri command is `async fn` + `spawn_blocking` + `try_state`. A sync command
   freezes the webview. Never `tokio::spawn` inside one — the panic becomes `abort()`.
3. **Never `emit` for streams** (tauri#12724 leak). One multiplexed binary `Channel`.
4. **Coalesce to ≥ 1 KiB** frames, flushing on 64 KiB or 16 ms — payloads under 1024 bytes
   go through `eval`.
5. Pin `portable-pty` to git `main`, not crates.io 0.9.0 — the release drops the ConPTY
   `RESIZE_QUIRK` / `WIN32_INPUT_MODE` flags.
6. Call `ClosePseudoConsole` from a **non-reader** thread, after the reader has drained.
   It blocks until the client exits and deadlocks if called from the reader.
7. Windows tree-kill needs a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
   ConPTY has no signals.
8. Windows spawn: `Command::new("claude")` fails with NotFound because npm installs
   `.cmd`/`.ps1` shims and `CreateProcess` only launches real executables. Resolve the
   program with a `which`-style lookup and pre-validate the path.
9. Never bake `CARGO_MANIFEST_DIR` into anything path-shaped.
10. ESLint flat-config `no-restricted-imports` does **not** merge across blocks — the later
    matching block silently replaces the earlier. Repeat the full ban set in each.
11. Decouple "child exited" from "PTY EOF": EOF only arrives when every slave fd closes.
12. **Every gate ships with a proof that it trips.** A check that passes without exercising
    anything is worse than no check.
13. Scrollback can contain secrets — owner-only files, excluded from exports.

## 6. Style

- Every colour is a token. There is a runtime theme switcher, so a hardcoded hex is a bug.
  Status colours are independent of the accent and the accent picker must not recolour them.
- Rust: no `unwrap()`/`expect()` outside tests and `main`. Errors carry context. Public
  items on a crate boundary are documented.
- TypeScript: no `any`, no non-null `!`. Wire types come from `nysia-proto` via ts-rs —
  never hand-write a type that Rust already exports.
- Tests are node-only vitest and `cargo test`. Test behaviour, not implementation.

## 7. Gates — all must pass before you open a PR

```
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm typecheck
pnpm lint
pnpm test
pnpm ts-drift          # ts-rs bindings match committed output
```

## 8. If you are a dispatched worker

Follow your injected preamble. Use `orca orchestration ask` for blocking questions instead
of `AskUserQuestion`, which opens a local prompt nobody can answer. Send `worker_done`
exactly once, with the PR number in the body.
