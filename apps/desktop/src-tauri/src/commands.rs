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
//!    panic in a command is the `abort()` above. Every command below takes an `AppHandle`
//!    and resolves the client through [`client`], which calls `try_state` and turns a
//!    missing one into a [`CommandFailure`] the user can read.
//!
//! Every failure is a `CommandFailure` rather than a bare string, because the webview turns
//! it into a `StoreCommandError` — the one rejection type the store surfaces to the user.
//! Anything else reaches `console.error` and nobody sees it.

use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::handshake::DaemonIdentity;
use nysia_proto::identity::SessionHandle;
use nysia_proto::session::{SessionClose, SessionCreate, SessionList, SessionSummary};
use nysia_proto::terminal::{TerminalResize, TerminalSend};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager};

use crate::daemon::{CommandFailure, DaemonError};
use crate::state::Client;
use nysia_proto::stream::StreamId;

/// What every command answers with when it could not do the thing.
type Failed<T> = Result<T, CommandFailure>;

/// Pull the managed client out of Tauri's state without panicking.
///
/// `try_state`, never `state`. `state()` panics when the state is absent, and a panic in a
/// command unwinds across wry's `extern "C"` boundary and becomes `abort()` — the whole
/// app dies with no message (traps register #2). This returns an error the user can read
/// instead, which is a state that should never happen and must still not be fatal.
fn client(app: &AppHandle) -> Result<Client, DaemonError> {
    app.try_state::<Client>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| DaemonError::Io("the window started without a daemon client".to_owned()))
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
pub async fn daemon_connect(app: AppHandle) -> Failed<DaemonIdentity> {
    let client = client(&app)?;
    blocking(move || client.connect()).await
}

/// Whether the window currently holds a connection, and to which daemon.
///
/// # Errors
///
/// Never fails — `Ok(None)` is "not connected", which is a state rather than a failure.
#[tauri::command]
pub async fn daemon_status(app: AppHandle) -> Failed<Option<DaemonIdentity>> {
    let client = client(&app)?;
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
pub async fn daemon_watch(app: AppHandle) -> Failed<()> {
    let client = client(&app)?;
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
pub async fn session_list(app: AppHandle) -> Failed<Vec<SessionSummary>> {
    let client = client(&app)?;
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
pub async fn session_create(app: AppHandle, request: SessionCreate) -> Failed<SessionHandle> {
    let client = client(&app)?;
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
pub async fn session_close(app: AppHandle, handle: SessionHandle) -> Failed<()> {
    let client = client(&app)?;
    blocking(move || {
        client.detach_session(&handle);
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
pub async fn terminal_attach(app: AppHandle, channel: Channel<InvokeResponseBody>) -> Failed<()> {
    let client = client(&app)?;
    blocking(move || client.attach_channel(channel)).await
}

/// Ask the daemon to route a session's output on this window's stream connection.
///
/// Separate from [`terminal_attach`], which opens the connection itself. The daemon assigns
/// the id — a client cannot, because ids are per connection and only the daemon knows which
/// are in use — so this answers with the one a pane's frames will carry.
///
/// # Errors
///
/// [`CommandFailure`] if no stream connection is open, or whatever the daemon said.
#[tauri::command]
pub async fn stream_attach(app: AppHandle, handle: SessionHandle) -> Failed<StreamId> {
    let client = client(&app)?;
    blocking(move || client.attach_session(handle)).await
}

/// Stop routing a session's output.
///
/// # Errors
///
/// Never fails: a session that has already gone took its stream with it, and reporting that
/// as an error would put a notice in front of the user for an ordinary close.
#[tauri::command]
pub async fn stream_detach(app: AppHandle, handle: SessionHandle) -> Failed<()> {
    let client = client(&app)?;
    blocking(move || {
        client.detach_session(&handle);
        Ok(())
    })
    .await
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
pub async fn terminal_ack(app: AppHandle, stream: StreamId, bytes: u32) -> Failed<()> {
    let client = client(&app)?;
    blocking(move || client.rendered(stream, bytes)).await
}

/// Send input to a session.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon refuses or the session is gone.
#[tauri::command]
pub async fn terminal_send(app: AppHandle, request: TerminalSend) -> Failed<()> {
    let client = client(&app)?;
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
pub async fn terminal_resize(app: AppHandle, request: TerminalResize) -> Failed<()> {
    let client = client(&app)?;
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
/// The body is a constant, so `spawn_blocking` buys nothing here on its own. It is used
/// anyway: "every command is `async fn` plus `spawn_blocking`" is only a rule anyone can
/// check if it holds for all of them, and an exception justified by today's body is an
/// exception that outlives the justification the first time someone adds a line to it.
///
/// # Errors
///
/// Never fails.
#[tauri::command]
pub async fn host_platform() -> Failed<&'static str> {
    blocking(|| Ok(std::env::consts::OS)).await
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
