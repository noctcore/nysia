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
//!
//! # What this module writes to the log, and what it never writes
//!
//! Every command goes through [`blocking`], which is therefore the one place that knows a
//! verb was attempted and how it ended. It logs **the verb's name**, and on a failure the
//! [`DaemonError::kind`] and whether it is retryable. That is the whole vocabulary.
//!
//! It does **not** log the request. Not abbreviated, not redacted, not on failure: the
//! argument never reaches a `tracing` macro at all, because `TerminalSend.text` is what the
//! user typed and may be a password, `SessionCreate.env_overrides` is where a token reaches a
//! shell, and `cwd` names a person's disk. `nysia_core::rpc::log_file` states the rule for
//! every Nysia log and `a_verbs_name_is_logged_and_its_payload_is_not` holds this half of it.
//!
//! Two answers *are* logged, once each, and they are the reason the rest of the log can be
//! read at all: [`session_create`] records the `pane_key`, `handle` and `incarnation` the
//! daemon assigned, and [`stream_attach`] records which `StreamId` a handle was given. Those
//! two lines are what let every later frame, ack and stall — on either side, in either file —
//! be traced back to a pane. Nothing new was invented for it: the protocol already carried
//! all four, and #75's complaint was that nothing used them for correlation.

use std::path::PathBuf;

use nysia_proto::envelope::{RequestPayload, ResponsePayload};
use nysia_proto::handshake::DaemonIdentity;
use nysia_proto::identity::SessionHandle;
use nysia_proto::project::{Project, ProjectId, ProjectList, ProjectRegister, ProjectRegistered};
use nysia_proto::session::{SessionClose, SessionCreate, SessionList, SessionSummary};
use nysia_proto::tasks::{Issue, TasksList};
use nysia_proto::terminal::{TerminalResize, TerminalSend};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

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

/// Run `work` on the blocking pool, and record that the verb was attempted.
///
/// The whole reason the commands below are three lines each: this is the only place that
/// knows how work leaves the main thread, so no command can get it wrong. It is also the
/// only place that knows a verb *happened*, which is why the log line lives here rather than
/// at fourteen call sites that would each have to remember it.
///
/// `verb` is a `&'static str` and not, say, the request: a caller cannot pass anything here
/// that was not compiled into the binary, so there is no argument at which a payload could be
/// substituted. See the module docs for what that rules out and why.
///
/// **The success line is `trace`, not `debug`, and that is a privacy decision.** `terminal_send`
/// is one verb per keystroke and `terminal_ack` one per render batch, so a file recording every
/// success carries the *cadence* of what was typed — which is a weak side-channel on a
/// password even though the text never appears. Failures stay at `warn`, which is what
/// "which verb was in flight" is actually asked about; somebody who wants the successful ones
/// can ask for them with `NYSIA_LOG=trace` and know what they are turning on.
async fn blocking<T, F>(verb: &'static str, work: F) -> Failed<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DaemonError> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(Ok(value)) => {
            tracing::trace!(verb, "served");
            Ok(value)
        }
        Ok(Err(error)) => {
            // `kind` and `retryable`, never `error` itself. Its `Display` is the sentence the
            // user is about to be shown, built from whatever the failure was about.
            tracing::warn!(
                verb,
                kind = error.kind(),
                retryable = error.retryable(),
                "a command failed"
            );
            Err(CommandFailure::from(error))
        }
        // The blocking pool lost the task. Not something a user can act on, but it must not
        // hang the caller's promise forever either. The log gets `join` and nothing else: a
        // `JoinError` renders the panic message, and a panic message is arbitrary text from
        // wherever it was raised.
        Err(error) => {
            tracing::error!(verb, kind = "join", "a command never ran");
            Err(CommandFailure::from(DaemonError::Io(error.to_string())))
        }
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
    blocking("daemon_connect", move || client.connect()).await
}

