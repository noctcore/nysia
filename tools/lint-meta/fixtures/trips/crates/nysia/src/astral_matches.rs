// The same defect reached through a match pattern rather than an array literal. Also alone
// in its file, for the same reason.
fn is_marker(c: char) -> bool {
    matches!(c, '🔥'|'"')
}

use tauri::Manager;
