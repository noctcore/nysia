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

// Laundering through an alias declared in this same file. None of the three lines below
// writes the word `claude` in the statement that does the handing-on, so a rule matching on
// spelling reports none of them — which is how the boundary was walkable from the inside
// while `pnpm lint` said zero and `cargo clippy -D warnings` agreed. Rust privacy does not
// help here: this is the module that is *allowed* to name `claude`.
use claude as c;
pub use c::Probe as NeutralProbe;
pub use c::*;

// The same laundering without a `use` at all: a public type alias, and a public signature.
// Both put a Claude type in the neutral surface exactly as a `pub use` would.
pub type AliasProbe = claude::Probe;
pub fn probe() -> claude::launch::T {
    unimplemented!()
}

// A second alias hop, through a module rather than a type.
use self::claude::hooks as h;
pub use h::EVENTS;

// Silent, and each of these was a false report before the resolution went in. A module named
// `claude` under a different parent is a different module; a restricted visibility cannot
// hand anything out of `agent/` at all; and a private item is private.
pub use crate::vendor::claude::Whatever;
pub(self) mod claude;
pub(in crate::agent) mod claude;
use claude::hooks::EVENTS as INTERNAL;
type PrivateProbe = claude::Probe;
fn internal() -> claude::launch::T {
    unimplemented!()
}

// Nesting, which `pub` on its own says nothing about. Neither of these hands anything to
// anybody — an item inside a private module, and a public field on a private struct — and a
// scan that looked for `pub` tokens without tracking what encloses them reported both.
mod private {
    pub type Hidden = claude::Probe;
}
struct Inner {
    pub held: claude::Probe,
}

// And the same nesting where it does escape, so the tracking cannot simply go quiet: a
// public module, and an inherent impl on a type this file declares public.
pub(crate) mod exported {
    pub type Leaked = claude::Probe;
}
impl AgentLaunch {
    pub fn leaked() -> claude::Probe {
        unimplemented!()
    }
}

// The type the impl above is for, declared public here so its visibility can be looked up.
pub struct AgentLaunch;
