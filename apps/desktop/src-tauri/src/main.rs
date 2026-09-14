//! The Nysia window.
//!
//! Deliberately thin. Under D-1 and D-2 the daemon owns every PTY, the store, git and
//! orchestration; this process opens a window, and killing it must never interrupt a
//! running session. Anything stateful that appears here is a bug, not a shortcut.
//!
//! Two rules that govern every line added here:
//!
//! - **Every Tauri command is `async fn` + `spawn_blocking` + `try_state`.** A sync command
//!   runs on the main thread and freezes the webview. Never `tokio::spawn` inside one: the
//!   panic unwinds across wry's `extern "C"` boundary and becomes `abort()`.
//! - **Never `emit` for streams** (tauri#12724 leaks). Terminal output goes through one
//!   multiplexed binary `Channel`, coalesced to at least 1 KiB per frame — payloads under
//!   1024 bytes are delivered through `eval`.
//!
//! What lives here is the client half of the socket protocol and the one multiplexed
//! `Channel` that carries terminal output to the webview. Nothing else: sessions, PTYs,
//! terminal state and the store all belong to the daemon.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod channel;
mod commands;
mod daemon;
#[cfg(test)]
mod interop;
mod state;

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
    //
    // The client is managed state rather than a global, so every command reaches it through
    // `try_state` and a missing one is a message rather than a panic. It holds no
    // connection until the webview asks for one: the daemon outlives the window, so
    // connecting is something the window does, not something it is born with (D-1).
    let app = tauri::Builder::default()
        .manage(state::Client::new())
        .invoke_handler(tauri::generate_handler![
            commands::daemon_connect,
            commands::daemon_status,
            commands::daemon_watch,
            commands::session_list,
            commands::session_create,
            commands::session_close,
            commands::terminal_attach,
            commands::stream_attach,
            commands::stream_detach,
            commands::terminal_ack,
            commands::terminal_send,
            commands::terminal_resize,
            commands::host_platform,
        ]);

    match app.run(tauri::generate_context!()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "the Nysia window could not start");
            ExitCode::FAILURE
        }
    }
}
