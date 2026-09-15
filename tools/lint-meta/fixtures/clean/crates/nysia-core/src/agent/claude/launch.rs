//! Inside the Claude module, where everything is allowed.
//!
//! Types flow up and behaviour stays down: this file imports the neutral `AgentLaunch` from
//! `agent/launch.rs` rather than declaring a Claude one and having `agent/mod.rs` re-export
//! it, which is the arrangement that leaves the laundering half of rule (f) with nothing to
//! report in the real module.

use crate::agent::launch::AgentLaunch;
use crate::pty::resolve;

pub(in crate::agent) const PROGRAM: &str = "claude";

pub use crate::agent::claude::hooks::EVENTS;

pub(in crate::agent) fn launch() -> AgentLaunch {
    AgentLaunch::new(PROGRAM, resolve(PROGRAM))
}
