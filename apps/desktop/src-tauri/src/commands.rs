//! Every verb the webview may ask the Rust side for.
//!
//! Three rules hold for all of them, and the first two are traps that have already cost
//! someone a day (traps register #2):
//!
//! 1. **`async fn`.** A synchronous command runs on the main thread and freezes the
//!    webview for as long as it takes. Every one of these is async.
//! 2. **`spawn_blocking`, never `tokio::spawn`.** The socket client is blocking by design,
//!    so the work goes to a blocking pool. `tokio::spawn` is forbidden here for a sharper
//!    reason than style: a panic inside one unwinds across wry's `extern "C"` boundary and
//!    becomes `abort()` — the whole app dies with no message.
//! 3. **`try_state`, never `state`.** `state()` panics when the state is absent, and a
//!    panic in a command is the `abort()` above. `try_state` returns `None` and the command
//!    answers with a [`CommandFailure`] the user can read.
//!
//! Every failure is a `CommandFailure` rather than a bare string, because the webview turns
//! it into a `StoreCommandError` — the one rejection type the store surfaces to the user.
//! Anything else reaches `console.error` and nobody sees it.

use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::handshake::DaemonIdentity;
use nysia_proto::identity::SessionHandle;
use nysia_proto::session::{SessionClose, SessionCreate, SessionList, SessionSummary};
use nysia_proto::terminal::{TerminalResize, TerminalSend};
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::channel::framing::StreamId;
use crate::daemon::{CommandFailure, DaemonError};
use crate::state::Client;

/// What every command answers with when it could not do the thing.
type Failed<T> = Result<T, CommandFailure>;

/// Pull the managed client out of Tauri's state without panicking.
fn client<'a>(app: &'a State<'a, Client>) -> &'a Client {
    app.inner()
}

/// Run `work` on the blocking pool.
///
/// The whole reason the commands below are three lines each: this is the only place that
/// knows how work leaves the main thread, so no command can get it wrong.
async fn blocking<T, F>(work: F) -> Failed<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DaemonError> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(outcome) => outcome.map_err(CommandFailure::from),
        // The blocking pool lost the task. Not something a user can act on, but it must not
        // hang the caller's promise forever either.
        Err(error) => Err(CommandFailure::from(DaemonError::Io(error.to_string()))),
    }
}

/// Connect to the daemon, or report why not.
///
/// Idempotent: calling it while already connected returns the identity of the daemon
/// already attached, so the store's reconnect loop does not have to track whether it has
/// called this before.
///
/// # Errors
///
/// Every [`DaemonError`], flattened to a [`CommandFailure`] the webview can show.
#[tauri::command]
pub async fn daemon_connect(app: State<'_, Client>) -> Failed<DaemonIdentity> {
    let client = client(&app).clone();
    blocking(move || client.connect()).await
}

/// Whether the window currently holds a connection, and to which daemon.
///
/// # Errors
///
/// Never fails — `Ok(None)` is "not connected", which is a state rather than a failure.
#[tauri::command]
pub async fn daemon_status(app: State<'_, Client>) -> Failed<Option<DaemonIdentity>> {
    let client = client(&app).clone();
    blocking(move || Ok(client.identity())).await
}

/// Block until the connection drops, then return.
///
/// This is how the webview learns the daemon went away while no command was in flight,
/// without `emit` (tauri#12724, traps register #3) and without a second frame kind. The
/// store awaits it, flips to `reconnecting` when it resolves, and re-invokes
/// [`daemon_connect`] on a backoff.
///
/// # Errors
///
/// Never fails: a dropped connection is the answer, not an error.
#[tauri::command]
pub async fn daemon_watch(app: State<'_, Client>) -> Failed<()> {
    let client = client(&app).clone();
    blocking(move || {
        client.wait_for_disconnect();
        Ok(())
    })
    .await
}

/// Every session the daemon is holding.
///
/// The verb that makes D-1 observable: a window that has just started, attaching to a
/// daemon that has been running for hours, gets back the sessions that outlived the last
/// window.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon is unreachable or refuses.
#[tauri::command]
pub async fn session_list(app: State<'_, Client>) -> Failed<Vec<SessionSummary>> {
    let client = client(&app).clone();
    blocking(
        move || match client.request(RequestPayload::SessionList(SessionList {}))? {
            ResponsePayload::SessionList { sessions } => Ok(sessions),
            other => Err(unexpected("session_list", &other)),
        },
    )
    .await
}

/// Start a session.
///
/// # Errors
///
/// [`CommandFailure`] carrying the daemon's own `nextSteps` when the shell will not
/// start — "pwsh is not on PATH" and what to do about it, rather than a status code.
#[tauri::command]
pub async fn session_create(
    app: State<'_, Client>,
    request: SessionCreate,
) -> Failed<SessionHandle> {
    let client = client(&app).clone();
    blocking(
        move || match client.request(RequestPayload::SessionCreate(request))? {
            ResponsePayload::SessionCreate(created) => Ok(created.handle),
            other => Err(unexpected("session_create", &other)),
        },
    )
    .await
}

