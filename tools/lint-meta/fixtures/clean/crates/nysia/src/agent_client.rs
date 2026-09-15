//! A crate outside `agent/**` that stays on the right side of the boundary.
//!
//! It talks about `crate::agent::claude::PROGRAM` in prose, holds the same text in a string,
//! and imports a `claude` module belonging to somebody else — none of which is reaching into
//! the agent's Claude module, and all three of which a rule matching the bare word would
//! report. There is no way to silence a false report in lint-meta, so the rule has to be
//! right rather than merely strict.

use crate::agent::{AgentLaunch, hooks};

// A `claude` module under a different parent is a different module. The rule looks for the
// `agent::claude` pair, not for the segment on its own.
use crate::vendor::claude::Whatever;
use crate::agent::launch;

/// A string naming crate::agent::claude is not an import.
pub const DOCUMENTED: &str = "crate::agent::claude::PROGRAM";

pub fn start() -> AgentLaunch {
    let _: Option<Whatever> = None;
    let _ = hooks::install;
    launch::launch([])
}
