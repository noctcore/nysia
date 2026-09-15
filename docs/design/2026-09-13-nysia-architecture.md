# Nysia — architecture and founding decisions

**Date:** 2026-09-13
**Status:** design, pre-scaffold
**Sources:** four parallel research passes (Orca 1.4.197 teardown · Tauri/PTY stack · nightcore + omniscribe + ERP recon · competitive landscape) plus the Claude Design project `Nysia ADE.dc.html`.

---

## 1. What Nysia is

A **terminal-first agentic development environment**: a desktop cockpit where every tab is a session, every session is either an agent or a shell, projects and their worktrees are the spine, and tasks come straight from GitHub Issues.

Built as **Tauri v2 + Rust**, developed primarily on **Windows**, shipping on Windows and macOS.

The product shape is taken from Orca, which the author uses daily. The engineering is not a clone: §4 describes the one structural thing Nysia does differently, and it is the reason the project is worth building rather than a reskin.

**Name:** `nysia.app` is available (RDAP-confirmed 2026-09-13), as are `.dev .ai .io .sh .co .tools .run .studio`. Only `nysia.com` is registered.

---

## 2. Decisions locked

| # | Decision | Rationale |
|---|---|---|
| D-1 | **Headless core, detachable UI.** A long-lived `nysiad` daemon owns everything stateful. The window is a client. | Updating or crashing the UI must never interrupt a running agent. This is the founding constraint. |
| D-2 | **The daemon owns the runtime too**, not just PTYs — store, orchestration, hook endpoint, git. | §4. This is the departure from Orca and the source of most of Nysia's structural advantage. |
| D-3 | **Claude is the only agent in v1.** Shells (pwsh, cmd, Git Bash, WSL) are the other session type. | Codex/Gemini/OpenCode are later releases. |
| D-4 | **No provider abstraction until there are two providers.** Claude code lives behind a lint-enforced module boundary, not behind a trait. | §7.4. |
| D-5 | **Tasks are GitHub Issues**, queried live. No local task domain model. | Deletes the entire spine that made nightcore's Rust core task-shaped (`task` appeared in 61% of its Rust files). |
| D-6 | **Worktrees are keyed by branch**, never by task id. | nightcore documented three shipped bugs caused by task-keying. Free to get right now. |
| D-7 | **Terminal state lives in Rust**; the webview is a display cache. | Makes 30 sessions cheap, makes agent reads a Rust API, makes the UI genuinely disposable. |
| D-8 | **Usage: statusline shim by default, OAuth path behind an off-by-default flag.** | §7.6. |
| D-9 | **Browser tab deferred past v1.** | WKWebView has no CDP; revisit when `tauri-runtime-cef` matures. |
| D-10 | **No Voice.** | Design mock only. |
| D-11 | **Two artifacts**: one Rust binary `nysia` that is both daemon and CLI (mode by argv), plus the Tauri GUI. The CLI ships in the app bundle and symlinks onto PATH. | Orca's shape. Lets the daemon and GUI run different versions, which is what surviving an upgrade requires. |
| D-12 | **SQLite (WAL) for state, JSON for settings.** | Orchestration needs indexes and transactions; settings need to be hand-editable. |
| D-13 | **ts-rs one-way, Rust → TS.** No zod→Rust generator. | With no sidecar, TypeScript never produces wire messages. Rust is the sole authority. Drops ~2,400 lines of generator. |
| D-14 | **Minimal gates + architecture rules from day one.** | Ratchets and layer rules are cheap now and a migration later. Coverage floors and the full 22-rule set wait. |
| D-15 | **The daemon owns all git**, and pushes status/diff to clients rather than answering polls. | One isolation chokepoint; the UI never blocks on git. |
| D-16 | **Hooks reach the daemon via `nysia hook`** writing to the socket. No HTTP server, no port, no token file. | §5.3. Simpler than Orca's HTTP listener, and identical on both platforms. |
| D-17 | **WSL is a plain shell in v1** — `wsl.exe -d <distro>` in a ConPTY. No path translation, no worktrees, no agents inside WSL. | Covers the design's menu entry honestly at near-zero cost. |
| D-18 | **pnpm + Vite + React 19 + Tailwind 4, vitest node-only.** No Bun, no Storybook, no browser-mode tests in v1. | pnpm is safer on Windows-primary development; browser-mode is the slowest CI job for a UI that is mostly a terminal. |

---

## 3. Process model

```
nysiad  ─ long-lived, detached, survives every UI restart and upgrade
│
├── PTY layer          portable-pty; one blocking reader thread per session
├── VT layer           alacritty_terminal grid + raw replay ring, per session
├── Store              SQLite (WAL): sessions, worktrees, orchestration,
│                      agent status, scrollback index
│                      JSON: settings, projects (hand-editable)
├── Git / worktree     all git spawns through one hardened chokepoint;
│                      repo watcher pushes status/diff to clients
└── RPC server         versioned NDJSON over named pipe (Windows) /
                       Unix socket (macOS), peer-credential auth
        ▲                    ▲                ▲              ▲
        │                    │                │              │
   Nysia.app            nysia <verb>     nysia hook    (future) relay
   (Tauri GUI)      (what agents call)  (status in)  (mobile approvals)
```

Every consumer is a client of the same socket and the same verb surface. The window has no privileged path.

### 3.0 Binaries

```
Nysia.app/
  Contents/MacOS/Nysia          <- Tauri GUI
  Contents/Resources/bin/nysia  <- daemon + CLI (one Rust binary)
                    |
  /usr/local/bin/nysia ---------+   (symlink; %LOCALAPPDATA%\Nysia\bin on Windows)

nysia --daemon        -> becomes nysiad
nysia terminal read   -> CLI client
nysia hook            -> status ingest (stdin -> socket)
```

Two build artifacts, two signing targets. The GUI and the daemon are deliberately allowed to run different versions — that is what an in-place upgrade requires, and the versioned socket handshake is what makes it safe.

### 3.1 Socket protocol

Lifted wholesale from Orca's daemon, which has earned it:

- **Versioned socket name** — `nysiad-v1.sock` / `\\.\pipe\nysiad-v1-<user>`. A new client refuses to attach to an incompatible daemon instead of corrupting it. Orca is on `daemon-v36` and still advertises `attachableDaemonProtocolVersions: [1..36]`, so a newer app adopts an older daemon rather than orphaning its sessions. Copy that.
- **Token file** beside the socket, mode 0600 / owner-only ACL.
- **Handshake first frame**: `{type:"hello", version, token, role:"control"|"stream", clientId}` → `{ok, daemonIdentity:{pid, startedAtMs, launchNonce, appVersion}}`. Reject with `retryable` set so clients know whether to back off or die.
- **Framing**: newline-delimited JSON for control; length-prefixed binary (`[kind:u8][streamId:u32 BE][len:u32 BE][payload]`) for terminal output.
- **Adoption lease**: pid + `startedAtMs` + `launchNonce` in a pid-record file, so a restarted app can verify the daemon it found is the one it thinks it is.
- **Idle retire**: `shutdownIfIdle` only when the caller is the sole client, no sessions exist, nothing in flight. Otherwise the daemon outlives the app — that is the point.

### 3.2 Caller authentication — better than Orca's

Orca proves a calling agent's identity by **hook echo**: the CLI presents a launch token, and the hook listener must independently attest that the same token has been POSTing from that pane. It is indirect, and it breaks on legitimate cases — the `legacy_read_only` refusal from a Claude child session is exactly this failing.

Nysia can prove it **directly**, because the daemon spawned the process:

1. **Socket peer credentials** — `SO_PEERCRED`/`getpeereid` on Unix, `GetNamedPipeClientProcessId` on Windows. The daemon learns the calling pid from the kernel; it cannot be forged.
2. **PTY process-tree ancestry** — the daemon spawned every PTY and knows each session leader. Walk the caller's pid up to a session it owns.
3. The pane identity follows from (2). No token echo, no attestation round-trip, no `restored authority` bookkeeping.

Env vars (`NYSIA_PANE_KEY` etc.) stay as a **hint** for speed, never as the proof.

### 3.3 Identity

Three ids, learned from Orca's model:

| Id | Shape | Lifetime |
|---|---|---|
| **Pane key** | `<tabId>:<leafId>` | Durable. The primary key for status, orchestration binding, everything persisted. |
| **Session handle** | `sess_<uuid>` | Runtime-scoped. Routing only. |
| **Incarnation** | `<paneKey>@<n>` | One per spawn. A relaunched agent in the same pane is a new incarnation. |

Orca's compensating machinery (`terminal_handle_stale`, re-listing after restart) exists because handles rotate when the runtime restarts. **Under D-2 the runtime does not restart when the UI does, so handles are stable across UI upgrades** and most of that machinery is unnecessary.

---

## 4. The structural difference from Orca

This is the part worth being precise about, because it is the whole engineering argument.

Orca splits its processes like this:

```
Electron main  ── RPC server, hook listener, store, git, orchestration DB, UI
       │
       └── daemon-entry.js ── PTYs only
```

Verified on this machine: the daemon was running `appVersion 1.4.195` started Sep 3; the app was `1.4.197` started Sep 12. **The PTYs survived an app upgrade.** That part works, and it is impressive.

But the *runtime* dies with the window. Every app restart therefore rotates:

- terminal handles → `terminal_handle_stale` errors, clients must re-list
- the hook endpoint port and token → mitigated by an `endpoint.env` indirection file rewritten atomically on every start, plus a **disk spool** so hook POSTs that land during the gap are not lost, plus launch-token-hash verification when draining that spool
- orchestration long-polls → `connectionLost`, coordinators must `run-use` to rebind, `consumer_generation` increments and fences outstanding deliveries
- dispatch authority → a whole `restored`-provenance path, plus `legacy_adoptions` tables

That is a large, correct, expensive compensating layer, and **all of it exists to paper over one process boundary being in the wrong place.**

Nysia puts the runtime in the daemon. Then:

- handles don't rotate
- the hook endpoint is stable for the daemon's lifetime → no indirection file, and the spool becomes a rare-path nicety instead of load-bearing
- orchestration long-polls survive a UI restart entirely
- there is no "restored authority" concept, because authority was never lost
- **updating Nysia is: replace the app bundle.** Nothing reconnects, nothing rebinds, nothing is fenced.

This is not a feature. It is the absence of a subsystem, and it is why the headless requirement is worth building around rather than bolting on.

**Cost, honestly:** the daemon must now own git, the store, and orchestration, so it is a bigger binary and a crash takes more with it. Mitigations: SQLite WAL for crash-safe state, `write_atomic` + corrupt-quarantine for JSON, and the daemon supervises nothing it cannot rebuild from disk.

---

## 5. Agent status detection

The single most-wanted mechanism. Orca's design, verified from its bundle, with Nysia's improvement.

### 5.1 How Orca does it

Layered, hooks-first. The header comment in `agent-status-types.js` is unambiguous: *"status comes from hooks (Claude, Codex, etc.) — never inferred from terminal titles."*

| Layer | Mechanism | Authority |
|---|---|---|
| Primary | Agent-native hooks → shell script → **HTTP POST to a localhost listener** | Authoritative |
| Secondary | OSC 0/2 window-title parsing | Only for agents without hooks; never overrides a live hook row |
| Tertiary | PTY tail scraping + foreground-process probe | Only when hooks give nothing (`wait --for tui-idle`) |
| Shell | OSC 133 A/C/D via an injected `ZDOTDIR` wrapper | Shell command state, exit codes, bell |

Orca installs **12 Claude hook events**, each running the same script: `UserPromptSubmit`, `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `TeammateIdle`, `PreToolUse(*)`, `PostToolUse(*)`, `PostToolUseFailure(*)`, `PermissionRequest(*)`, `SessionStart`, `PostCompact`.

Notably **no `Notification` hook** — "waiting" comes from `PermissionRequest` and from `PreToolUse` where `tool_name == AskUserQuestion`.

State mapping:

| Event | State |
|---|---|
| `UserPromptSubmit`, `PostToolUse`, `PostToolUseFailure`, `PreToolUse` (not AskUserQuestion) | **working** |
| `PermissionRequest`, `PreToolUse{tool_name:"AskUserQuestion"}` | **waiting** — carries the question payload verbatim |
| `Stop`, `StopFailure`, `PostCompact{trigger:"manual"}` | **done** (`is_interrupt:true` → `interrupted`) |
| `SessionStart{source: startup\|resume\|clear}` | **done** with `sessionBoundary:true` — **must never notify** |
| any event with `agent_id` | subagent roster update; lead keeps its own state |
| `PreCompact` | deliberately unmapped |

### 5.2 The hook script — four details that matter

1. **`printf "{}\n"` before anything else.** The hook is on the agent's critical path; it returns an empty decision immediately so it can never block or influence the agent.
2. **Hard network caps** — `--connect-timeout 0.5 --max-time 1.5`. Worst case ~1.5s per event.
3. **Spool on failure** to a per-pane JSONL, drained on startup, each record verified against a persisted launch-token hash so a stale or foreign process cannot forge status.
4. **Endpoint indirection** — port and token live in a file rewritten atomically at every app start, because Orca's listener dies with the UI. **Nysia does not need this** (§4): the endpoint is stable for the daemon's life. Keep the spool anyway for daemon restarts; drop the indirection.

### 5.3 Nysia's version

Same 12 hook events, same state mapping — but the hook command is **`nysia hook`**, not a shell script wrapping `curl`.

```
Orca:   hook -> sh script -> curl POST 127.0.0.1:<port>
                             reads endpoint.env (port+token, rewritten every app boot)
                             spools to JSONL on failure
                             Windows needs a .cmd + PowerShell EncodedCommand fallback

Nysia:  hook -> nysia hook -> named pipe / unix socket
                             endpoint stable for the daemon's life
                             peer-credential auth, no token
                             identical on Windows and macOS
