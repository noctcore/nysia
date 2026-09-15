//! A crate outside `agent/**` reaching for Claude's specifics (D-3, D-4).
//!
//! Every `use` below is a different spelling of the same import, and each one is a way past
//! a rule that matched the literal text `crate::agent::claude`. The grouped forms are the
//! point: neither of them writes those two segments next to each other, which is the shape
//! rule (a) already learned the hard way with `use {tauri, serde};`.
//!
//! This doc comment names crate::agent::claude::PROGRAM repeatedly on purpose. Comments are
//! blanked before the scan, so a rule that searched the text would report these lines and
//! `agent/mod.rs`'s own documentation along with them.

/* A block comment naming crate::agent::claude, with a stray ; that would truncate a use. */

use crate::agent::claude::PROGRAM;
use crate::agent::claude as shim;
use crate::agent::{claude, hooks};
use crate::agent::{
    claude::{install, uninstall},
    launch,
};
use ::nysia_core::agent::claude::EVENTS;

/// A string is not an import, however it reads.
pub const MENTIONS: &str = "crate::agent::claude::PROGRAM";

pub fn leak() -> &'static str {
    let _ = crate::agent::claude::EVENTS;
    shim::PROGRAM
}
