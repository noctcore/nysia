//! Tauri's build script: validates `tauri.conf.json`, embeds the icon resource on Windows,
//! and generates the capability schemas.

fn main() {
    tauri_build::build();
}