```

`nysia hook` reads the payload on stdin, prints `{}` first so it can never block or influence the agent, and writes to the socket. What this removes, relative to Orca: the HTTP server, the port, the token file, the endpoint-indirection file, and the whole Windows script-dialect problem. All of it existed because Orca's listener dies with its UI (§4).

Then:

- the endpoint survives UI restarts, so agents never lose status
- authority is proven by peer credentials + process tree (§3.2), not by token echo
- status is one more table in the daemon's SQLite, readable by the window, the CLI, and later the phone through the identical RPC

Keep the **disk spool** anyway, for the rarer case of a daemon restart: append to a per-pane JSONL, drain on daemon start. It is cheap insurance and Orca proves the shape.

Durability, copied from Orca: cap history at ~20 states per agent; treat a status older than 30 minutes as stale (a "working" dot decays to "active"); mark rows rehydrated from disk as `restoredUnconfirmed` and never count them as fresh until a live hook arrives.

**Shells** get OSC 133 via an injected rc wrapper (`ZDOTDIR` for zsh, `--rcfile` for bash, profile arg for pwsh) that chain-sources the user's real config and never writes into the project or `$HOME`. Claude Code emits no OSC 133 (anthropics/claude-code#22528) — which is exactly why hooks carry the agent path.

**Setting:** this maps 1:1 to the design's *"Agent status hooks — turn off to remove managed hooks"* and to Orca's `agent hooks on|off|status`. Installation must be atomic temp+rename, must preserve foreign hook entries, and must be idempotent.

---

## 6. Orchestration

Orca exposes this as **CLI → JSON-RPC over a Unix socket → SQLite**. There is no MCP server. Nysia should match the shape, because the shape is right.

### 6.1 The core insight

From Orca's own schema comment: *"A Run is only a durable namespace and coordinator inbox; it never schedules or places workers."*

Orca **retired** its automatic scheduler. The coordinating LLM is the loop — it sits in `check --wait` and reacts. The database is a durable mailbox, not an engine. This is why it works: no scheduler to fight, no placement heuristics to be wrong, and the loop is restartable because every fact is a row.

### 6.2 Verb surface to implement

Trimmed from Orca's 30 orchestration verbs to the ones that carry weight:

| Group | Verbs |
|---|---|
| Run | `run-create --objective` · `run-use --id` · `run-current` · `run-list` · `run-show` |
| Task | `task-create --spec [--deps <json>] [--parent]` · `task-list [--ready] [--brief]` · `task-update --id --status` |
| Dispatch | `worker-start --task (--agent\|--terminal) [--worktree new-child\|current\|<sel>]` (**blocks** until ready) · `worker-show` · `worker-read [--source auto\|transcript\|terminal]` · `worker-stop` · `worker-abandon` · `worker-release` · `worker-list` |
| Messaging | `send --type status\|worker_done\|escalation\|heartbeat\|question --task-id --dispatch-id [--outcome]` · `check [--wait] [--ack <delivery>]` (**long-poll**) · `inbox` |
| Blocking Q&A | `ask --question [--options] [--timeout-ms]` (**blocks**) · `reply --id --body` |
| Gates | `gate-create --task --question [--options]` · `gate-resolve --id --resolution` · `gate-list` |
| Idempotency | every mutation takes `--retry-request <id>`; `request-show --request` |

Semantics worth copying exactly:

- **FIFO deliveries with explicit ack.** `check` returns the oldest unacknowledged batch (≤50 messages) and *replays the same batch* until `--ack`. One outstanding delivery per run, enforced by a unique index. This is what makes a crashed coordinator resumable.
- **`worker_done` requires `--outcome` plus both task and dispatch ids**, and auto-settles both. Requiring the dispatch id is what stops a late completion from a failed retry settling the current attempt.
- **Circuit break at 3 failures** on a dispatch context.
- **Mutation receipts** keyed `(caller_fingerprint, request_id)` → makes every mutation idempotent and makes "did it land?" answerable.
- **Long-poll keepalives** — interleave `{"_keepalive":true}` frames so the client can distinguish a slow answer from a dead socket.
- **Every error carries `nextSteps` / `nextCommandArgs`**, so an agent can self-recover instead of hallucinating flags.
- **A machine-readable command catalogue** (`nysia agent-context --json`) — Orca's is 234 commands with usage, flags, and examples. This is how you stop agents inventing syntax.

### 6.3 The worker preamble

Orca injects a preamble into every dispatched worker. Three rules in it are hard-won and should be copied verbatim in spirit:

1. **Never use `AskUserQuestion`** — it opens a local TUI prompt the coordinator cannot see or answer, and the worker hangs forever. Use `ask` instead.
2. **Send `worker_done` exactly once**, with a three-sentence executive summary and both ids.
3. **Heartbeat every 5 minutes** while working; skip only while blocked inside `check --wait` or `ask`, which are themselves liveness signals.

### 6.4 What Nysia adds

Orca has **no merge/landing primitive** — no `merge` verb, no conflict-aware landing queue, no way to compare N attempts. The strongest path it offers is "open a PR". Given Nysia inherits nightcore's worktree merge machinery (abort-not-force, read-only `merge_preview`, `update_from_base`), a first-class `land` verb is available cheaply and is a genuine gap in the category.

---

## 7. Subsystems

### 7.1 PTY

`portable-pty` (wezterm), one blocking reader thread per session. **Pin to `main`, not crates.io 0.9.0** — the published release does not pass the ConPTY `RESIZE_QUIRK` / `WIN32_INPUT_MODE` flags.

Windows, since that is the primary dev platform:
- Bundle `conpty.dll` + `OpenConsole.exe` (MIT) so behaviour does not depend on the user's Windows build.
- `ClosePseudoConsole` **blocks until the client exits and deadlocks if called from the thread reading the output pipe.** Always close from a non-reader thread after the reader has drained.
- Tree kill needs a **Job Object** with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. ConPTY has no signals.

Unix:
- Child is a session leader (`setsid`), so `killpg(SIGTERM)` → grace 1–3s → `killpg(SIGKILL)`.
- Linux daemon sets `PR_SET_CHILD_SUBREAPER` so orphans reparent to it. macOS has no subreaper — sweep by session id on kill and on daemon start.
- A failed `execve` returns `Ok` from spawn and the child dies silently (wezterm#7893, caused by `close_random_fds` closing Rust's exec-error pipe). **Pre-validate program paths.**
- EOF only arrives when *every* slave fd closes — a backgrounded server keeps the master alive after the shell exits. Decouple "child exited" from "PTY EOF".
- Linux is neither a CI target nor a shipping platform, and the orphan test fails there on `main`, independent of any change near it. Recorded because the `cfg(unix)` paths are runnable from a Windows desktop in a container, so the failure is met by whoever does that next rather than by CI. The measurements behind the signal-ignoring guard, and why #21's proposed longer sleep was backwards, are in the doc comments beside those two tests.

Env hygiene: scrub `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, and specifically `CLAUDE_CODE_CHILD_SESSION` / `CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_BRIDGE_SESSION_ID` (Orca deletes exactly these) or a launched `claude` is misclassified as a child session. Force `TERM=xterm-256color`, `COLORTERM=truecolor`.

### 7.2 Terminal state and agent-readable output

Per session in the daemon: an `alacritty_terminal` 0.26 grid (alt-screen aware) plus a raw-byte replay ring. The VT handler intercepts OSC 133 / OSC 7 before forwarding.

Plus a **logical line log**: when a row scrolls out of the viewport, append its plain text with a monotonic line id. This gives cursor-paged agent reads for free and avoids Orca's known footgun — its default `terminal read` returns the accumulated escape-stripped stream, so a `clear` typed key-by-key reads back as `cclclecleaclear`. Orca's own help text apologises for it.

**Nysia defaults `terminal read` to the rendered screen.** Stream mode is opt-in.

Agent-facing verbs: `terminal read [--screen|--stream] [--cursor] [--limit]`, `terminal wait --for exit|idle`, `terminal send [--text] [--enter] [--interrupt]`.

For anything Nysia runs *on behalf of* an agent, spawn the command as the PTY child directly and take the exit status from `wait()` — no escape sequences, no ambiguity. OSC 133 is the "block" model for humans typing; process exit is the "job" model for automation.

### 7.3 Rendering and transport

xterm.js pinned to the beta line VS Code and Orca actually ship: `@xterm/xterm` 6.1.0-beta.30x + `@xterm/addon-webgl` 0.20.0-beta.30x. xterm 6 removed the canvas renderer — it is WebGL or DOM, nothing between.

- **Windows/WebView2 and Linux**: WebGL on by default.
- **macOS**: WebGL is a pooled opt-in — max 6 contexts, LRU-evict hidden panes, `onContextLoss → dispose → DOM`. xtermjs#5816 (atlas corruption in WebKit) is still open; WebKit also hard-caps live WebGL contexts at 16 app-wide. omniscribe's `webglPool.ts` is a working implementation to lift.
- Only **visible** panes get a renderer. Hidden sessions cost nothing in JS because Rust holds the state.
- Keep a `TerminalSurface` adapter so `ghostty-web` (canvas-2D, no WebGL at all) can be swapped in as a second implementation later.

Transport: **one multiplexed binary `tauri::ipc::Channel`. Never `emit`** — tauri#12724 (memory leak on sustained emits) is open. Critical threshold from Tauri's source: raw payloads **under 1024 bytes go through `eval`**; larger ones through a fetch queue. So coalesce to **≥1 KiB** frames, flushing on 64 KiB or 16 ms.

Backpressure: credit windows, using Orca's production constants as defaults — per-stream 512 KiB initial / 2 MiB max, total 2 / 8 MiB, pending cap 256 KiB, ACK batch 192 KiB, chunk 48 KiB. The web side acks after xterm's `write()` callback; at zero credit Rust stops reading, the kernel PTY buffer fills, and the child blocks. That is the complete `yes`-flood answer.

On overflow for a hidden pane: drop the **whole** transient buffer and inject `ESC c`. Never cut mid-sequence — a partial cut corrupts xterm's parser.

### 7.4 The Claude module boundary

No `AgentProvider` trait in v1. Instead: all Claude specifics live in one module, and a lint rule forbids anything outside it from importing them.

nightcore's provider-coupling audit is the evidence for this. Its seam was designed against one implementation; when Codex arrived, `StartSessionParams` fields were silently dropped, scans hardcoded an autonomy level Codex refuses, and every Codex scan failed on first run *from the UI*. A trait extracted from one implementation encodes that implementation's assumptions and calls them universal.

When Codex lands, extract the trait from two real implementations. Same protection now, none of the speculative generality.

### 7.5 Worktrees, git, and confinement

Lifted near-verbatim from nightcore, where these modules are already zero-Tauri and lint-fenced:

