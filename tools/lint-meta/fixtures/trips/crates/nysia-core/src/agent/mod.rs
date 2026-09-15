//! The two ways a boundary dissolves from the inside, which the compiler permits.
//!
//! This file is *inside* `agent/**`, so naming `claude` here is fine — `agent/launch.rs`
//! next door does it on every call. What is not fine is handing the module's contents back
//! out, and neither of the lines below is a compile error.

mod claude;
mod launch;

// Laundering. Callers write `agent::ClaudeLaunch` and `agent::EVENTS`, so they get Claude's
// shape while the word `claude` disappears from the path — which is where a rule matching on
// that word stops seeing it. This is the reason the real module declares its neutral types in
// `agent/` and has `claude/` import them upward instead.
pub use claude::ClaudeLaunch;
pub use self::claude::hooks::EVENTS as HOOK_EVENTS;
pub use claude::{hooks::install, launch::PROGRAM};

// Un-privating. Rust's own privacy is the load-bearing half of this boundary; any visibility
// modifier at all takes it away, and `pub(crate)` takes it away just as completely as `pub`.
pub(crate) mod claude;

// And a module whose name merely begins the same way, which must stay silent — the rule is
// about the `claude` module, not about every identifier starting with those six letters.
pub mod claude_helpers;

// Neutral, and silent: a re-export that passes through nothing called `claude`.
pub use launch::AgentLaunch;
