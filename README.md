# Nysia

A terminal-first agentic development environment. Tauri v2 + Rust.

Every tab is a session; a session is either an agent or a shell. Projects and their
worktrees are the spine. Tasks come from GitHub Issues.

**Headless core, detachable UI** — a long-lived `nysiad` daemon owns the PTYs, the store,
the orchestration state and the agent-status endpoint. The window is a client. Updating or
crashing the UI never interrupts a running agent.

Status: design. Nothing is built yet.

- [Architecture and founding decisions](docs/design/2026-09-13-nysia-architecture.md)
- [Design system spec](docs/design/design-spec.md)