- **`git_command` chokepoint** — every git spawn prepends config neutralizers (`core.fsmonitor=`, `core.hooksPath=/dev/null`, `core.pager=cat`, `diff.external=`, `core.sshCommand=`, `core.gitProxy=`) and scrubs 11 `GIT_*` overrides plus exec vectors (`LD_PRELOAD`, `GIT_SSH_COMMAND`, `GIT_ASKPASS`…). Without this, an agent that writes `.git/config` gets host code execution. Uses `env_remove`, not `env_clear`, so credential helpers keep working.
- **`safe_join` / `path_confine`** — lexical rejection → execution-sink denylist (`.git/`, `.github/workflows/`, `.claude/`, `package.json`, `.envrc`…) → lstat symlink-walk containment.
- **worktree module** — allocate / remove / reconcile, `remove` refuses anything outside the base dir, merge is abort-not-force, `merge_preview` is read-only, and a loop refuses to start on a dirty base.

**Keyed by branch, not task** (D-6). Path shape `<project>/.nysia/worktrees/<branch-slug>`.

**Platform reality:** nightcore's Seatbelt/SBPL confinement is macOS-only. Windows has no OS-level containment, and since Windows is the primary dev platform, **v1 confinement on Windows is lexical gates only.** Say so in the docs rather than implying isolation that is not there. Note that Orca ships *no* confinement at all — it runs agents with `--dangerously-skip-permissions` in real checkouts — so lexical gates alone still put Nysia ahead.

### 7.6 Usage metering

**Default — statusline shim (ToS-clean).** Claude Code pipes `rate_limits.five_hour.used_percentage`, `.resets_at`, and `rate_limits.seven_day.*` into the user's statusline command as JSON on stdin, for Pro/Max. Nysia installs a `nysia-statusline` shim that appends this to the daemon and then execs the user's existing statusline command. Clean because the Claude Code binary makes the request.

Limitation to surface in the UI: it only refreshes while a Claude session is alive. The popover must show **"updated N min ago"**.

**Behind an off-by-default flag — the OAuth path.** Read the Keychain credential and call `/api/oauth/usage` directly. Always fresh, includes the Fable window, matches the design mock exactly. Orca ships this today. But Anthropic's legal page states developers "may not collect, store, or intermediate Claude.ai credentials or session tokens," and enforcement has been reported. The setting must state that plainly at the point of enabling — not bury it.

**Never refresh the OAuth token** under either path. Claude's refresh token rotates; consuming it logs the user's CLI out.

### 7.7 Tasks = GitHub Issues

- **Auth**: reuse the `gh` CLI token (`gh auth token`), with a fine-grained PAT fallback. Conductor and Claude Code both do exactly this. Graduate to a GitHub App only when a relay exists to receive webhooks.
- **Sync**: poll `GET /repos/{o}/{r}/issues?state=all&sort=updated&since=<iso>` with `If-None-Match`. **304s do not count against the 5,000/hr limit**, so a 10-project board at 60s costs ~nothing. Honour `x-poll-interval`. Keep query params byte-identical between polls or the ETag never matches.
- Issues endpoints return PRs too — filter on the `pull_request` key.
- **Issue → branch**: the `createLinkedBranch` GraphQL mutation, so the branch appears in the issue's Development sidebar. There is no REST equivalent, and no public mutation to link an *existing* PR to an issue — use `Closes #N` and read `closingIssuesReferences`.
- Key tasks by `(host, owner, repo, number)`; GitHub owns issue fields, the local store owns run state. Never mirror local "in progress" as a GitHub state change without opt-in — it spams collaborators.

### 7.8 Computer use

**Broker, do not implement.** Claude Code already ships a `computer-use` MCP server with per-app approval, a machine-wide lock, terminal-excluded screenshots, and an Esc kill switch. It works **only in interactive sessions, not with `-p` or the SDK** — which is a second independent argument for the PTY-first model, since Nysia's sessions are exactly that.

Route to what is present (Claude Code's MCP, Claude in Chrome, Orca's CLI if installed). Writing a screenshot/coordinate loop means re-deriving Anthropic's entire guardrail list, and macOS TCC grants are keyed to bundle id **and signature**, so every unsigned dev rebuild loses them.

---

## 8. Design system

From `Nysia ADE.dc.html`. Full extraction in `docs/design/design-spec.md`.

**Two themes** — Ember (cool blue-black, `#090b10` → `#dfe3ea`) and Graphite (neutral, `#0f0f11` → `#e6e4e0`). **One live-changeable accent**, default amber `#f2b35b`, with teal / violet / pink presets; `--acc14` and `--acc35` are derived alphas.

Every colour must be a token — there is a runtime theme switcher, so a hardcoded hex is a bug.

**Status colours are deliberately independent of the accent**: running `oklch(78% 0.12 180)`, needs-input `oklch(80% 0.15 70)`, queued `#7c7c84`, failed `oklch(70% 0.15 25)`. Accent is identity; status is semantics. The accent picker must not recolour them.

Type: Space Grotesk (UI), Fira Code (mono/terminal/metrics). Base 13px, terminal 12.5px/1.65.

Chrome: 40px titlebar (wordmark · tab strip · `+` menu · `⌘K` · custom window controls) / body `48px rail | 222px sidebar | 1fr` / 30px status bar. Custom chrome on all platforms.

Three screens are specified: **Session**, **Tasks**, **Settings › Agents**.

---

## 9. Scope ladder

| | Scope | Proves |
|---|---|---|
| **v0.1** | Daemon + socket protocol · PTY sessions (shells) · Rust VT state · one multiplexed Channel · projects sidebar · tabs · Ember/Graphite + accent · Settings: General + Appearance | **The walking skeleton**: kill the UI, the shell survives; reattach, scrollback replays. If this does not work, nothing else matters. |
| **v0.2** | Claude agent sessions · hook install/uninstall · status → sidebar + tabs · notifications | The design's live status dots. |
| **v0.3** | **Projects: register a folder, persist it, serve it** · GitHub Issues view · `Start →` creates branch-keyed worktree + session + tab | The first thing that is not a terminal. |
| **v0.4** | Worktree manager · merge/diff/discard · usage meter (statusline shim) | |
| **v0.5** | Orchestration verb surface · `nysia agent-context --json` · worker preamble · `land` | The differentiator. |
| **v0.6+** | Second provider (Codex) → extract the trait · browser tab · relay + mobile approvals | |

**Project registration moved into v0.3 (2026-09-15).** It was written into v0.4 beside the
worktree manager, and the sidebar's disabled `+` still says so. That ordering was wrong: v0.3's
`Start →` creates a **branch-keyed worktree in a project**, so it cannot be built before a
project exists. Today the sidebar's list is the web mock's seed — `nysia-proto` carries no
project type at all and the daemon has never heard of one. Registration is therefore the first
thing v0.3 builds, and the Issues view sits on top of it. Browse-a-folder is the path that
unblocks the rest; clone-from-URL and create-new-folder are follow-ons, not prerequisites.

**v0.1 must include the daemon split.** Retrofitting a process boundary is the refactor measured on nightcore this morning: 98 of 322 files threading a god-object. A week now; a month later, and realistically never.

---

## 10. What we lift, and from where

**From nightcore** — `infra/platform.rs` (`git_command`), `infra/safe_join.rs`, `infra/path_confine.rs`, the `git/` and `worktree/` modules (already zero-Tauri), the zod→Rust→TS codegen pipeline *including its prove-the-gate-trips guards*, `store/atomic.rs` (`write_atomic` + `quarantine_corrupt`), the lint-meta engine, the terminal daemon skeleton, the CI shape.

