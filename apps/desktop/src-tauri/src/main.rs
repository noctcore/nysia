//! The Nysia window.
//!
//! Deliberately thin. Under D-1 and D-2 the daemon owns every PTY, the store, git and
//! orchestration; this process opens a window, and killing it must never interrupt a
//! running session. Anything stateful that appears here is a bug, not a shortcut.
//!
//! Two rules for whoever fills this in (wave 2, W5):
//!
//! - **Every Tauri command is `async fn` + `spawn_blocking` + `try_state`.** A sync command
//!   runs on the main thread and freezes the webview. Never `tokio::spawn` inside one: the
//!   panic unwinds across wry's `extern "C"` boundary and becomes `abort()`.
//! - **Never `emit` for streams** (tauri#12724 leaks). Terminal output goes through one
//!   multiplexed binary `Channel`, coalesced to at least 1 KiB per frame — payloads under
//!   1024 bytes are delivered through `eval`.
//!
//! There are no commands yet. Wave 0 ships a window and nothing else.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NYSIA_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // The window chrome is drawn by the webview on every platform (decorations are off in
    // tauri.conf.json), because the design uses the same custom titlebar everywhere.
    match tauri::Builder::default().run(tauri::generate_context!()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "the Nysia window could not start");
            ExitCode::FAILURE
        }
    }
}
