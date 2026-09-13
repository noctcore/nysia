// Rule (a): spellings a line-anchored `use tauri::` regex walked straight past.
// The line numbers below are asserted by scripts/prove-lint-meta.ts, so do not reformat.

use ::tauri::Builder;

// A grouped import on one line.
use {tauri, serde};

// The same, spanning lines.
use {
    tauri::Manager,
    serde::Serialize,
};

// A string literal must not be able to open a comment. Without literal handling the `"/*"`
// below swallows the rest of the file and every import above stays invisible.
const OPEN: &str = "/*";
const LINE: &str = "// use tauri::Nothing;";
const QUOTE: char = '"';
const RAW: &str = r#"/* tauri::Nothing */"#;

/// A doc comment naming tauri:: must not trip the rule — comments discuss it constantly.
// Nor must a line comment saying use tauri::Builder;
pub fn leaked() {
    let _ = (Builder::default(), tauri::generate_context!(), OPEN, LINE, QUOTE, RAW);
}