**From omniscribe** — `osc-agent-detector.ts` (byte-stream OSC state machine, never output heuristics, so a repainting TUI cannot make status flap), `shell-integration.service.ts` (versioned, hash-checked rc snippets in userData that chain-source the user's real config), `hook-manager.service.ts` (atomic, foreign-preserving, idempotent hook installation), `buildSafeEnv`, `webglPool.ts`.

**From the ERP template** — `pre-push.manifest.json` + the parity lint rule that makes "passes pre-push, fails CI" structurally impossible; the component/feature scaffolders; the split eslint-config shape.

**Explicitly not taken** — the kanban and the Task domain model, the scan features, Council, the PR system, the Bun sidecar tier, the Agent SDK.

---

## 11. Traps register

Each of these cost someone a day already.

1. **Cargo must run with cwd = the crate dir**, never `--manifest-path` from the root. Cargo finds `.cargo/config.toml` by walking up from cwd; from the root the ts-rs env is unset, bindings land somewhere gitignored, and the drift guard passes vacuously.
2. **Every Tauri command is `async fn` + `spawn_blocking` + `try_state`.** A sync command runs on the main thread and freezes the webview. Never `tokio::spawn` inside one — the panic unwinds across wry's `extern "C"` boundary and becomes `abort()`.
3. **Never `emit` for streams.** Binary `Channel` only.
4. **Coalesce to ≥1 KiB** or payloads go through `eval`.
5. **Pin `portable-pty` to `main`** for ConPTY flags.
6. **`ClosePseudoConsole` from a non-reader thread**, after draining.
7. **Windows spawn**: `Command::new("bun")`/`("claude")` fails with NotFound because npm installs `.cmd`/`.ps1` shims and `CreateProcess` only launches real executables. Resolve with a `which`-style lookup.
8. **Never bake `CARGO_MANIFEST_DIR`** into anything path-shaped — a CI-built release once shipped the runner's path.
9. **ESLint flat-config `no-restricted-imports` does not merge across blocks** — the later matching block silently replaces the earlier. Repeat the full ban set in each.
10. **Bun `Glob` has no `{a,b}` brace alternation** — it silently returns `[]`.
11. **macOS ad-hoc signing (`-`) is a Gatekeeper dead end.** Developer ID + notarization is required, and mandatory if computer use is ever enabled.
12. **The updater's minisign key bricks all clients if rotated or lost.** Vault it before the first release.
13. **Every gate ships with a proof that it trips.** nightcore found four CI checks passing without exercising anything.
14. **Scrollback can contain secrets** — owner-only files, excluded from exports and diagnostics bundles.

---

## 12. Open questions

> **Wave 3 (2026-09-14).** The v0.1 acceptance criterion — *kill the UI and the shell survives;
> reattach and the scrollback replays* — was driven against the real app on Windows 11 and
> **passed**: the window was closed with the × in its title bar, the daemon and the same
> `cmd.exe` pid kept running with the same session handle and pane key, and the relaunched app
> brought the tab back with its scrollback and took input again. §4's upgrade claim — *updating
> Nysia is: replace the app bundle; nothing reconnects, nothing rebinds, nothing is fenced* —
> **held**, verified against a genuinely different binary: same daemon pid, same launch nonce,
> same handle, same shell process, scrollback intact. One honest footnote on that: Windows
> refuses to overwrite a running image (`os error 5`), so the upgrade flow *is*
> close → replace → relaunch, and what §4 claims spans all three is the daemon, not the app.
>
> The proofs, in the order of how much they can prove on their own:
> `crates/nysia/tests/survival.rs` (CLI-only, in CI),
> `interop::a_relaunched_window_finds_the_session_and_replays_its_scrollback` (the window's own
> client against a real daemon, in CI), and `scripts/e2e/walking-skeleton.ps1` (the real app,
> by hand, because opening a tab and closing a window are things a person does). Questions 2
> and 3 below are answered with measurements from `scripts/e2e/measure-memory.ps1`; questions 6
> and 7 are what driving the real GUI turned up. Both were open when this was written and
> neither is now: 6 was fixed in wave 4, and 7 took two changes — the replay boundary it asked
> for, and then a second one after that fix's rationale turned out to rest on something untrue.

1. **Windows confinement.** Lexical gates only in v1 (§7.5). Is there an acceptable OS-level story later — AppContainer, a restricted token, or WSL-only agent sessions?
2. **Scrollback persistence budget.** ~~Orca: 5k rows default, 512 KiB replay, 5 MiB store, up
   to 200 MB history checkpoints. What is Nysia's ceiling…~~
   **Answered for v0.1 (wave 3).** The ceiling is chosen and enforced; the persistence half of
   the question is answered and the answer is *none*.

   Per session, from `VtConfig::default` in `nysia_core::vt::state`:

   | | Nysia v0.1 | Orca |
   |---|---|---|
   | raw replay ring | **256 KiB** | 512 KiB |
   | logical line log | **10 000 lines, capped at 4 MiB of text** | 5 000 rows, 5 MiB store |
   | grid history | **4 096 rows**, and it is a *staging area*, not the scrollback | — |
   | on-disk history | **none** | up to 200 MB of checkpoints |

   The grid row count is deliberately not the scrollback: §7.2's split means the viewport is
   the grid, the scrollback text is the line log, and a reattaching client replays raw bytes.
   Three ceilings rather than one because each bounds a different unbounded thing — a stream
   with no newline at all grows the line log forever unless the byte budget stops it.

   **It survives neither a crash nor a clean restart.** All three live in the daemon's memory
   and `nysia_core::store` is still a module-level doc comment: nothing is written to SQLite in
   v0.1, so a daemon that stops for any reason takes every session's scrollback with it — which
   is the same answer question 4 gives for the sessions themselves, for the same reason. This
   is a real regression against Orca's 200 MB of history checkpoints, and it is the honest
   state of v0.1 rather than a design position. **Still open:** whether the line log should be
   the thing that persists (it is text, it is already bounded, and it is what an agent reads)
   while the replay ring stays transient, which is the shape the split in §7.2 suggests.

3. **Memory expectations.** ~~Orca on this machine: app 1.1 GB, 356–817 MB *per worktree*, PTY
   daemon 88 MB for 5 terminals…~~
   **Measured in wave 3**, on the same machine as the Orca figures, with
   `scripts/e2e/measure-memory.ps1` — a fresh daemon per row, every shell settled before the
   reading, and the children counted rather than quietly left out. `cmd` sessions, because this
   machine has no PowerShell 7; a `pwsh` child is larger, so the per-session column is a floor.

   | sessions | daemon WS | daemon private | children | children WS | total WS |
   |---|---|---|---|---|---|
   | 1 | 11.5 MB | 4.6 MB | 3 | 26.7 MB | 38.2 MB |
   | 5 | 12.9 MB | 14.4 MB | 11 | 92.3 MB | 105.2 MB |
   | 10 | 14.7 MB | 26.8 MB | 21 | 174.3 MB | 189.0 MB |

   Working set and private bytes both, because they answer different questions and the Orca
   comparison figures were read off Task Manager, which shows the former.

   **The honest claim, in three parts.**

   - **The runtime is genuinely small.** Orca's PTY daemon is 88 MB for five terminals; Nysia's
     is **12.9 MB** for five, roughly one seventh, and it grows about **2.2 MB per session** —
     which is the VT state and the ring above, not a leak. That is the part D-1 moved, and it
     is the part that got cheaper.
   - **The window is not.** Measured with one shell tab open: `nysia-desktop.exe` itself is
     28.2 MB, but it spawns **six WebView2 processes** that bring it to **409.6 MB working set
     / 205.7 MB private**. Against Orca's 1.1 GB app that is about 2.7× better and it is not a
     rounding error — but "Tauri is tens of megabytes" is false, and the doc's own warning that
     Tauri saves the Electron renderer rather than the expensive part is the right one. Counting
     only the Tauri exe would have produced a flattering 28 MB and would have been dishonest.
   - **The expensive part is not measured here at all.** Orca's 356–817 MB *per worktree* is
     agent processes, and v0.1 ships no agent sessions (D-3/D-4 land in v0.2), so there is no
     Nysia number to compare and claiming a win would be claiming credit for a feature that does
     not exist. On the shells themselves Nysia has no advantage and expects none: a `cmd` plus
     its `conhost` costs about **16.4 MB** whoever spawns it.

   **Still open:** what an agent session costs once v0.2 lands, which is the only number that
   decides whether the per-worktree figure improves.
4. **Daemon crash blast radius.** ~~Under D-2 the daemon owns more than Orca's does.~~
   **Answered in wave 2 (W4).** A crash takes the sessions with it, and nothing pretends
   otherwise.

   - **The PTYs die with the daemon, by construction.** On Windows every child is in a Job
     Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the kernel tears the tree down when
     the last handle closes — which is exactly what a crash does. On Unix the children are in
     the daemon's process tree. This is not a regression against Orca, whose PTY daemon has
     the same property; it is the cost §4 names, paid knowingly in exchange for deleting the
     compensating layer.
   - **Nothing supervises the daemon in v0.1, and that is deliberate.** A supervisor that
     restarts a daemon whose sessions are already gone restores an empty runtime, which is
     what the next client does anyway by spawning one. Adding a supervisor before there is
     state worth resurrecting would be machinery with nothing to protect.
   - **What a crash leaves behind is safe to find.** The lease file outlives the process, so
     nothing may read its presence as proof of life — see question 5. On Unix the socket file
     outlives it too and refuses connections; on Windows the pipe name stops existing with the
     last handle. Both are the "nothing is listening" answer, and both lead a client to start
     a fresh daemon rather than attach to a corpse.
   - **What is *not* lost:** the clients. Every connection simply fails, and the CLI's error
     envelope says to retry — which starts a daemon. There is no fencing, no generation
     counter and no "restored authority" path, because authority was never delegated.
   - **Open beyond v0.1:** once the store holds sessions worth resurrecting (v0.4+), the
     question becomes whether a restarted daemon should re-spawn the shells it lost. Today it
     cannot: a shell's process state is not in SQLite and never will be.
5. **How does the GUI discover and start the daemon?** ~~Spawn-if-absent on app launch is
   obvious, but two clients racing to spawn needs a lock…~~
   **Answered in wave 2 (W4), and true of the window since wave 4.** One implementation of the
   race, in `nysia_core::rpc::discovery`, with both front ends inside it — but *how* they are
   inside it is worth stating exactly, because an earlier version of this answer said "the
   window links it like everyone else" and the shipped window did not (q6 below, now closed).

   **The seam is synchronous and the caller supplies the dial.** `discovery::ensure_daemon`
   holds the policy: probe, take the spawn lock, probe again, spawn `program --daemon`, then
   probe until it answers or twenty seconds pass. It is generic over a caller-supplied probe
   because the two front ends cannot share a dial: `nysia <verb>` is `async` over this crate's
   tokio transport, and the window's transport is blocking by construction — every Tauri
   command is `spawn_blocking`, and a runtime inside one is how a panic becomes `abort()`.

   - `nysia <verb>` calls `discovery::discover`, which dials once itself — a daemon that is
     already listening is the common case and costs one round trip — and then runs
     `ensure_daemon` on a blocking thread with its async dial bridged in.
   - The window calls `ensure_daemon` directly from `state::Client::connect`, with
     `daemon::control::Control::connect` as the probe. Outside the client lock, because
     starting a daemon takes seconds and this module's one rule is that nothing waits on
     another thread while holding it.

   The probe answers two things and never three: *something answered* — with the identity the
   lease is then checked against — or *nothing is listening*. A refusal is an error, because
   "the daemon refused my version" and "there is no daemon" want opposite responses, and
   folding them together is how a client starts a second daemon beside one that was perfectly
   willing to talk to somebody else.

   **What the window spawns is a file it ships.** The `nysia` binary is declared as an
   `externalBin`, so the bundle carries the same artefact the CLI installs (D-11), and the
   window resolves it *beside its own executable* — `Contents/MacOS/nysia` in the app,
   `nysia.exe` next to `Nysia.exe` on Windows, and the sibling cargo built in `target/` during
   development. One rule, both worlds. Never `PATH`: a window that started whichever `nysia` a
   shell happened to put first would be running a daemon nobody chose.

   **The declaration lives in `tauri.bundle.conf.json`, which only the bundler merges, and
   that is D-1 defending itself.** `tauri_build` acts on `externalBin` at *compile* time: for
   every `cargo build` of the desktop crate it deletes `target/<profile>/nysia` and copies the
   staged file over it, and the delete is an `unwrap`. But the daemon **outlives the window by
   design**, so "run the app, close it, build again" is the ordinary sequence — and the daemon
   still running is holding the file about to be deleted. **Holding it stops the delete only
   once that file is Tauri's own single-linked copy.** Windows unlinks a running image
   while another name for it survives, so a daemon started from the two-name hardlink cargo
   leaves from `deps/` does not trip it: measured, the unlink succeeds through the other name,
   the link count goes from two to one, and the build finishes. It is the copy Tauri writes over
   it — one name, and a daemon running from that name — where the delete fails, so the build
   panicked with `PermissionDenied` as soon as that copy had replaced the hardlink.

   **What that bought: every ordinary `cargo` command.** `cargo build`, `cargo test`, `cargo
   clippy` and `tauri dev` neither need the sidecar nor touch `target/<profile>/nysia`, so a
   developer's window starts the runtime cargo just compiled rather than a copy of an older
   one, and the inner loop — the one D-1 made dangerous, because the daemon from the last run
   is still up — no longer has a delete in it. The flag that merges the declaration lives in
   `apps/desktop/package.json`, so `pnpm build:app` and CI bundle the same way.

   **What it did not buy: the bundle build itself, which still deletes.** `tauri build
   --config` hands the merged config to the build script in `TAURI_CONFIG` (tauri-cli 2.11.4
   `helpers/config.rs:186`); `tauri-build 2.6.3` merges it at `lib.rs:487` and reaches the same
   `copy_binaries` at `:546`, whose `fs::remove_file(target/<profile>/nysia).unwrap()` at `:80`
   panics `PermissionDenied` when a daemon is running from that exact file. What makes that
   tolerable is that bundling is a deliberate, occasional command rather than the inner loop,
   and that the remedy is one command: stop the daemon holding `target/<profile>/nysia`, then
   bundle again. What it takes to trip is a daemon running under **the profile being
   bundled** — build the app, run it from `target/release`, close the window, bundle again;
   or `tauri build --debug` against the daemon a `tauri dev` window left behind, which is how
   it was reproduced. The profiles are not reliably disjoint, so that second one is not
   exotic: `--debug` puts the bundle build straight onto the file the dev loop is running.
   CI never meets any of it, because nothing is running there.

   **And there is no staging move that fixes it,** which is why this claim is narrowed rather
   than the build changed. The window resolves its runtime beside its own executable and one
   rule serves both worlds, so the file has to be called `nysia` and has to land beside the
   window — which is exactly the file a release daemon runs from. Renaming the sidecar only
   moves the delete to a path the window would not then look in. Windows will *rename* a
   running image even though it refuses to unlink a single-linked one, so moving the file
   aside before the bundle looks tempting; it was measured and not taken, because the profile
   such a step would have to guess is the one thing a package script cannot see, and guessing
   wrong deletes the runtime beside a window somebody is running.

   **When it cannot spawn, the window says so and stops — and "cannot" is now two cases, not
   three.** A missing sidecar and a file that will not execute do not improve by waiting, so
   each is still a non-retryable `DaemonError::Spawn` carrying the path that was tried or the
   log that says why. `DaemonStore.run` **returns** on that answer rather than looping: a retry
   re-takes the spawn lock, starts a process and waits twenty seconds for it, so a window that
   kept trying would start one every twenty-odd seconds for its whole life and append to the
   daemon log each time. The next steps say to reopen Nysia, because a window that has stopped
   is not going to notice the fix on its own.

   **A runtime that ran and has not answered is the third case, and it was wrong to call it
   permanent.** A readiness wait bounds one call; it is not a verdict on the daemon, and
   Defender scanning a binary it has never seen pushes a first bind past twenty seconds on
   exactly the machine where a first launch happens. So that case is `DaemonError::Starting`,
   which is **retryable**, and the window keeps probing on the store's backoff — 250ms, 500ms,
   1s, 2s, then 5s — for `PROBES_BEFORE_RESPAWN` (12) probes, a little over three quarters of a
   minute on top of the readiness wait that came first. Then it starts one more runtime, and
   only one: `RUNTIME_STARTS` is 2, because a runtime can die on its way up for a reason that
   does not repeat and an unbounded supply of them is the defect this bound closes, arriving a
   minute at a time instead of twenty seconds at a time. After the second the failure is
   permanent, the store ends its reconnect loop, and the notice on screen is the last word:
   *Nysia started its runtime 2 times and none of them answered*, with the log named and
   reopening offered.

   **Only two dial failures mean "nothing is listening".** `NotFound`, and on Unix
   `ConnectionRefused` — a socket file outlives its daemon and refuses. Everything else is an
   error rather than a licence to spawn: a busy pipe or an access denial means a daemon *is*
   there, and starting a second one against it produced a process that could not bind, twenty
   seconds of waiting, and a complaint pointing at a log whose only line said the endpoint was
   already held. The window's classification is the one `nysia_core::rpc::transport` uses, and
   the two are checked against each other in `daemon::endpoint`'s tests.

   **Liveness is three steps, and never the pid.** A pid that still exists says nothing: pids
   are reused, and attaching to whatever now holds an old number is the failure the launch
   nonce exists to prevent. So:

   1. **connect** to the versioned endpoint;
   2. **`hello`**, and read back `daemonIdentity`;
   3. **`PidRecord::describes`** — the lease beside the endpoint must describe *that*
      identity. When it does not, something is listening and it is not what the file says,
      which a crash and an immediate restart produces. The connection is still good; the
      record is what is stale.

   **Stale endpoints differ by platform, and only one platform has the problem.** A Unix
   socket file outlives its daemon and refuses connections — a far stronger signal than the
   file's existence, and the one discovery acts on. A Windows pipe *name* is the object and
   stops existing when the last handle closes, so there is no such thing as a stale pipe.

   **The race is settled by the kernel, twice over.** Whoever binds the endpoint wins:
   `first_pipe_instance` refuses the second creator on Windows, `bind` fails with `AddrInUse`
   on Unix, and a daemon that loses exits zero rather than reporting a failure — what it was
   started to guarantee is true. In front of that sits a spawn lock held by the operating
   system (`flock`, or an exclusive Windows share mode) rather than a flag written in a file,
   so a spawner killed mid-spawn releases it and there is no stale lock to reason about. The
   loser of the race does not fail; it waits for the winner's daemon and uses that one.

   **Idle retire is not a verb.** §3.1 sketched `shutdownIfIdle` as something a caller asks
   for; `nysia-proto` ships no such verb and proto is the sole authority on the wire (D-13),
   so the daemon decides for itself: no clients, no sessions, nothing in flight, sustained for
   a grace period. **A daemon holding a session never retires**, and the grace only arms once
   a client has connected and gone, so a freshly spawned daemon does not exit in the moment
   before its spawner dials in.

   **One trap worth writing down.** On Windows a redirected spawn inherits *every* inheritable
   handle, not only the three that were named — including the write end of the pipe the
   spawner's own stdout happens to be. A daemon started from inside `H=$(nysia session
   create)` therefore held that pipe open for its whole life and the shell waited forever for
   output it already had. The parent's standard handles are detached before the spawn, and
   `crates/nysia/tests/survival.rs` has a regression test that reports the hang rather than
   hanging on it.

6. **The window could not start a daemon, and no build of the app shipped one.**
   ~~Found in wave 3 by launching the app on a machine with no `nysiad` running, which is what
   every first launch is.~~ **Fixed in wave 4.** The window comes up on a machine with nothing
   listening, starts the runtime it ships with, and opens a tab.

   What was wrong, recorded because the shape of it is the lesson:

   - `Client::connect` in `apps/desktop/src-tauri/src/state.rs` called `Control::connect` on
     the resolved endpoint and stopped there. It never reached `discovery`, where the spawn
     lock, the re-probe and the readiness wait live — and it could not, because `discover` is
     `async` and hands back a `nysia_core` client while the window's transport is synchronous.
   - Even with the code there would have been nothing to spawn: `tauri.conf.json` declared no
     `externalBin` and no `resources`, so a bundled `Nysia.exe` / `Nysia.app` contained no
     `nysia` binary. It worked on a developer machine only because `cargo build` leaves both
     binaries in one `target/` directory.
   - And question 5 **claimed the opposite was already true**, which was the expensive part:
     the next person reads the answer, believes the window links discovery like everyone else,
     and does not look. A doc that is wrong about the shipped code is worse than a gap it
     admits to.

   Both halves are closed by the synchronous seam and the sidecar described in question 5.
   What proves it, at three levels:

   - `state::tests::a_window_with_no_daemon_anywhere_starts_one_and_reaches_a_working_session`
     — the window's own client, from nothing listening to a shell that has run what was typed
     at it. Run against the code before the fix it fails on the first line that matters, with
     the `Unreachable` a user saw as *Reconnecting*.
   - the `bundle` job in CI, which builds the app on both runners and looks **inside what a
     user receives**: the macOS `.app`, and on Windows the installer run to a prefix of its
     own — a check of `target/release` alone would have passed with the sidecar declaration
     deleted, because the build step above it had just written that directory. `pnpm
     prove:sidecar` proves the check trips on a bundle missing its runtime (traps
     register #12).
   - `scripts/e2e/first-launch.ps1`, for the two things neither can do: launching the
     *installed* app so the sidecar is found where Tauri put it rather than where cargo did,
     and reading the notice a window shows when its runtime has been taken away.

7. **Re-attaching replays queries as well as output, and the shell reads the answers as
   input.** *(Resolved — see the end of this entry.)* Deterministic, and it was in D-1's
   flagship path. On every `stream_attach` the window
   issues two `terminal_send` calls that nobody typed — visible in a daemon debug log at first
   attach and again at every re-attach, with no keystrokes in between.

   The chain: the daemon replays the raw scrollback on attach (which is the whole point, §7.3),
   `apps/web/src/transport/terminals.ts` writes those bytes into xterm, and xterm answers the
   terminal queries that are *in* them — a replay contains every `ESC[6n` and `ESC[c` the child
   ever wrote, and a parser cannot tell a replayed query from a live one. `XtermSurface`
   forwards every `onData` straight to `terminal_send`, so the answers go to the child as
   keystrokes, and ConPTY translates the `…R` of a cursor-position report into F3 — which is
   `cmd`'s recall-previous-command.

   Harmless on a first attach, because there is nothing to recall — which is the reading that
   turned out to be wrong, and the correction is further down. On a re-attach the pane comes
   back showing a phantom command line the user never typed, sitting unsubmitted in the line
   editor, so the next thing they type is concatenated onto it. Measured: after a relaunch,
   typing `echo STILL-ALIVE` ran `echo NYSIA-%NYS%echo STILL-ALIVE` and printed
   `NYSIA-42echo STILL-ALIVE`. One `Esc` clears it and the session is fine, which is why the
   acceptance criterion still passes — *the shell survives and the scrollback replays* — but a
   person who does not know to press it loses their next command.

   **The first change: the replay boundary this entry asked for**, as a frame rather than as
   a field: `FrameKind::ReplayEnd`, empty payload, sent exactly once per attach after the last
   replayed byte and before anything live. A byte count on `StreamAttached` was the smaller diff
   and was rejected — this wire has already shipped a defect where the credit window was
   specified in payload bytes and one end charged encoded ones, which every unit test on both
   sides passed and only a real-daemon interop test caught. A count is a number two
   implementations must keep deriving identically; a marker is a byte that either arrived or did
   not. A new kind is breaking in one direction, because a decoder that meets one it does not
   know drops the connection by design, so `PROTOCOL_VERSION` moved to 2 and the attachable
   range narrowed with it: a v0.1 daemon's sessions are orphaned by a v0.2 window rather than
   corrupted by one.

   The client half is **not** where the obvious reading puts it, and this is the part worth
   writing down. The daemon's reader sees the boundary and hands it to the webview milliseconds
   before xterm has *parsed* the bytes it follows — xterm parses on its own timer to keep the
   main thread responsive. So a gate in the Rust client that opened when the marker arrived
   would already be open when the reply appeared, and would have shipped as a non-fix that
   passed a Rust interop test. The gate lives in `XtermSurface` and opens on the write
   *callback* for the last chunk issued before the marker, which is the only signal that side
   gets which says parsed rather than queued. Keystrokes during that window are dropped rather
   than queued — the pane is painting history and is not interactive yet, and one held and
   delivered later lands at a prompt that has moved on. If the marker never arrives a five-
   second deadline opens the gate anyway and raises a notice, because a pane that silently
   ignored the first seconds of typing is indistinguishable from a broken keyboard.

   Proved at the three layers that can each see one part, because no single layer can see them
   all — the input that corrupted the shell was never written by any code in this repository, so
   no Rust test can reproduce it. `crates/nysia-core/src/rpc/interop.rs` and
   `apps/desktop/src-tauri/src/interop.rs` pin the wire contract against a real daemon on a real
   socket; `apps/web/src/transport/surface/surface.test.ts` pins the gate against a terminal
   stub that answers queries in the order a real one does.

   **And then the rationale for it turned out to be half wrong, which is the part worth
   keeping.** This entry used to close by saying both rejected workarounds stayed rejected, and
   to give as the reason that *a live query after the boundary is still answered, so full-screen
   programs are unaffected* — a property stripping query sequences would have broken. Nothing
   was being protected. **The daemon answers every query itself**: its virtual terminal is built
   with a reply sink and `nysia_core::rpc::session::spawn_pump` calls `take_replies` on every
   chunk it feeds and writes the result to the pty, precisely so that `pwsh`, which will not
   draw its prompt until `CSI 6 n` comes back, draws one. So by the time the bytes reach the
   window the authority has already answered, and the renderer's answer was a duplicate —
   unsolicited, every single time, on both sides of the boundary.

   It also explains what the original report noticed and could not account for: that the
   corruption was "harmless on a first attach". ConPTY's own startup probe gets the authority's
   reply and then the renderer's, and the second arrives as F3 with no history to recall. It was
   never harmless. It was invisible.

   **Closed properly by a second change (#43): the display cache answers nothing.**
   `apps/web/src/transport/surface/muteReplies.ts` registers a handler for every sequence xterm
   would answer, ahead of the built-in responder, so no reply is ever composed. D-7 is the frame
   — terminal state lives in Rust, the webview is a display cache, and a display cache does not
   answer on the authority's behalf. Recognising replies on the way *out* was rejected again and
   for the same reason it was rejected the first time, now with the better version available:
   `ESC[5;3R` is a reply and also something a person can type, and a reply that is never composed
   needs no recognising.

   The muted set is every path by which parsing output reaches xterm's `triggerDataEvent`: DA1,
   DA2, DSR, DECDSR, XTVERSION, DECRQM in both forms, the kitty keyboard query, DECRQSS, the
   three `CSI t` options that report, and an `OSC 4/10/11/12` whose value is `?`. Sequences that
   also *act* are read by parameter, so a palette set still runs and a title push is left to the
   built-in. No ESC handler is registered: xterm has no ESC-level responder to displace, and
   claiming a guarantee about a handler that does not exist would be worse than the gap.

   **Two of those are muted for safety rather than because they answer**, which is worth saying
   because the difference is one option either way. `kittyKeyboardQuery` returns without writing
   unless `vtExtensions.kittyKeyboard` is set, and `CSI t`'s reports are refused by
   `paramToWindowOption` against `windowOptions` — xterm defaults both to `{}` and `surface/xterm.ts`
   sets neither, so neither reaches `triggerDataEvent` as the app is configured today. They stay in
   the table: a default an ordinary caller can undo with one option is a suggestion (CLAUDE.md §6),
   and the `CSI t` split is the shape the app would need the moment a window option is turned on.

   **The ordering it rests on was read out of xterm's source, not its documentation** — the same
   standard the boundary itself was held to, and the reason that one held up. In
   `@xterm/xterm` 6.1.0-beta.304: registration *pushes* onto the per-identifier list, both CSI
   dispatch sites walk it backwards and break on the first `true`, `OscParser.end` and
   `DcsParser.unhook` do the same, `OscHandler.end(false)` and `DcsHandler.unhook(false)` do not
   call their callback, and `EscapeSequenceParser.reset()` clears parser *state* rather than the
   handler lists — which matters, because this repository writes `ESC c` itself after a hidden
   pane's buffer overflows, and a mute that died there would come back only on the panes that
   had already lost output.

   **The replay boundary stays, as defence in depth, and the reasons are not sentiment.** Two
   things the mute cannot do. Real keystrokes typed at a pane that is still painting history are
   still dropped, which is the gate's own claim and nothing to do with queries. And the gate is
   the layer that does not need a table to be complete: the mute is an enumerated list, applied from
   `surface/xterm.ts` — the one module the node-only tests cannot exercise against a real terminal
   (D-18), which is why the line that applies it is pinned by a lint-meta rule of its own — while
   the gate takes *everything* `onData` produces during a replay whatever produced it. An xterm bump that adds a responder, or moves the dispatch order the mute is read against,
   lands on a closed channel rather than in the shell. Different failure, different layer. The
   deadline is still a backstop rather than the mechanism, which is what separates it from the
   time-based window that was rejected.

   **What now goes unanswered, said plainly.** The daemon answers DA1, DA2, DSR 5/6, DECRQM and
   `CSI 18 t` — `alacritty_terminal`'s `identify_terminal`, `device_status`, `report_mode` and
   `text_area_size_chars`, all of which reach the pty through the reply sink. It does **not**
   answer XTVERSION, DECRQSS, `CSI 14/16 t` or an OSC colour query: alacritty either has no
   handler or raises an event (`TextAreaSizeRequest`, `ColorRequest`) that the sink deliberately
   drops. Those four are now answered by nobody. The kitty keyboard query belongs in neither
   list — `alacritty_terminal`'s `report_keyboard_mode` returns early unless `kitty_keyboard` is
   on and 0.26 defaults it to `false`, so as both sides are configured nobody answered it before
   this change either. That is the right
   direction under D-7 — the window's palette, cell size and renderer version are not the
   session's, and an answer from the cache would have been wrong as often as it was redundant —
   but it is a daemon gap rather than a non-issue, and it is recorded here so the next program
   that waits on one has somewhere to look.

   **And one thing the window still writes to the child, which no parser hook can stop.**
   `DECSET 2031` asks a terminal to *notify* the program when the colour scheme changes, and
   xterm emits `CSI ?997;Nn` from its theme-change listener rather than from parsing anything —
   `CoreBrowserTerminal` registers it on `ThemeService.onChangeColors`. There is no handler to
   displace, and Nysia has a runtime theme switcher (§8), so the path is live: a program that set
   2031 is typed at the next time the user changes theme. It is left as it is, and named here
   rather than glossed over. It is the one item in this entry the authority does not hold — the
   daemon has no theme — the program opted in rather than the cache volunteering, and suppressing
   it means intercepting `CSI ? h` and dropping one mode out of a list that may carry several the
   program does want. Worth doing if a shell ever sets 2031 and the stray line matters; not worth
   folding into the change that closed the rest.

   **The end-to-end step was a proxy too (#44), and is not one now.** It snapshotted the screen,
   relaunched, waited for idle and asserted the screen was unchanged — which catches phantom
   input only if it *echoes*, and the defect did not echo: F3 put a recalled line into the editor
   without submitting it. `scripts/e2e/walking-skeleton.ps1` now does what the original
   reproduction did. It types a command after the attach and asserts a whole screen line equal to
   a token the shell computed — a whole line, never a substring, because the concatenation the
   report describes still *contains* the token in its output — and then repeats it with two
   characters left waiting in the editor and asserts the same check fails. One thing it
   deliberately does not assert is the precondition that the replayed scrollback contained a
   query: the replay ring is raw bytes and `terminal read` hands back text with the escapes
   already stripped, so nothing a script can call can see one. The step stops depending on it
   instead, and the control is what makes a pass mean something.

   **One correction to the record.** The commit that fixed a race in the boundary interop test
   diagnosed it as *a session nothing is watching does not fill its replay ring*. The pump says
   otherwise: `state.feed(&chunk)` runs on every chunk it reads, and the ring is filled inside
   it, unconditionally. What is discarded when no sink is attached is the *coalescing* buffer —
   `flush_all` clears it and says why, because the grid already has the bytes. The test change
   was right for a different reason: attaching a first window and draining it is a
   synchronisation, and what it waits for is the pump having fed the state at all.

---

## 13. Repo layout

```
nysia/
  crates/
    nysia/          bin: daemon + CLI (D-11)
    nysia-core/     PTY, VT state, store, git, worktree, orchestration
    nysia-proto/    wire types; ts-rs export lives here
  apps/
    desktop/        Tauri GUI (src-tauri + thin Rust shell)
    web/            React 19 + Vite + Tailwind 4
  tools/
    lint-meta/      architecture rules (D-14)
  docs/
```

Cargo workspace + pnpm workspace side by side. `nysia-core` is the library both the daemon
and the Tauri shell link; the GUI's Rust side stays thin because it is a client, not an owner.

Gates for v1 (D-14): `cargo clippy -D warnings`, `cargo fmt --check`, `tsc`, `vitest`,
the ts-rs drift guard with its prove-it-trips script, the pre-push ⇄ CI parity manifest,
plus lint-meta's `layer-rank`, `rust-module-shape` and the file-size ratchet.
