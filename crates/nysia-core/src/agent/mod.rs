//! Agent sessions: starting the CLI, and keeping its status hooks installed.
//!
//! # The D-4 boundary
//!
//! Claude is the only agent in v1 and there is **no provider trait** (D-3, D-4). All of
//! Claude's specifics live in one module — [`claude`] — and nothing outside `agent/` may
//! import them.
//!
//! §7.4 gives the reason, and it is worth reading rather than skipping: a trait extracted
//! from one implementation encodes that implementation's assumptions and calls them
//! universal. nightcore is the evidence. Its seam was designed against one provider, and
//! when a second arrived `StartSessionParams` fields were silently dropped, scans hardcoded
//! an autonomy level the new provider refuses, and every scan of the new kind failed on first
//! run *from the UI*. The trait gets extracted from two real implementations when Codex lands
//! in v0.6+, not from one now.
//!
//! The boundary is enforced twice over, because each half catches what the other cannot:
//!
//! - **Rust privacy is the load-bearing half.** `mod claude` below carries no visibility
//!   modifier, so `crate::agent::claude` cannot be named from anywhere else in the crate and
//!   nothing outside it can be imported at all. Code that breaks this does not compile.
//! - **`no-claude-specifics-outside-agent` in `tools/lint-meta` is the half that reports a
//!   file and a line**, the same way rule (a) does next to the cargo rules. It also covers
//!   the case privacy *permits* and which is how a boundary quietly stops being one: a
//!   `pub use` inside `agent/` that hands a Claude item on under a neutral name, so that
//!   callers get Claude's shape while the word `claude` vanishes from the path. That rule
//!   ships with a fixture proving it trips, and `pnpm prove:lint-meta` runs it.
//!
//! # How the two halves are arranged
//!
//! Types flow **up**, behaviour stays **down**. [`AgentLaunch`], [`LaunchError`] and
//! [`hooks::HookChange`] are declared here in the neutral half; [`claude`] imports them and
//! returns them. Nothing has to be re-exported out of [`claude`], which is what keeps the
//! laundering rule above from having anything to report in the first place.
//!
//! # What this module does not do
//!
//! It does not spawn a pty. [`launch`] resolves and pre-validates the CLI and hands back an
//! [`AgentLaunch`]; hosting one in a session is [`crate::rpc::SessionRegistry::create`]'s,
//! through [`crate::pty::SessionSpec::for_program`] — which takes an argv and a label and is
//! neutral by construction, so the pty layer spawns what it is handed without learning what
//! it is. It does not implement `nysia hook` either — it installs entries that point at it.

pub mod hooks;

mod claude;
mod launch;
mod ordered_json;

#[cfg(test)]
mod scratch;

pub use launch::{AgentLaunch, LaunchError, launch};
