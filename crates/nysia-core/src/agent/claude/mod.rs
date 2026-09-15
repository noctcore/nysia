//! **The one module Claude's specifics are allowed to live in** (D-3, D-4).
//!
//! Everything that would have to change if a second agent arrived is in here: what the CLI
//! is called, how it is resolved on each platform, which hook events it has, what one of its
//! settings entries looks like, and where its settings file is. Nothing outside
//! [`super`] may name this module, and the reasoning is in [`super`]'s own docs rather than
//! repeated here.
//!
//! The module is declared **private** by [`super`], so Rust's own privacy is the load-bearing
//! half of the boundary — `no-claude-specifics-outside-agent` in `tools/lint-meta` is the
//! half that reports a file and a line.
//!
//! Types flow the other way: this module imports [`super::launch::AgentLaunch`] and
//! [`super::hooks::HookChange`] rather than defining its own and having them re-exported
//! out. A `pub use` in `agent/` that handed a Claude type on under a neutral name would put
//! Claude's shape in the neutral surface while the word `claude` disappeared from it, which
//! is exactly the laundering the lint rule's second half reports.

pub(super) mod hooks;
mod launch;

pub(super) use launch::launch;
