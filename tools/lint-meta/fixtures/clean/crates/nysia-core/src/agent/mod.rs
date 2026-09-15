//! The arrangement rule (f) is built around, written the way the real module is written.
//!
//! The doc discusses `crate::agent::claude` and `pub use claude::ClaudeLaunch;` at length,
//! because explaining the boundary is what a boundary's own documentation is for. None of it
//! may trip: comments are blanked before the scan.

mod claude;
mod launch;

// Naming the module from inside `agent/**` is ordinary, on a `use` and on a path alike.
use claude::hooks::EVENTS;
use crate::agent::claude::launch as claude_launch;

// Re-exporting is fine too, as long as what is handed on is neutral. `AgentLaunch` is
// declared in `launch.rs`, here in `agent/`, and the Claude module imports it upward.
pub use launch::{AgentLaunch, LaunchError};

pub fn installed() -> usize {
    let _ = claude_launch::PROGRAM;
    let _ = crate::agent::claude::hooks::install;
    EVENTS.len()
}