/// Whether the window currently holds a connection, and to which daemon.
///
/// # Errors
///
/// Never fails — `Ok(None)` is "not connected", which is a state rather than a failure.
#[tauri::command]
pub async fn daemon_status(app: AppHandle) -> Failed<Option<DaemonIdentity>> {
    let client = client(&app)?;
    blocking("daemon_status", move || Ok(client.identity())).await
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
    blocking("daemon_watch", move || {
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
    blocking("session_list", move || {
        match client.request(RequestPayload::SessionList(SessionList {}))? {
            ResponsePayload::SessionList { sessions } => Ok(sessions),
            other => Err(unexpected("session_list", &other)),
        }
    })
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
    blocking("session_create", move || {
        match client.request(RequestPayload::SessionCreate(request))? {
            ResponsePayload::SessionCreate(created) => {
                // **One of the two correlation lines**, and the reason the rest of either log
                // can be read. Everything after this names a handle or a stream id; this is
                // what says which pane those belong to. All three come off the daemon's
                // answer, so none of it is the request echoed back — see the module docs.
                tracing::info!(
                    pane_key = %created.pane_key,
                    handle = %created.handle,
                    incarnation = %created.incarnation,
                    "the daemon opened a session"
                );
                Ok(created.handle)
            }
            other => Err(unexpected("session_create", &other)),
        }
    })
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
    blocking("session_close", move || {
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
    blocking("terminal_attach", move || client.attach_channel(channel)).await
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
    blocking("stream_attach", move || {
        let stream = client.attach_session(handle.clone())?;
        // The second correlation line. A `StreamId` is a small integer scoped to one stream
        // connection and appears on every frame, ack and stall from here on; without this
        // line none of them resolves to a session, and #75's complaint was exactly that.
        tracing::info!(%handle, %stream, "the daemon routed a session onto this connection");
        Ok(stream)
    })
    .await
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
    blocking("stream_detach", move || {
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
    blocking("terminal_ack", move || client.rendered(stream, bytes)).await
}

/// Send input to a session.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon refuses or the session is gone.
#[tauri::command]
pub async fn terminal_send(app: AppHandle, request: TerminalSend) -> Failed<()> {
    let client = client(&app)?;
    blocking("terminal_send", move || {
        match client.request(RequestPayload::TerminalSend(request))? {
            ResponsePayload::TerminalSend => Ok(()),
            other => Err(unexpected("terminal_send", &other)),
        }
    })
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
    blocking("terminal_resize", move || {
        match client.request(RequestPayload::TerminalResize(request))? {
            ResponsePayload::TerminalResize => Ok(()),
            other => Err(unexpected("terminal_resize", &other)),
        }
    })
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
    blocking("host_platform", || Ok(std::env::consts::OS)).await
}

/// Every folder registered as a project.
///
/// The list the sidebar draws. Until v0.3 wave C1 serves the verb this answers
/// `unsupported`, which is a real answer and not a fault: the window shows an empty sidebar
/// carrying the daemon's own sentence, rather than ten names it made up.
///
/// # Errors
///
/// [`CommandFailure`] if the daemon is unreachable or does not serve the verb yet.
#[tauri::command]
pub async fn project_list(app: AppHandle) -> Failed<Vec<Project>> {
    let client = client(&app)?;
    blocking("project_list", move || {
        match client.request(RequestPayload::ProjectList(ProjectList {}))? {
            ResponsePayload::ProjectList { projects } => Ok(projects),
            other => Err(unexpected("project_list", &other)),
        }
    })
    .await
}

/// A project's open GitHub issues, queried live (D-5).
///
/// The Tasks screen's only data source. The project and nothing else travels: the daemon
/// resolves the registered folder and lets `gh` read the repository out of it, so there is no
/// slug or path on this call — which is also what keeps a person's disk out of it (traps
/// register #13/#14).
///
/// # What this forwarder has to preserve
///
/// **The refusal code.** The screen draws three different headings — `gh_missing`,
/// `gh_unauthenticated`, `query_failed` — and degrades a code it does not recognise to the
/// last. That works because [`CommandFailure::kind`](crate::daemon::CommandFailure) is the
/// daemon's own code for a *known* `ErrorCode` and the literal `"other"` for anything else,
/// and the three codes are known to `nysia-proto` as of v0.3 wave C3. A generic message here
/// would collapse all three into one heading and put the screen back to saying the same thing
/// about four different endings, which is exactly what the plan's §5 forbids.
///
/// **An empty list is a success.** A repository with no open issues answers `Ok(vec![])`, not
/// an error, and the screen says "No open issues" rather than "not signed in".
///
/// # Errors
///
/// [`CommandFailure`] carrying the daemon's own code, sentence and next steps — including
/// `gh auth login` as `nextCommandArgs` when the credentials are the problem.
#[tauri::command]
pub async fn tasks_list(app: AppHandle, project: ProjectId) -> Failed<Vec<Issue>> {
    let client = client(&app)?;
    // Blocking, and the one command here that waits on the network: the daemon spawns `gh`,
    // which is a round trip to GitHub. On the main thread that is a frozen webview (traps
    // register #2).
    blocking("tasks_list", move || {
        match client.request(RequestPayload::TasksList(TasksList { project }))? {
            ResponsePayload::TasksList { issues } => Ok(issues),
            other => Err(unexpected("tasks_list", &other)),
        }
    })
    .await
}

/// Register a folder as a project.
///
/// Idempotent, and the answer says which of the two happened — see
/// [`ProjectRegistered::already_registered`]. Registering a folder that is already
/// registered is **not** an error and must not be shown as one (v0.3 plan §3.2).
///
/// The path travels unresolved: canonicalising is the daemon's job, and a client that did it
/// first would be a second implementation of the identity rule that everything else in this
/// milestone rests on.
///
/// # What can be slow here, and why it is still not the main thread
///
/// The daemon canonicalises before it does anything else, and `fs::canonicalize` on a
/// disconnected network share blocks for the OS's own SMB timeout — outside any git deadline,
/// because it happens before git is reached. That is the daemon's to bound. What this side
/// owes the user is that a folder on a dead share cannot freeze the window, which is what
/// [`blocking`] is for (traps register #2).
///
/// # Errors
///
/// [`CommandFailure`] carrying the daemon's own code and next steps: `not_a_repository`,
/// `many_repositories` and `path_unreadable` are the three refusals §3.2 requires a caller
/// to be able to tell apart, and [`CommandFailure::kind`](crate::daemon::CommandFailure)
/// is what it tells them apart by.
#[tauri::command]
pub async fn project_register(app: AppHandle, path: String) -> Failed<ProjectRegistered> {
    let client = client(&app)?;
    blocking("project_register", move || {
        // `String` at the Tauri boundary and `PathBuf` on the wire. The conversion is here
        // rather than in the signature because a `PathBuf` deserialised from JavaScript would
        // accept whatever the webview sent as a path shape; a `String` says plainly that what
        // arrives is text, and the daemon is what decides whether it names a folder.
        let request = ProjectRegister {
            path: PathBuf::from(path),
        };
        match client.request(RequestPayload::ProjectRegister(request))? {
            ResponsePayload::ProjectRegister(registered) => Ok(registered),
            other => Err(unexpected("project_register", &other)),
        }
    })
    .await
}

/// Ask the user for a folder, with the platform's own picker.
///
/// `Ok(None)` is a cancelled dialog, which is an outcome rather than a failure: the caller
/// puts the `+` button back and says nothing.
///
/// # Why the picker is here and not in the webview
///
/// The window is a client with no privileged path (D-1/D-2) and `apps/web` may not import
/// `@tauri-apps` outside its transport module, so browsing a disk is something only this
/// process can do. Driving `tauri-plugin-dialog` from Rust rather than from JavaScript also
/// keeps the plugin's own commands unreachable from the webview, which is why
/// `capabilities/default.json` grants no `dialog:` permission — there is nothing there for
/// the window to be allowed to call.
///
/// # Blocking, on the blocking pool, deliberately
///
/// `blocking_pick_folder` deadlocks when it is called *on* the main thread — it dispatches
/// the dialog there and then waits for it — so it has to run somewhere else. [`blocking`] is
/// that somewhere, and it is the same rule every other command follows rather than an
/// exception carved for this one. A modal the user leaves open for a minute is a minute this
/// thread is parked and the window is not.
///
/// # Errors
///
/// [`CommandFailure`] when the picked folder's name is not valid Unicode and therefore
/// cannot be put on the wire. The message does not repeat the name: an error envelope is the
/// thing most likely to be written to a log, and a folder name is a person's disk
/// (traps register #13).
#[tauri::command]
pub async fn project_pick_folder(app: AppHandle) -> Failed<Option<String>> {
    blocking("project_pick_folder", move || {
        let Some(picked) = app.dialog().file().blocking_pick_folder() else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|_| {
            DaemonError::Io(
                "the folder picker returned something that is not a path on this disk".to_owned(),
            )
        })?;
        path.into_os_string().into_string().map(Some).map_err(|_| {
            DaemonError::Io(
                "that folder's name is not valid Unicode, so it cannot be sent to the daemon"
                    .to_owned(),
            )
        })
    })
    .await
}

/// The events the webview is allowed to record, and the only ones.
///
/// Three, and each is something **only the webview can see**. `daemon::stream` and `channel`
/// already record the failures on their own side of the boundary — a corrupt framing, a
/// credit frame that would not parse, an incoherent grant, a frame discarded for a detached
/// stream — so this list stays at what is left after them:
///
/// - `channel_unreadable` — the one multiplexed `Channel` delivered bytes the decoder could
///   not resynchronise on. Rust wrote well-formed frames; whatever happened to them happened
///   in the delivery, where only the webview is standing.
/// - `replay_timeout` — the daemon's `replay_end` marker never arrived, so a pane opened its
///   input gate on the deadline and the keystrokes typed in the meantime are gone.
/// - `dropped_while_hidden` — a background pane's ring overflowed and its screen was reset.
///
/// **This list is the gate, and it is on this side of the boundary on purpose.** A webview
/// that could name its own event kind could put anything in one, which is precisely the
/// free-text hole trap 13 forbids; a TypeScript constant would be a suggestion, because the
/// caller and the constant live in the same process. A name that is not here is refused.
const WEBVIEW_EVENTS: &[&str] = &[
    "channel_unreadable",
    "replay_timeout",
    "dropped_while_hidden",
];

/// Record something only the webview could have seen.
///
/// The webview's whole log surface: a name from [`WEBVIEW_EVENTS`], a stream id and a count.
/// **There is no message parameter and there will not be one** — a free-text field on this
/// path is a place for a payload to land, and every candidate for one so far has been
/// expressible as a name plus a number.
///
/// Fire-and-forget by contract. The caller does not await it and must not surface a
/// rejection: a logger that can fail a command turns a diagnostic into an outage, and a
/// logger that logs its own failure recurses.
///
/// # Why not through the daemon
///
/// Because the transport events worth reading are the ones that happen when the daemon is
/// what is broken. See the `log` module for the argument in full.
///
/// # The one command with no `spawn_blocking`, and why
///
/// The rule at the top of this module sends work to the blocking pool because **the socket
/// client is blocking by design**. This command never touches the client: it compares a
/// string against a three-element list and calls a `tracing` macro. There is no blocking call
/// here to hand anywhere.
///
/// Handing it to the pool would also be the *inconsistent* choice rather than the safe one.
/// [`blocking`] writes its own lines after the `await` — on the runtime thread, not the pool
/// — so every other log line this file produces is already written there, and sending this
/// one to a pool thread would make it the only line in the file with a different writer. The
/// rule this does keep, because it is the one with teeth, is that there is no `tokio::spawn`
/// (traps register #2), and nothing here can block a webview: this is the Tauri runtime's
/// thread, not the main thread.
///
/// **If this ever grows a call that can block — a socket, a lock, a file it opens itself —
/// it moves to [`blocking`] and this paragraph goes away.**
///
/// # Errors
///
/// Never fails. An unlisted name is dropped with a warning rather than refused, because the
/// caller is not waiting for an answer and there is nobody for a rejection to reach.
#[tauri::command]
pub async fn client_log(event: String, stream: Option<u32>, count: Option<u64>) -> Failed<()> {
    let Some(known) = WEBVIEW_EVENTS.iter().find(|allowed| **allowed == event) else {
        // The rejected name is **not** echoed. Somebody who could get an arbitrary string
        // into the log by sending one that fails the check would have the hole the allowlist
        // exists to close, and "it was only in the error path" is how that hole usually gets
        // built. The list is a `const` in this file; grep is the way to find out which name
        // was wrong.
        tracing::warn!("the webview asked to log an event that is not on the allowlist");
        return Ok(());
    };
    tracing::info!(
        event = known,
        stream,
        count,
        "the webview reported a transport event"
    );
    Ok(())
}

/// A response of the wrong shape is a protocol failure, not a silent success.
fn unexpected(verb: &str, got: &ResponsePayload) -> DaemonError {
    DaemonError::Protocol(format!("{verb} was answered with a {} payload", got.verb()))
}

/// The prefixes of every permission the folder picker's plugins would grant the webview.
///
/// `tauri-plugin-dialog` brings `tauri-plugin-fs` in with it, and between them they ship
/// permission sets that let a webview open dialogs and read files straight off the disk.
/// Nysia grants neither: the picker is driven from [`project_pick_folder`] on this side of
/// the boundary, because the window is a client with no privileged path (D-1/D-2).
///
/// A Tauri v2 capability is what makes a plugin command reachable from JavaScript, so this
/// is not a style rule — a capability naming nothing from either plugin is the whole reason
/// neither is callable. Named here so [`granted_plugin_permissions`] can check it.
///
/// Test-only, because nothing at run time consults it: the capability file *is* the
/// enforcement and Tauri is what reads it, so a second copy of the list compiled into the
/// binary would be a second authority that could disagree with the first. This exists to
/// assert on the first.
#[cfg(test)]
const PICKER_PLUGIN_PREFIXES: &[&str] = &["dialog:", "fs:"];

/// Which of `permissions` a picker plugin would have granted the webview.
///
/// Split out from the test that uses it so the check can be pointed at a list that *does*
/// grant one, which is the proof that it trips (traps register #12). A check that reads the
/// real file and passes says nothing on its own about whether it could ever fail.
#[cfg(test)]
fn granted_plugin_permissions(permissions: &[String]) -> Vec<&str> {
    permissions
        .iter()
        .filter(|permission| {
            PICKER_PLUGIN_PREFIXES
                .iter()
                .any(|prefix| permission.starts_with(prefix))
        })
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nysia_proto::identity::{Incarnation, PaneKey};
    use nysia_proto::session::SessionCreated;

    use super::*;

    /// The capability file's text, embedded rather than located.
    ///
    /// `include_str!` and not a path composed at run time: traps register #9 forbids baking
    /// `CARGO_MANIFEST_DIR` into anything path-shaped, and what this asserts on is the
    /// content. The path is resolved by the compiler and nothing path-shaped reaches the
    /// binary.
    const DEFAULT_CAPABILITY: &str = include_str!("../capabilities/default.json");

    /// What `capabilities/default.json` grants the webview, in the order it lists them.
    fn granted() -> Vec<String> {
        let capability: serde_json::Value =
            serde_json::from_str(DEFAULT_CAPABILITY).expect("the capability file is JSON");
        capability["permissions"]
            .as_array()
            .expect("the capability lists permissions")
            .iter()
            .map(|permission| {
                permission
                    .as_str()
                    .expect("a permission is a string")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_folder_picker_grants_the_webview_nothing() {
        // The file is three lines from anyone adding `dialog:allow-open` to make something
        // work, and a dialog permission that also reaches the filesystem is not what the
        // picker is for. Asserted rather than described in a comment nobody runs.
        assert_eq!(
            granted_plugin_permissions(&granted()),
            Vec::<&str>::new(),
            "the picker is called from Rust; the webview needs no permission for it"
        );
    }

    #[test]
    fn the_capability_check_trips_on_a_permission_that_was_added() {
        // The proof that the assertion above is load-bearing. Without this, a
        // `PICKER_PLUGIN_PREFIXES` that had been emptied, or a filter that matched nothing,
        // would go green against the real file and catch the change it exists to catch.
        let widened = [
            "core:default".to_owned(),
            "dialog:allow-open".to_owned(),
            "fs:allow-read-text-file".to_owned(),
        ];
        assert_eq!(
            granted_plugin_permissions(&widened),
            vec!["dialog:allow-open", "fs:allow-read-text-file"]
        );
    }

    /// What a keystroke might be, and what must never reach a log file.
    const SECRET: &str = "hunter2-correct-horse-battery-staple";

    /// Everything `tracing` wrote while it was installed.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("the capture buffer")).into_owned()
        }
    }

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("the capture buffer")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `body` with `tracing` captured, and hand back everything it wrote.
    fn capturing(body: impl FnOnce()) -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::with_default(subscriber, body);
        captured.text()
    }

    fn a_handle() -> SessionHandle {
        "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
            .parse()
            .expect("a well-formed handle")
    }

    #[test]
    fn a_verbs_name_is_logged_and_its_payload_is_not() {
        // The failure this catches is one line long and would ship: somebody adds `?request`
        // or `%error` to the warning in `blocking` because a verb name alone was not enough
        // to debug something, and every password typed into a pane while the daemon was
        // refusing input lands in a file on disk (trap 13).
        //
        // The sentinel is in two places at once — in the request the command carries, and in
        // the *error message* the failure renders — so the test fails for either mistake.
        let text = capturing(|| {
            let request = TerminalSend {
                handle: a_handle(),
                text: SECRET.to_owned(),
                enter: false,
                interrupt: false,
            };
            let outcome: Failed<()> =
                tauri::async_runtime::block_on(blocking("terminal_send", move || {
                    let _ = &request;
                    Err(DaemonError::Protocol(format!(
                        "the daemon choked on {SECRET}"
                    )))
                }));
            assert!(outcome.is_err(), "the command was supposed to fail");
        });

        assert!(
            text.contains("terminal_send"),
            "the verb was not recorded at all: {text}"
        );
        assert!(
            text.contains("protocol"),
            "the failure's kind was not recorded: {text}"
        );
        assert!(
            !text.contains(SECRET),
            "a keystroke reached the log: {text}"
        );
        assert!(
            !text.contains("choked"),
            "the error's message reached the log: {text}"
        );
    }

    #[test]
    fn the_webview_may_only_log_events_this_build_names() {
        // The allowlist is the gate, and a name that is not on it is dropped *without being
        // echoed*. Echoing it would hand any caller a free-text field through the error path,
        // which is the hole the allowlist exists to close.
        let text = capturing(|| {
            let accepted = tauri::async_runtime::block_on(client_log(
                "replay_timeout".to_owned(),
                Some(4),
                Some(12),
            ));
            assert!(accepted.is_ok());

            let refused = tauri::async_runtime::block_on(client_log(
                format!("smuggled:{SECRET}"),
                None,
                None,
            ));
            assert!(
                refused.is_ok(),
                "an unlisted name must be dropped, not turned into a command failure"
            );
        });

        assert!(
            text.contains("replay_timeout"),
            "a listed event was not recorded: {text}"
        );
        assert!(
            !text.contains(SECRET),
            "an unlisted event name was echoed into the log: {text}"
        );
        assert!(
            text.contains("not on the allowlist"),
            "the refusal itself was not recorded: {text}"
        );
    }

    #[test]
    fn every_allowlisted_event_is_something_only_the_webview_can_see() {
        // Not a behaviour test — a guard on the list's size. The allowlist is the whole
        // attack surface of the webview's log path, and it grows one plausible addition at a
        // time. Anything the Rust side of the window can observe for itself belongs in
        // `daemon::stream` or `channel`, where it is logged from typed values rather than
        // from a name the webview chose.
        assert_eq!(
            WEBVIEW_EVENTS,
            [
                "channel_unreadable",
                "replay_timeout",
                "dropped_while_hidden"
            ]
        );
    }

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
