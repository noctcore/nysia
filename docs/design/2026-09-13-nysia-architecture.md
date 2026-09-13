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

---

## 3. Process model

```
nysiad  ─ long-lived, detached, survives every UI restart and upgrade
│
├── PTY layer          portable-pty; one blocking reader thread per session
├── VT layer           alacritty_terminal grid + raw replay ring, per session
├── Store              SQLite (WAL): sessions, projects, worktrees,
│                      orchestration, agent status, scrollback index
├── Hook endpoint      localhost HTTP; receives Claude Code hook payloads
├── Git / worktree     all git spawns funnelled through one hardened chokepoint
└── RPC server         versioned NDJSON over named pipe (Windows) /
                       Unix socket (macOS), token + peer-credential auth
        ▲                    ▲                       ▲
        │                    │                       │
   Nysia.app            nysia CLI              (future) relay
   (Tauri window)   (what agents call)         (mobile approvals)
```

Every consumer is a client of the same socket and the same verb surface. The window has no privileged path.

### 3.1 Socket protocol

Lifted wholesale from Orca's daemon, which has earned it:

- **Versioned socket name** — `nysiad-v1.sock` / `\\.\pipe\nysiad-v1-<user>`. A new client refuses to attach to an incompatible daemon instead of corrupting it. Orca is on `daemon-v36` and still advertises `attachableDaemonProtocolVersions: [1..36]`, so a newer app adopts an older daemon rather than orphaning its sessions. Copy that.
- **Token file** beside the socket, mode 0600 / owner-only ACL.
- **Handshake first frame**: `{type:"hello", version, token, role:"control"|"stream", clientId}` → `{ok, daemonIdentity:{pid, startedAtMs, launchNonce, appVersion}}`. Reject with `retryable` set so clients know whether to back off or die.
- **Framing**: newline-delimited JSON for control; length-prefixed binary (`[kind:u8][len:u32 BE][payload]`) for terminal output.
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

Same hooks, same event mapping, but POST to **the daemon's** endpoint. Then:

- the endpoint survives UI restarts, so agents never lose status
- authority is proven by peer credentials + process tree (§3.2), not by token echo
- status is one more table in the daemon's SQLite, readable by the window, the CLI, and later the phone through the identical RPC

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
| **v0.3** | GitHub Issues view · `Start →` creates branch-keyed worktree + session + tab | The first thing that is not a terminal. |
| **v0.4** | Worktree manager · merge/diff/discard · usage meter (statusline shim) | |
| **v0.5** | Orchestration verb surface · `nysia agent-context --json` · worker preamble · `land` | The differentiator. |
| **v0.6+** | Second provider (Codex) → extract the trait · browser tab · relay + mobile approvals | |

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

1. **Does the daemon own git, or does the UI?** §4 says the daemon. Cost: `git.exec` is Orca's hot path — 2,557 spans per 3,000 trace events over 10 days. If git lives in the daemon, its latency is on the RPC path.
2. **Windows confinement.** Lexical gates only in v1 (§7.5). Is there an acceptable OS-level story later — AppContainer, a restricted token, or WSL-only agent sessions?
3. **Scrollback persistence budget.** Orca: 5k rows default, 512 KiB replay, 5 MiB store, up to 200 MB history checkpoints. What is Nysia's ceiling, and does it survive a daemon crash or only a clean restart?
4. **Does the `nysia` CLI ship inside the app bundle or install separately?** Orca re-invokes its own Electron binary with `ELECTRON_RUN_AS_NODE=1`. A Rust CLI could be a second binary in the same bundle, or the same binary behind a flag — nightcore already does the latter for its terminal daemon.
5. **Memory expectations.** Orca on this machine: app 1.1 GB, 356–817 MB *per worktree*, PTY daemon 88 MB for 5 terminals. The agent processes are the entire cost. Tauri saves the Electron renderer, not the expensive part — so what is the honest performance claim?