/// Close a session, and with it the process tree behind it.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon refuses or the session is already gone.
#[tauri::command]
pub async fn session_close(app: State<'_, Client>, handle: SessionHandle) -> Failed<()> {
    let client = client(&app).clone();
    blocking(move || {
        client.forget(&handle);
        match client.request(RequestPayload::SessionClose(SessionClose { handle }))? {
            ResponsePayload::SessionClose => Ok(()),
            other => Err(unexpected("session_close", &other)),
        }
    })
    .await
}

/// Hand the Rust side the one `Channel` every session's output rides.
///
/// One channel for every session, never one per session and never `emit`: tauri#12724 is a
/// memory leak on sustained emits, and terminal output is the definition of a sustained
/// emit (traps register #3). Frames carry a stream id so the webview can take them apart
/// again.
///
/// Calling this twice replaces the sink — which is what a webview reload does, and it must
/// not leave the old channel receiving frames nobody reads.
///
/// # Errors
///
/// [`CommandFailure`] if the client is not connected.
#[tauri::command]
pub async fn terminal_attach(
    app: State<'_, Client>,
    channel: Channel<InvokeResponseBody>,
) -> Failed<()> {
    let client = client(&app).clone();
    blocking(move || client.attach_channel(channel)).await
}

/// Report that the webview has rendered `bytes` of `stream`.
///
/// Invoked from inside xterm's `write()` callback, never on arrival: the credit window
/// tracks what has been *rendered*, and a message sitting in a queue has not been (§7.3).
/// This is what returns credit upstream and lets a stalled reader start again.
///
/// # Errors
///
/// [`CommandFailure`] if the client is not connected.
#[tauri::command]
pub async fn terminal_ack(app: State<'_, Client>, stream: StreamId, bytes: u32) -> Failed<()> {
    let client = client(&app).clone();
    blocking(move || client.rendered(stream, bytes)).await
}

/// Send input to a session.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon refuses or the session is gone.
#[tauri::command]
pub async fn terminal_send(app: State<'_, Client>, request: TerminalSend) -> Failed<()> {
    let client = client(&app).clone();
    blocking(
        move || match client.request(RequestPayload::TerminalSend(request))? {
            ResponsePayload::TerminalSend => Ok(()),
            other => Err(unexpected("terminal_send", &other)),
        },
    )
    .await
}

/// Tell the daemon the pane changed size.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon refuses or the session is gone.
#[tauri::command]
pub async fn terminal_resize(app: State<'_, Client>, request: TerminalResize) -> Failed<()> {
    let client = client(&app).clone();
    blocking(
        move || match client.request(RequestPayload::TerminalResize(request))? {
            ResponsePayload::TerminalResize => Ok(()),
            other => Err(unexpected("terminal_resize", &other)),
        },
    )
    .await
}

/// Which OS this is, for the renderer policy.
///
/// The WebGL pool is a macOS-only opt-in (§7.3), and the honest way to know is to ask the
/// process rather than to parse a user-agent string — WebView2 and WKWebView both report
/// strings that have changed between releases, and a renderer that guesses wrong either
/// forfeits WebGL or exhausts WebKit's app-wide context cap.
///
/// # Errors
///
/// Never fails.
#[tauri::command]
pub async fn host_platform() -> Failed<&'static str> {
    Ok(std::env::consts::OS)
}

/// A response of the wrong shape is a protocol failure, not a silent success.
fn unexpected(verb: &str, got: &ResponsePayload) -> DaemonError {
    DaemonError::Protocol(format!("{verb} was answered with a {} payload", got.verb()))
}

#[cfg(test)]
mod tests {
    use nysia_proto::identity::{Incarnation, PaneKey};
    use nysia_proto::session::SessionCreated;

    use super::*;

    #[test]
    fn a_response_of_the_wrong_shape_is_reported_rather_than_ignored() {
        // The failure this catches: a daemon bug that answers `terminal_send` with a
        // `session_close` payload. Treating that as success would leave the window
        // believing it had typed something it never sent.
        let error = unexpected("terminal_send", &ResponsePayload::SessionClose);
        assert!(matches!(error, DaemonError::Protocol(_)));
        assert!(error.to_string().contains("terminal_send"));
        assert!(error.to_string().contains("session_close"));
        // A protocol disagreement never becomes true by retrying.
        assert!(!error.retryable());
    }

    #[test]
    fn the_verb_named_in_the_message_is_the_payloads_own() {
        let pane_key: PaneKey = "tab_1:leaf_1".parse().expect("a well-formed pane key");
        let created = ResponsePayload::SessionCreate(SessionCreated {
            handle: "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
                .parse()
                .expect("a well-formed handle"),
            incarnation: Incarnation::new(&pane_key, 1),
            pane_key,
        });
        assert!(
            unexpected("session_list", &created)
                .to_string()
                .contains(created.verb())
        );
    }
}
