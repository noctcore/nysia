//! This module doc talks about `tauri` and `use tauri::Builder;` constantly, because that
//! is the rule it exists to explain. Comments are blanked before the scan, so none of it
//! trips — otherwise nysia-core's own documentation would fail the gate.

/*
 * A block comment may also say tauri::Builder and carry a stray `;` that would otherwise
 * truncate the statement after it. /* Nested block comments are handled too: tauri:: */
 */

use crate::tauri_helpers::Nothing;
use std::fmt;

pub use self::fmt as formatting;

pub fn safe(_: Nothing) {}
