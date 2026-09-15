//! The module itself, which may import whatever it likes.
//!
//! It sits in the `trips` tree rather than the clean one because it is only meaningful next
//! to the file above it: `agent/mod.rs` reports, and this file — which names
//! `claude` far more often — reports nothing. A rule that had simply banned the word would
//! report both, and the exemption is the whole point of having a module to put things in.

use crate::agent::claude::hooks::EVENTS;
use crate::agent::hooks::HookChange;

pub(super) mod hooks;
pub mod launch;

pub use hooks::install;
pub use self::launch::{PROGRAM, launch};

pub fn everything() -> (&'static [&'static str; 12], HookChange) {
    (&EVENTS, crate::agent::claude::hooks::nothing())
}
