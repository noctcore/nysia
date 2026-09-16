# Changelog

Notable changes per release. Newest first.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), with one
addition that matters more here than the categories do: every entry says what was
**deliberately not** built, because a scope ladder only means something if each rung is
honest about where it stops.

## [Unreleased]

## [0.3.0] — 2026-09-16

**Projects become real, and tasks arrive.** One acceptance criterion, and it holds: *point
Nysia at a folder on disk and it appears in the sidebar, survives a daemon restart, and a
GitHub issue in it can be started into a branch-keyed worktree with a session and a tab.*
`crates/nysia/tests/projects.rs` landed written to fail, naming the wave that would land the
verbs, and passes now with no assertion, no timeout and, at the unignore, no step of the
harness moved. It is also the release where an agent session first exists — the one thing
0.2.0's ladder row promised and did not ship, though not yet the dot that row was for; see
*Known defects*.

### Added

- **A project is a registered folder, and the daemon has heard of one.** `nysia-proto` carries
  the project and worktree types, the `project_register` / `project_list` / `project_forget`
  verbs, and an error envelope that tells the cases apart **by code** rather than by a message
  a caller would have to parse. The id is derived from the canonical path — symlinks resolved,
  Windows case and 8.3 names normalised, what is *not* normalised written down and held by
  tests — so registering the same folder twice is one project, and the sidebar does not reorder
  itself after a restart.
