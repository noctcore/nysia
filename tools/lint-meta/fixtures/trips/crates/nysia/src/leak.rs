// Rule (a) fixture. Every literal form below sits ABOVE the imports it must not hide, and
// the line numbers of those imports are asserted by scripts/prove-lint-meta.ts — so do not
// reformat this file, and do not move the literals back below them. When they sat below,
// removing the literal handling still left the proof printing OK: only the unit tests
// caught it, which made the CI step named "prove the lint-meta architecture rules trip"
// vacuous for exactly the regression it exists to prevent.
//
// The emoji are load-bearing too: they are astral, so a blanker that indexes by code point
// instead of UTF-16 code unit drifts one slot per emoji and erases the imports below.
// 🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥
use std::ffi::CStr;

const OPEN: &str = "/*";
const LINE: &str = "// use tauri::Nothing;";
const QUOTE: char = '"';
const ESCAPED: &str = "he said \"/*\"";
const RAW: &str = r#"/* tauri::Nothing */"#;
const BYTES: &[u8] = b"/*";
const CSTR: &CStr = c"/*";
const CRAW: &CStr = cr#"/*"#;

use ::tauri::Builder;

// A grouped import on one line.
use {tauri, serde};

// The same, spanning lines.
use {
    tauri::Manager,
    serde::Serialize,
};

/// A doc comment naming tauri:: must not trip the rule — comments discuss it constantly.
// Nor must a line comment saying use tauri::Builder;
pub fn leaked() {
    let _ = (Builder::default(), tauri::generate_context!(), OPEN, LINE, QUOTE, RAW);
    let _ = (ESCAPED, BYTES, CSTR, CRAW);
}
