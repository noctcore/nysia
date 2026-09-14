# Changelog

Notable changes per release. Newest first.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), with one
addition that matters more here than the categories do: every entry says what was
**deliberately not** built, because a scope ladder only means something if each rung is
honest about where it stops.

## [0.1.0] — 2026-09-14

**The walking skeleton.** One acceptance criterion, and it holds: *close the window and the
shell survives; relaunch and the tab comes back with its scrollback replayed and the session
still taking input.* Verified against the real app on Windows 11, and in CI at two levels
below that on both `windows-latest` and `macos-latest`.

### Added

- **`nysiad`, the headless runtime** (D-1/D-2). A long-lived daemon owning every PTY, behind a
  versioned named pipe on Windows and a Unix socket on macOS: `hello` handshake with a
  `daemonIdentity`, NDJSON control framing beside length-prefixed binary output, peer-credential
  auth, a pid-record lease, spawn-if-absent settled by an OS-held lock, and idle retire that a
  daemon holding a session never takes.
- **One binary, two modes** (D-11). `nysia --daemon` *is* `nysiad`; every other argv is a client
  of that same socket — the same verbs the window uses, which is what makes "the window has no
  privileged path" checkable rather than asserted. Verbs: `session create|list|close`,
  `terminal read|send|resize|wait`. Every one takes `--json`, with the result on stdout, the
  error envelope on stderr, and the exit code saying which.
- **PTY shell sessions** for `pwsh`, `cmd`, Git Bash and WSL: one blocking reader thread each,
  the ConPTY `RESIZE_QUIRK` / `WIN32_INPUT_MODE` flags, `ClosePseudoConsole` from a non-reader
  thread after the reader has drained, and Windows tree-kill through a Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
- **Terminal state in Rust** (D-7). An `alacritty_terminal` grid as a staging area, a bounded
  raw replay ring (256 KiB) and a logical line log (10 000 lines / 4 MiB) — so `terminal read
  --screen` is the rendered grid an agent can read, `--stream` is the raw scrollback, and a
  reattaching client replays bytes rather than a re-render.
- **The window as a client.** Tauri v2 with one multiplexed binary `Channel` and never `emit`
  (tauri#12724), output coalesced to ≥ 1 KiB and flushed on 64 KiB or 16 ms, a credit-window
  backpressure exchange, and an xterm.js surface over a WebGL renderer pool.
- **The chrome:** Ember and Graphite themes with a live accent and derived `--acc14`/`--acc35`,
  the projects sidebar, the tab strip and its `+` launcher menu, the status bar, and Settings →
  General and Appearance.
- **`nysia-proto` as the sole wire authority** (D-13), with the ts-rs export generated one way
  into `apps/web/src/generated/` and a drift guard that ships with a proof it trips.
- **The end-to-end proofs.** `crates/nysia/tests/survival.rs` (the daemon, CLI-only),
  `interop::a_relaunched_window_finds_the_session_and_replays_its_scrollback` (the window's own
  client against a real daemon over a real socket), and `scripts/e2e/walking-skeleton.ps1` plus
  `scripts/e2e/measure-memory.ps1` for what a test runner cannot do for you.

### Measured

Recorded in §12 of the architecture rather than left to be re-derived, on the same machine as
the Orca figures it is compared against.

- Daemon: **11.5 MB** working set with one session, **12.9 MB** with five, **14.7 MB** with ten
  — against Orca's 88 MB PTY daemon for five terminals. About 2.2 MB per session.
- App: **409.6 MB** working set / 205.7 MB private with one shell tab, six WebView2 processes
  included — against Orca's 1.1 GB. Better, and not by the order of magnitude "Tauri instead of
  Electron" is often taken to mean.
- Scrollback survives neither a daemon crash nor a clean restart. Nothing is persisted in v0.1.

### Deliberately not in this release

Not missing — not built, and each has a rung on the ladder:

- **Agent sessions.** Claude sessions, hook install/uninstall, status detection and the live
  status dots are v0.2. The `+` menu lists Claude because the launcher table is shared; choosing
  it does not start an agent.
- **GitHub Issues** and the `Start →` flow that creates a branch-keyed worktree (v0.3); the
  worktree manager and the usage meter (v0.4); the orchestration verb surface and
  `nysia agent-context` (v0.5); a second provider, and with it the extraction of a provider
  trait, plus the browser tab (v0.6+).
- **Settings → Tasks and Settings → Agents**, which the design spec draws. Only General and
  Appearance are built.
- **Persistence.** `nysia_core::store` is a module doc comment: SQLite holds nothing yet, so
  sessions and scrollback live and die with the daemon (§12 q2 and q4).
- **OS-level confinement.** Lexical path gates only (§7.5, §12 q1).

### Known defects

Found by driving the real GUI, reported rather than papered over, and both open with their
evidence in §12:

- **The window cannot start a daemon, and no build of the app ships one** (§12 q6). On a machine
  with nothing listening, the app launches into *Reconnecting* and the `+` menu fails with
  "Start the Nysia daemon, then try again". Start one first; the README says how.
- **Re-attaching injects input into the shell** (§12 q7). The replay contains the terminal
  queries the child once wrote, xterm answers them, and ConPTY reads a cursor-position report as
  F3 — so after a relaunch the pane shows a phantom command line and the next thing you type is
  concatenated onto it. `Esc` clears it.

[0.1.0]: https://github.com/noctcore/nysia/releases/tag/v0.1.0