- **One hardened chokepoint for every git spawn** (§7.5). The folder is the child's working
  directory rather than an argument, so git's option parser never sees a path; `git worktree
  add`'s one positional goes behind `--`; `CanonicalPath` is a type that exists only for a path
  that resolved to a directory, and is what every spawn takes as its cwd; and every invocation
  carries a deadline and kills its tree on expiry — a Job Object on Windows (trap 7), the
  process group on Unix — with "git exited" kept separate from "the pipe reached EOF"
  (trap 11). The environment is neutralised rather than tidied: `GIT_TERMINAL_PROMPT=0` and
  `GCM_INTERACTIVE=never` because a prompt hangs invisibly, §7.5's config neutralisers because
  `core.fsmonitor` names a program git runs and lives in a file an agent can write, and a
  `GIT_*` scrub because with `GIT_DIR` set `rev-parse` in a folder that is no repository exits
  0 and answers for a different one. `env_remove`, never `env_clear`, so credential helpers
  keep working.
- **What a folder is.** `git::inspect` answers a repository, a folder *of* repositories — it
  reports the list and registers nothing, because choosing between all, none and asking belongs
  to the verb — a folder that is no repository, a file, a path that is not there, and a folder
  git will not open (dubious ownership, a `.git` pointing nowhere) carrying git's own stderr.
  Decided from the filesystem rather than by reading git's wording, which would break the first
  time git reworded one.
- **The projects table** (D-12) — a migration on the runner 0.2.0 built, not new infrastructure.
  A v0.2 database reaches the v0.3 schema **with its rows**, which is a test rather than a hope.
- **`Start →`, keyed by branch and never by task id** (D-6). It creates the worktree for the
  branch an issue derives, or adopts the one already on that branch, then a session and a tab.
  Worktrees live under `.nysia/worktrees/` with a `.gitignore` of `*` beside them, so `git add
  -A` in the project cannot stage one as an embedded repository — proved by `git status
  --porcelain` being empty after a start, not by the file existing.
- **The Tasks screen: GitHub Issues, queried live** (D-5, no local task domain model). `gh` runs
  behind the same chokepoint under a policy of its own; `tasks_list` is on the wire and `nysia
  tasks list` on the CLI. The screen has **four endings that read differently** — issues, a
  repository with none, a query that could not run, and a `gh` that is missing or signed out —
  rather than one empty state standing in for four situations.
- **The sidebar reads real projects, and `+` is live.** *Add a project* is browse-a-folder, and
  the picker is a command the webview cannot call for itself. A list that could not be refreshed
  says it is stale rather than showing names that may no longer be true; a project git cannot
  describe is listed with no worktrees rather than dropped, so an unplugged drive shows a
  project that cannot be opened instead of emptying the sidebar.
- **An agent session, served** (D-3/D-4). `SessionRegistry::create` no longer refuses
  `SessionKind::Agent`: the daemon resolves the CLI **before** it creates anything (trap 8),
  hosts it in a PTY it owns, and tears the whole process tree down on close. The window's
  `Start →` sends `kind: agent`; `nysia project start --kind agent` and `nysia session create
  --kind agent` are the CLI's half of the same path.

### Fixed

- **A killed daemon no longer blocks its own restart on Unix.** A socket file left behind by a
  process that never unlinked it was read as an endpoint in use, so the daemon that replaced it
  could not bind. Liveness is decided by reaching the socket, never by the file existing.
- **A daemon that could not serve no longer reports success.** The fix above stops a stale
  socket producing `AlreadyBound` at all; it left the mapping behind it alone, and `bind_unix`
  still answers `AlreadyBound` for a regular file, a symlink, a liveness probe that failed with
  anything other than `ConnectionRefused`, and a `remove_file` that was refused. Each of those
  became *already running* and a success exit — which is the one thing a supervisor must not be
  told wrongly, because it waits for a daemon nobody is going to start. `AlreadyBound` is the
  transport's reading of a failed `bind` and not a claim about the world, so the claim is now
  checked by dialling: anything that completes a connection is a daemon, including one too new
  to answer this build's handshake, since D-11 lets the window and the daemon run different
  versions. Only a transport failure is nobody, and that exits non-zero saying what to do.
- **An endpoint whose every pipe instance is taken is no longer read as an empty one.** A dial
  re-opens a busy pipe on a bounded schedule — a race, not a queue, since the window is one
  `CreateNamedPipeW` long — and busy is now a third answer rather than *nothing is listening*,
  which is what keeps a window from starting a second daemon beside a saturated one.
- **`nysia --daemon` cannot block for ever confirming another daemon.** The handshake dial has a
  deadline, and silence gets an outcome of its own: neither *already running*, which would tell
  a supervisor something is serving there, nor *not listening*, which would send somebody to
  delete what is at that path.
- **A refusal that could not create a session no longer leaves the repository changed.** An
  agent start whose CLI will not launch is refused before it makes a branch, a worktree or a
  `.nysia` directory, and a test drives that with a `PATH` holding a deliberately unlaunchable
  `claude`.

### Security

- **A refusal no longer names a file on the machine it was refused on.** Three session-error
  variants wrapped an error whose `Display` spells a path, and `portable-pty` formats the whole
  command line *and the working directory* into the error a failed `CreateProcessW` answers
  with — and a session's working directory is a worktree inside somebody's project. Messages
  are built per variant from phrases chosen in the file; the wrapped error goes to the daemon's
  own log at `warn`. The same split covers a project refusal that carried the daemon's database
  path, and `portable_pty` is held down in the log for the same reason.
- **A path confinement that held on one platform now holds on both.** The refusal that lists
  the repositories found inside a folder cut each name with `Path::file_name`, which splits on
  the separators of the platform it was compiled for — so on Unix a backslash is an ordinary
  character and `C:\Users\someone\Projekty\nysia` came back whole. The macOS leg caught it, on
  the test written to prove the confinement holds. Cutting at `/` and `\` on every platform is
  the fix, with a name that reduces to nothing dropped rather than listed as an empty string;
  the test carries both spellings, a trailing separator and a name that is only separators, so
  the next platform-dependent split reds both runners. The cost is recorded: a Unix folder
  genuinely named `my\dir` is written down as `dir`.
- **`NYSIA_LOG` cannot be spelled so as to undo the log's confinement.** Folding the confined
  targets in after the user's filter stopped `NYSIA_LOG=debug`, but not a directive naming a
  module: `EnvFilter` resolves a callsite against its longest matching target, so
  `vte::ansi=trace` out-specifies `vte=off` and puts every unhandled OSC parameter in the file.
  Directives are now screened — a target beginning with a confined crate's name, and anything
  carrying a `[`, which enables every target however the list is spelled — in the core, in the
  daemon and in the window, each with its own proof. The deliberate override is
  `NYSIA_LOG_UNCONFINED`, named so the call site is obvious and greppable (§6).
- **Another repository's text cannot drive your terminal.** An issue's title, author and labels
  went to a terminal with `println!` and nothing between them, while the daemon was already
  scrubbing `GH_FORCE_TTY` and `CLICOLOR_FORCE` from gh's environment for exactly that reason.
  Unicode `Cc` — C0, DEL and C1 in one definition — is replaced with U+FFFD, which covers an
  erase-display, a cursor jump, an OSC retitling the window, the single-byte C1 introducer, and
  a newline that would otherwise let a title forge a row of its own in the table. gh's stderr
  stays out of the log as well as out of the envelope.

### Deliberately not in this release

Not missing — not built, and each has a rung on the ladder or a line in the plan:

- **The worktree manager.** Merge, diff and discard are v0.4. v0.3 *creates* a worktree from an
  issue; it does not manage the ones that exist. The **usage meter** is v0.4 with it.
- **Clone-from-URL and create-new-folder.** Orca's dialog offers three ways into a project.
  Browse a folder you already have is the one that unblocks everything else; the other two are
  follow-ons, and the reason is recorded rather than implied.
- **Remote hosts.** Orca's dialog has a *Host* selector. Nysia is local-only until the relay
  lands in v0.6+, and a selector with one option and a future is worse than no selector.
- **Shell command-state.** v0.2's plan deferred the OSC 133 rc-wrapper to v0.3, and v0.3 did not
  build it either. The VT has *intercepted* OSC 133 since v0.1; nothing injects the `ZDOTDIR` /
  `--rcfile` / pwsh-profile-argument wrapper that would make a shell emit it, so a shell tab
  still reports no command state. Unclaimed now rather than scheduled.
- **Settings → Tasks**, which the design spec draws. General, Appearance and Agents are built.
- **Scrollback persistence.** SQLite holds agent status and registered projects and not one byte
  of terminal output, so §12 question 2's answer is still *none* and a daemon that stops takes
  every session's scrollback with it.
- **OS-level confinement.** Lexical path gates only, still (§7.5, §12 q1).

### Known defects

- **Nysia still does not install the hooks its dots depend on.** This release closed the launch
  half of v0.2's ladder row and not the installer half: `nysia_core::agent::hooks::install` and
  `uninstall` are `pub` and tested and called from **no** production path and no CLI verb — the
  only caller in the tree is the live-drive test, which writes `.claude/settings.json` into the
  worktree by hand before it starts the agent. An agent session started from the window runs,
  and its dot does not move until somebody installs those hooks themselves. A comment in
  `rpc/project.rs` names `nysia agent hooks install`; there is no such verb.
- **What an agent session costs is still unmeasured** (§12 question 3). v0.2's plan owed the row
  and could not have produced it, because nothing in v0.2 could start an agent to measure.
  0.3.0 can, and did not: `scripts/e2e/measure-memory.ps1` is unchanged across both releases and
  the question is still marked open.

## [0.2.0] — 2026-09-15

**The live status dots.** One acceptance criterion, and it held for the pipeline but not for
the thing the pipeline is for: *a Claude agent session started from Nysia drives its own dot —
working, waiting, done — in the sidebar and on its tab, through hooks, with no terminal-title
guessing anywhere; and killing the window never loses a status the daemon already recorded.*
The hook, the daemon ingest, the persistence and the three dots are all built, and
`crates/nysia/tests/agent_status.rs` proves them end to end — driving them **from a shell
session**, because `nysia session create` had no `--kind agent` and nothing in this release
could start an agent at all. See *Known defects*: the first clause of that sentence is the one
that did not ship, and it lands in 0.3.0.

### Added

- **The agent status contract on the wire** (D-13). `AgentState` with exactly four states and
  the mapping from Claude's hook events onto them; the per-pane row carrying `question`,
  `is_interrupt`, `session_boundary`, `agent_id`, `observed_at` and `restored_unconfirmed`; an
  `agent_status` frame kind added **without a protocol bump**; and the RPC verbs to hook, read,
  list, subscribe and unsubscribe. `PreCompact` is deliberately unmapped and a test says so
  rather than a comment. `nysia-proto` owns the type and nobody defines a second.
- **The Claude module, behind a boundary that reports** (D-3/D-4). `nysia_core::agent` declares
  the neutral types and `agent::claude` imports them *upward*; `mod claude` carries no
  visibility modifier, so Rust privacy is the load-bearing half and the import from outside does
  not compile — and lint-meta's `no-claude-specifics-outside-agent` is the half that reports a
  file and a line, with fixtures that trip it wired into `pnpm prove:lint-meta`. Resolving the
  CLI is a `which`-style lookup with the path pre-validated, because `Command::new("claude")`
  answers NotFound against the `.cmd` shim npm installs (trap 8). Installing hooks preserves a
  settings file's line endings and refuses settings this code would have to overwrite.
- **`nysia hook`, and the status verbs served.** The hook prints `{}` and **flushes** it as its
  first statement — before stdin is read, before an endpoint is resolved, before anything below
  it can fail — so it can never block or influence the agent; stdout is block-buffered when it
  is a pipe, which is what Claude gives a hook, so the flush is half the property. Pane identity
  is proven from socket peer credentials and PTY process-tree ancestry, with `NYSIA_PANE_KEY` a
  hint for speed and never the proof, so a `nysia hook` spawned outside any session's process
  tree cannot write status into a pane. With it: daemon ingest, the per-pane JSONL spool and its
  drain on start, the thirty-minute staleness decay, and `nysia agent status`.
- **SQLite in WAL, for real** (D-12). `nysia_core::store` was a ten-line doc comment stating the
  decision's intent; it is now a migration runner and the status table. The version lives in
  `user_version` rather than a table, because it commits transactionally with the schema it
  describes, and every step runs inside `BEGIN IMMEDIATE` and re-reads the version under the
  write lock, so two daemons opening one fresh database do not both create the table. WAL is
  verified rather than requested — `PRAGMA journal_mode` is read back — and a database path that
  is a symbolic link is refused.
- **The dots, and the notices.** Sidebar per session, tab strip per tab, and in-window
  notifications on the transitions that deserve one. The wire's four states and the design
  spec's §1 status palette are *not the same list*, so the join is a table with a reason on
  every row — and a pane with **no** row takes the accent, because nothing is known about it and
  grey would be a claim. A stale `working` decays to the same token at half weight rather than
  to a fifth colour, since staleness is not a fifth state. Status colours stay independent of
  the accent, and the accent picker does not recolour them. A `SessionStart` boundary raises no
  notification, and neither does a row rehydrated from the spool — a status recovered from disk
  on daemon start is not an event that just happened.
- **Settings → Agents**, the design spec's Screen 2c as its Claude slice: the client preference
  rows, the default-agent chips, and *Installed* with one detected row and a count derived from
  the list it labels. The artboard draws nine chips; D-3 makes one of them real, so the other
  seven are **absent, not disabled** — six agents rendered when one works is a screen describing
  a product that does not exist.
- **A mark for every settings nav entry and every shell**, so the nav and the `+` menu tell four
  shells apart by glyph rather than by name alone. The *Voice* entry is gone, settled by D-10.
- **Logs a support conversation can use.** The daemon's log is capped at 8 MiB per file with 3
  rotated copies kept — measured on a tick rather than on every write, so the ceiling is the
  figure that holds when the trim keeps up and the module says that rather than claiming a bound
  it cannot make. The live file is **truncated, never renamed**: the daemon does not open its
  own log, its stdout is an inherited handle, and a handle names the file rather than the path,
  so a rename succeeds and leaves the daemon writing into the rotated copy for the rest of its
  life. The window gets a log beside it, recording which verb was in flight and never its
  payload, plus the three transport events only the webview can see.
- **`pnpm dev` builds the runtime before `tauri dev` opens the window**, and only when the dev
  profile has none.

### Changed — **breaking**

- **The socket protocol is v2, and v1 is no longer served.** The endpoint carries the version
  (§3.1), so a v0.2 window dials `nysiad-v2` and a v0.1 daemon goes on serving `nysiad-v1`
  beside it: nothing is corrupted, but **sessions held by a running v0.1 daemon are not adopted
  by a v0.2 window**. Close them, or leave the old daemon running and reach it with the old
  CLI. The narrowing is deliberate rather than incidental — v2 adds a frame kind, and a v1
  decoder treats a kind it does not know as fatal, so serving a v1 client would drop every
  session on its stream connection the first time anything attached.

### Fixed

- **Re-attaching no longer injects input into the shell** (§12 q7). A replay is the bytes the
  child once wrote, escape sequences intact, so it carries every `ESC[6n` and `ESC[c` it ever
  emitted; a terminal emulator answers a query when it parses one and cannot tell a replayed one
  from a live one, and its answers left as keystrokes. The daemon now marks where the replay
  stops — one empty `replay_end` frame per attach — and the window holds its input channel shut
  until the renderer has finished *parsing* everything before that marker. Keystrokes typed in
  that window are dropped rather than queued: the pane is painting history and is not
  interactive yet, and one delivered late lands at a prompt that has moved on. A live query
  after the boundary is still answered, so full-screen programs are unaffected.
- **A first launch works.** The window starts the daemon it ships with, so an app launched on a
  machine with nothing listening reaches a working tab instead of *Reconnecting* (§12 q5/q6).
  Two halves: `nysia_core::rpc::ensure_daemon` is a synchronous seam over the *existing* spawn
  lock, re-probe, readiness wait and lease check — the window supplies its own blocking dial
  and `nysia <verb>` keeps its async one, so two front ends share one implementation of the
  race rather than two answers to it — and the app now ships the `nysia` binary as a sidecar,
  with a CI job that builds the bundle on both runners and looks inside what a user receives:
  the macOS `.app`, and on Windows the installer run to a prefix of its own.
- **A window that cannot start a daemon says why, once.** A missing sidecar, a runtime that
  will not execute and one that starts without binding are each a non-retryable failure
  carrying the path that was tried or the log that explains it — and the reconnect loop
  **ends** on one instead of re-taking the spawn lock and starting a process every twenty
  seconds for the life of the window.
- **A busy or refusing endpoint is no longer read as an empty one.** The window classifies dial
  failures the way `nysia_core::rpc::transport` does, so only `NotFound` — and, on Unix, a
  socket that refuses — leads it to start a daemon.

### Deliberately not in this release

Not missing — not built:

- **No provider trait** (D-3/D-4). Claude is the only agent, and the trait gets extracted from
  two real implementations when Codex lands in v0.6+, not designed against one now. A seam
  designed against one implementation is what dropped `StartSessionParams` fields silently in
  nightcore the day a second arrived.
- **No second agent in the Settings screen.** Seven of the artboard's nine chips are absent with
  a comment naming v0.6 — not disabled, not greyed, absent.
- **No shell command-state.** §5.3 lists OSC 133 under status detection, but this rung is
  *agent* status; a shell's command state is an rc-wrapper injection with its own trap surface
  (`ZDOTDIR`, `--rcfile`, the pwsh profile argument), and it is deferred to v0.3.
- **No Tasks screen, no worktree manager, no usage metering.** v0.3 and v0.4.
- **No scrollback persistence.** The store is real now and holds agent status; it holds no
  terminal output, so §12 question 2's answer is still *none* and a daemon that stops still
  takes every session's scrollback with it.
- **OS-level confinement.** Lexical path gates only (§7.5, §12 q1).

### Known defects

Reported rather than papered over, in the release that shipped them:

- **Nothing in this release can start an agent session.** The ladder row is *Claude agent
  sessions · hook install/uninstall · status → sidebar + tabs · notifications*, and the status
  pipeline, the dots and the notifications all shipped. The sessions did not:
  `SessionRegistry::create` refused `SessionKind::Agent` outright, with the next step *"v0.1
  serves shell sessions; agent sessions land in v0.2"* — a sentence that was already wrong on
  the day it shipped in v0.2 — and `nysia_core::agent::launch` is `pub`, tested, and called from
  nowhere in the tree. `agent::hooks::install` and `uninstall` are the same shape: a library
  with no verb behind it and no caller. So the window drew agent tabs and status dots for a
  session that could not be created, and the acceptance test drives the status path from a shell
  — which its own doc comment states and argues, rather than leaving it to be discovered.
  **The session half is fixed in 0.3.0**; the installer half is still open there.
- **What an agent session costs was not measured** (§12 question 3). The plan owed the row and
  it could not have been produced: there was no agent session to measure.
  `scripts/e2e/measure-memory.ps1` is unchanged in this release and the question is still marked
  open.

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

Found by driving the real GUI, reported rather than papered over, with their evidence in §12:

- **The window cannot start a daemon, and no build of the app ships one** (§12 q6). On a machine
  with nothing listening, the app launches into *Reconnecting* and the `+` menu fails with
  "Start the Nysia daemon, then try again". Start one first; the README says how.
  **Fixed in 0.2.0** — see that entry above. Left standing here because it is what 0.1.0
  shipped, and a changelog that edits a defect out of a released version is not a record.
- **Re-attaching injects input into the shell** (§12 q7). The replay contains the terminal
  queries the child once wrote, xterm answers them, and ConPTY reads a cursor-position report as
  F3 — so after a relaunch the pane shows a phantom command line and the next thing you type is
  concatenated onto it. `Esc` clears it. **Fixed in 0.2.0**, and left standing here because
  it is what 0.1.0 does.


[0.3.0]: https://github.com/noctcore/nysia/releases/tag/v0.3.0
[0.2.0]: https://github.com/noctcore/nysia/releases/tag/v0.2.0
[0.1.0]: https://github.com/noctcore/nysia/releases/tag/v0.1.0
