//! The Nysia runtime.
//!
//! Everything stateful lives behind this crate: the PTYs, the terminal state, the store,
//! git, worktrees and the socket server. The daemon (`nysia --daemon`) is a thin argv
//! wrapper around it, and the Tauri shell links it only so that a client-side helper does
//! not have to be written twice — the window has no privileged path into it (D-1, D-2).
//!
//! There is deliberately **no god object**. Each module owns its own state and its own
//! error type; the daemon composes them. A `NysiaCore` struct holding six `Arc<Mutex<_>>`
//! fields is the shape this crate exists to avoid.
//!
//! # Architectural rule
//!
//! This crate must never depend on `tauri`, directly or transitively through a Nysia
//! crate. The runtime outliving the UI is the founding constraint (D-1); a runtime that
//! links the UI toolkit cannot honour it. `tools/lint-meta` enforces this, and ships a
//! fixture proving the rule trips.
//!
//! # Status
//!
//! Wave 0 scaffolding. Every module below is a documented placeholder that states what it
//! will own and which wave fills it in. Nothing here reads a PTY yet.

pub mod agent;
pub mod git;
pub mod pty;
pub mod rpc;
pub mod store;
pub mod vt;
pub mod worktree;
