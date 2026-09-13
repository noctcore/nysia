//! The other half of the socket: what the CLI and the Tauri shell both use.
//!
//! There is one client implementation, in `nysia-core`, and both consumers link it. That is
//! not tidiness — it is D-1 enforced in code. If the window had its own way in, "the window
//! has no privileged path" would be a claim about discipline rather than a property of the
//! program, and the first feature that was easier to add on the privileged side would end it.
//!
//! # What `connect` does that a socket call does not
//!
//! 1. Dials the endpoint, which both halves resolve through the same [`Endpoint`].
//! 2. Sends `hello` and reads the answer, so the connection either has an agreed protocol
//!    version and a known daemon identity, or does not exist.
//! 3. Turns a rejection into an error that says whether to back off or to die — the daemon
//!    carries `retryable` on the wire precisely so a client does not have to understand the
//!    reason taxonomy to act on it.
//!
//! Every verb below checks that the answer's tag matches the question's. A daemon that
//! answered `session_list` to a `terminal_read` would be a serious bug, and finding it here is
//! better than handing the caller a shape that happens to deserialise.

use nysia_proto::{
    ClientId, ClientRole, DaemonIdentity, ErrorCode, ErrorEnvelope, HelloRequest, HelloResponse,
    PROTOCOL_VERSION, RejectReason, RequestEnvelope, RequestId, RequestPayload, ResponseEnvelope,
    ResponsePayload, SessionClose, SessionCreate, SessionCreated, SessionHandle, SessionList,
    SessionSummary, StreamAttach, StreamAttached, StreamDetach, StreamId, TerminalRead,
    TerminalReadResult, TerminalResize, TerminalSend, TerminalWait, TerminalWaitResult,
};

use crate::rpc::control::{ControlError, ControlReader, ControlWriter};
use crate::rpc::endpoint::Endpoint;
use crate::rpc::errors::envelope;
use crate::rpc::transport::{ConnectionReader, ConnectionWriter, TransportError};

/// Why a client call failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The endpoint could not be resolved.
    #[error(transparent)]
    Endpoint(#[from] crate::rpc::endpoint::EndpointResolveError),
    /// The connection could not be made or was lost.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// A frame could not be read or written.
    #[error(transparent)]
    Control(#[from] ControlError),
    /// The daemon refused the handshake.
    #[error("the daemon refused the connection: {reason:?}")]
    Rejected {
        /// Why it refused.
        reason: RejectReason,
        /// Whether backing off and reconnecting could succeed.
        retryable: bool,
    },
    /// The daemon closed the connection without answering.
    #[error("the daemon closed the connection without answering")]
    NoAnswer,
    /// The daemon answered something other than what was asked.
    #[error("asked for {asked} and the daemon answered {answered}")]
    Mismatched {
        /// The verb that was sent.
        asked: &'static str,
        /// The verb that came back.
        answered: &'static str,
    },
    /// The verb itself failed, and the daemon said what to do about it.
    #[error("{0}")]
    Verb(ErrorEnvelope),
}

impl ClientError {
    /// The error as an envelope, so every failure a caller can see carries next steps.
    ///
    /// §6.2 makes next steps non-optional for the daemon's answers. A client-side failure is
    /// no less in need of them — "connection refused" with nothing after it is exactly the
    /// message that has an agent inventing flags.
    #[must_use]
    pub fn envelope(&self) -> ErrorEnvelope {
        match self {
            Self::Verb(envelope) => envelope.clone(),
            Self::Endpoint(_) => envelope(
                ErrorCode::Internal,
                self.to_string(),
                "set NYSIA_RUNTIME_DIR to an absolute directory you own",
                &["`nysia --daemon` prints the endpoint it resolved on startup"],
            ),
            Self::Transport(TransportError::NotListening { .. }) => envelope(
                ErrorCode::Internal,
                self.to_string(),
                "start the daemon with `nysia --daemon`",
                &["most verbs start one for you; this one was told not to"],
            )
            .retryable(true)
            .with_next_command_args(["nysia", "--daemon"]),
            Self::Transport(_) | Self::Control(_) | Self::NoAnswer => envelope(
                ErrorCode::Internal,
                self.to_string(),
                "retry the command",
                &[
                    "if it keeps failing, the daemon may have died; `nysia session list` will \
                     start a fresh one",
                ],
            )
            .retryable(true),
            Self::Rejected { reason, retryable } => envelope(
                ErrorCode::Unsupported,
                self.to_string(),
                match reason {
                    RejectReason::UnsupportedVersion { .. } => {
                        "this build and the running daemon speak different protocols; stop the \
                         daemon and let this build start one"
                    }
                    RejectReason::Unauthorized { .. } => {
                        "the daemon serves one account; run as the user that started it"
                    }
                    RejectReason::ShuttingDown => "the daemon is retiring; retry in a moment",
                    RejectReason::Malformed { .. } | RejectReason::Unknown => {
                        "upgrade the client, the daemon, or both so they agree on the protocol"
                    }
                },
                &["`nysia session list` shows which daemon answered"],
            )
            .retryable(*retryable),
            Self::Mismatched { .. } => envelope(
                ErrorCode::Internal,
                self.to_string(),
                "report this: a daemon answering the wrong verb is a bug in Nysia",
                &["the client and the daemon may be from incompatible builds"],
            ),
        }
    }
}

/// A connected, handshaken client.
pub struct Client {
    reader: ControlReader<ConnectionReader>,
    writer: ControlWriter<ConnectionWriter>,
    identity: DaemonIdentity,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("daemon", &self.identity)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Dial `endpoint` and complete the handshake.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] when nothing is listening, and
    /// [`ClientError::Rejected`] — carrying the daemon's own `retryable` — when the daemon
    /// refuses.
    pub async fn connect(
        endpoint: &Endpoint,
        client_id: &ClientId,
        role: ClientRole,
    ) -> Result<Self, ClientError> {
        let connection = crate::rpc::transport::connect(endpoint).await?;
        let (reader, writer) = connection.split();
        let mut reader = ControlReader::new(reader);
        let mut writer = ControlWriter::new(writer);

        writer
            .write_frame(&HelloRequest::new(
                PROTOCOL_VERSION,
                role,
                client_id.clone(),
            ))
            .await?;
        match reader.read_frame::<HelloResponse>().await? {
            Some(HelloResponse::Accepted(accepted)) => Ok(Self {
                reader,
                writer,
                identity: accepted.daemon_identity,
            }),
            Some(HelloResponse::Rejected(rejected)) => Err(ClientError::Rejected {
                // `retryable` is taken from the wire rather than re-derived from the reason:
                // a daemon newer than this build may refuse for a reason this build cannot
                // name, and the bit it *can* read is the one it branches on.
                retryable: rejected.retryable,
                reason: rejected.reason,
            }),
            None => Err(ClientError::NoAnswer),
        }
    }

    /// Who answered.
    #[must_use]
    pub fn identity(&self) -> &DaemonIdentity {
        &self.identity
    }

    /// Take the halves apart, for a stream-role connection that speaks binary from here on.
    #[must_use]
    pub fn into_stream_parts(self) -> (Vec<u8>, ConnectionReader, ConnectionWriter) {
        let (leftover, reader) = self.reader.into_parts();
        (leftover, reader, self.writer.into_inner())
    }

    /// Send one verb and read its answer.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Verb`] when the daemon reports the verb failed — carrying the
    /// envelope with its next steps — and the transport variants when the connection does.
    pub async fn request(
        &mut self,
        payload: RequestPayload,
    ) -> Result<ResponsePayload, ClientError> {
        self.send_request(RequestEnvelope::new(payload)).await
    }

    /// Send a verb as a retry of `original`, so the daemon replays rather than repeats.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn retry(
        &mut self,
        payload: RequestPayload,
        original: RequestId,
    ) -> Result<ResponsePayload, ClientError> {
        self.send_request(RequestEnvelope::retrying(payload, original))
            .await
    }

    /// Write an envelope and read the answer that correlates with it.
    async fn send_request(
        &mut self,
        request: RequestEnvelope,
    ) -> Result<ResponsePayload, ClientError> {
        let asked = request.payload.verb();
        let request_id = request.request_id.clone();
        self.writer.write_frame(&request).await?;

        let response: ResponseEnvelope = self
            .reader
            .read_frame()
            .await?
            .ok_or(ClientError::NoAnswer)?;
        if response.request_id != request_id {
            return Err(ClientError::Mismatched {
                asked,
                answered: response.payload.verb(),
            });
        }
        match response.payload {
            ResponsePayload::Error(envelope) => Err(ClientError::Verb(envelope)),
            payload if payload.verb() == asked => Ok(payload),
            payload => Err(ClientError::Mismatched {
                asked,
                answered: payload.verb(),
            }),
        }
    }

    /// Spawn a session.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn session_create(
        &mut self,
        request: SessionCreate,
    ) -> Result<SessionCreated, ClientError> {
        match self.request(RequestPayload::SessionCreate(request)).await? {
            ResponsePayload::SessionCreate(created) => Ok(created),
            other => Err(mismatched("session_create", &other)),
        }
    }

    /// Every session the daemon owns.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn session_list(&mut self) -> Result<Vec<SessionSummary>, ClientError> {
        match self
            .request(RequestPayload::SessionList(SessionList {}))
            .await?
        {
            ResponsePayload::SessionList { sessions } => Ok(sessions),
            other => Err(mismatched("session_list", &other)),
        }
    }

    /// Close a session and tear down its process tree.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn session_close(&mut self, handle: SessionHandle) -> Result<(), ClientError> {
        match self
            .request(RequestPayload::SessionClose(SessionClose { handle }))
            .await?
        {
            ResponsePayload::SessionClose => Ok(()),
            other => Err(mismatched("session_close", &other)),
        }
    }

    /// Read the rendered screen, or the scrollback from a cursor.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn terminal_read(
        &mut self,
        request: TerminalRead,
    ) -> Result<TerminalReadResult, ClientError> {
        match self.request(RequestPayload::TerminalRead(request)).await? {
            ResponsePayload::TerminalRead(result) => Ok(result),
            other => Err(mismatched("terminal_read", &other)),
        }
    }

    /// Write to a session's input.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn terminal_send(&mut self, request: TerminalSend) -> Result<(), ClientError> {
        match self.request(RequestPayload::TerminalSend(request)).await? {
            ResponsePayload::TerminalSend => Ok(()),
            other => Err(mismatched("terminal_send", &other)),
        }
    }

    /// Change a session's viewport size.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn terminal_resize(&mut self, request: TerminalResize) -> Result<(), ClientError> {
        match self
            .request(RequestPayload::TerminalResize(request))
            .await?
        {
            ResponsePayload::TerminalResize => Ok(()),
            other => Err(mismatched("terminal_resize", &other)),
        }
    }

    /// Block until a session exits or goes idle.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn terminal_wait(
        &mut self,
        request: TerminalWait,
    ) -> Result<TerminalWaitResult, ClientError> {
        match self.request(RequestPayload::TerminalWait(request)).await? {
            ResponsePayload::TerminalWait(result) => Ok(result),
            other => Err(mismatched("terminal_wait", &other)),
        }
    }

    /// Start routing a session's output to this client's stream connection.
    ///
    /// # Errors
    ///
    /// As [`Client::request`]. Attaching without a stream connection bound under the same
    /// client id is refused, with next steps saying to open one.
    pub async fn stream_attach(
        &mut self,
        handle: SessionHandle,
    ) -> Result<StreamAttached, ClientError> {
        match self
            .request(RequestPayload::StreamAttach(StreamAttach { handle }))
            .await?
        {
            ResponsePayload::StreamAttach(attached) => Ok(attached),
            other => Err(mismatched("stream_attach", &other)),
        }
    }

    /// Stop routing a stream.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub async fn stream_detach(&mut self, stream_id: StreamId) -> Result<(), ClientError> {
        match self
            .request(RequestPayload::StreamDetach(StreamDetach { stream_id }))
            .await?
        {
            ResponsePayload::StreamDetach => Ok(()),
            other => Err(mismatched("stream_detach", &other)),
        }
    }
}

/// The error for an answer whose tag did not match the question's.
fn mismatched(asked: &'static str, answered: &ResponsePayload) -> ClientError {
    ClientError::Mismatched {
        asked,
        answered: answered.verb(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::lease::PidRecordFile;
    use crate::rpc::server::{Daemon, DaemonConfig};
    use nysia_proto::{
        CreditAck, CreditFrame, Frame, FrameDecoder, FrameKind, LineCursor, ReadMode, SessionKind,
        ShellProfile as WireProfile, WaitFor, WaitOutcome,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Every wait in these tests is bounded; a test that can hang is a test that will.
    const DEADLINE: Duration = Duration::from_secs(30);

    /// The same deadline, as the wire spells one.
    const DEADLINE_MS: u64 = 30_000;

    /// A daemon serving on its own endpoint, torn down with the harness.
    struct Harness {
        daemon: Arc<Daemon>,
        endpoint: Endpoint,
        serving: tokio::task::JoinHandle<()>,
        runtime_dir: PathBuf,
    }

    impl Harness {
        /// Start a daemon that never retires, so a test's own pauses cannot kill it.
        fn start(tag: &str) -> Self {
            let endpoint = crate::rpc::endpoint::scratch(tag);
            let runtime_dir = endpoint.runtime_dir().to_path_buf();

            let (daemon, listener) = Daemon::bind(DaemonConfig {
                idle_retire_after: None,
                ..DaemonConfig::new(endpoint.clone())
            })
            .expect("the daemon binds");
            let serving = tokio::spawn({
                let daemon = Arc::clone(&daemon);
                // The real accept loop, not a stand-in. `idle_retire_after: None` is what
                // keeps a test's own pauses from looking like an idle daemon.
                async move {
                    let _ = daemon.serve(listener).await;
                }
            });
            Self {
                daemon,
                endpoint,
                serving,
                runtime_dir,
            }
        }

        async fn client(&self, id: &str) -> Client {
            Client::connect(&self.endpoint, &client_id(id), ClientRole::Control)
                .await
                .expect("a client connects")
        }

        async fn stream_client(&self, id: &str) -> Client {
            Client::connect(&self.endpoint, &client_id(id), ClientRole::Stream)
                .await
                .expect("a stream client connects")
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.serving.abort();
            self.daemon.shutdown();
            let _ = std::fs::remove_dir_all(&self.runtime_dir);
        }
    }

    fn client_id(id: &str) -> ClientId {
        id.parse().expect("a well-formed client id")
    }

    /// The shell these tests drive, and the line that makes it compute a token.
    ///
    /// `pwsh` where it is installed, which is what §9 names and what CI has; the platform's
    /// own shell where it is not, so a machine without PowerShell 7 still runs the test.
    fn shell() -> (Option<WireProfile>, Vec<&'static str>) {
        if crate::pty::resolve("pwsh").is_ok() {
            return (
                Some(WireProfile::Pwsh),
                vec![r#"Write-Output ("NYSIA" + "-" + (6*7))"#],
            );
        }
        if cfg!(windows) {
            return (
                Some(WireProfile::Cmd),
                vec!["set /a NYS=6*7", "echo NYSIA-%NYS%"],
            );
        }
        (None, vec![r#"echo "NYSIA-$((6*7))""#])
    }

    /// A shell that never stops writing, for the backpressure proof.
    fn flood() -> &'static str {
        if crate::pty::resolve("pwsh").is_ok() {
            r#"while ($true) { Write-Output ("x" * 120) }"#
        } else if cfg!(windows) {
            "for /l %i in (1,1,100000000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        } else {
            "yes xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        }
    }

    /// The token the chosen shell computes. It appears in no line that is typed.
    const TOKEN: &str = "NYSIA-42";

    fn create_request() -> SessionCreate {
        SessionCreate {
            kind: SessionKind::Shell,
            pane_key: None,
            profile: shell().0,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 100,
            rows: 30,
        }
    }

    /// Attach a stream, waiting for the stream connection's own handshake to land.
    ///
    /// The two connections are independent: `Client::connect` returns once the daemon has
    /// answered its `hello`, which is a moment before the daemon starts serving that
    /// connection and registers its outbox. Retrying is the client-side half of the contract
    /// the refusal states — it is marked retryable precisely because this race is normal.
    async fn attach(client: &mut Client, handle: &SessionHandle) -> StreamAttached {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match client.stream_attach(handle.clone()).await {
                Ok(attached) => return attached,
                Err(err) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = err;
                }
                Err(err) => panic!("the stream never attached: {err}"),
            }
        }
    }

    /// Wait for the shell to stop producing output.
    ///
    /// Used between typed lines rather than waiting for the token: only the last line
    /// produces it, so waiting for it after the first would burn the whole budget on a
    /// condition that cannot be true yet.
    async fn settle(client: &mut Client, handle: &SessionHandle) {
        let _ = client
            .terminal_wait(TerminalWait {
                handle: handle.clone(),
                wait_for: WaitFor::Idle,
                timeout_ms: Some(DEADLINE_MS),
            })
            .await;
    }

    /// Read the screen until `predicate` holds, or until the deadline.
    async fn until(
        client: &mut Client,
        handle: &SessionHandle,
        predicate: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let read = client
                .terminal_read(TerminalRead::screen(handle.clone()))
                .await
                .expect("a read is answered");
            let text = read.lines.join("\n");
            if predicate(&text) || Instant::now() >= deadline {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    #[tokio::test]
    async fn the_handshake_names_a_daemon_the_lease_beside_it_describes() {
        let harness = Harness::start("identity");
        let client = harness.client("window").await;
        let record = PidRecordFile::at(harness.endpoint.pid_record_path())
            .read()
            .expect("the lease reads")
            .expect("the lease is there");
        assert!(
            record.describes(client.identity()),
            "the daemon that answered must be the one the lease describes"
        );
        assert_eq!(client.identity().pid, std::process::id());
    }

    #[tokio::test]
    async fn the_verb_surface_round_trips_over_the_socket() {
        let harness = Harness::start("verbs");
        let mut client = harness.client("window").await;

        let created = client
            .session_create(create_request())
            .await
            .expect("a session is created");
        let listed = client.session_list().await.expect("the list is answered");
        assert!(listed.iter().any(|row| row.handle == created.handle));
        assert!(
            listed.iter().all(|row| row.exit_status.is_none()),
            "a session that has just started has not exited"
        );

        until(&mut client, &created.handle, |text| !text.trim().is_empty()).await;
        client
            .terminal_resize(TerminalResize {
                handle: created.handle.clone(),
                cols: 120,
                rows: 40,
            })
            .await
            .expect("a resize is taken");

        for line in shell().1 {
            client
                .terminal_send(TerminalSend::line(created.handle.clone(), line))
                .await
                .expect("input is written");
            settle(&mut client, &created.handle).await;
        }
        let screen = until(&mut client, &created.handle, |text| text.contains(TOKEN)).await;
        assert!(screen.contains(TOKEN), "got {screen:?}");

        // Screen is the default and says so in its own answer; stream pages the scrollback.
        let scrollback = client
            .terminal_read(TerminalRead::stream(
                created.handle.clone(),
                LineCursor::START,
            ))
            .await
            .expect("a stream read is answered");
        assert_eq!(scrollback.mode, ReadMode::Stream);
        assert!(
            scrollback.lines.iter().any(|line| line.contains(TOKEN)),
            "got {:?}",
            scrollback.lines
        );

        client
            .session_close(created.handle.clone())
            .await
            .expect("the session closes");
        assert!(
            client
                .session_list()
                .await
                .expect("the list is answered")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_session_outlives_every_client_that_was_watching_it() {
        // D-1, at the library level. The CLI-only proof lives in the `nysia` crate's
        // integration test; this one fails faster and points at the layer that broke.
        let harness = Harness::start("survival");
        let mut first = harness.client("the-window").await;
        let created = first
            .session_create(create_request())
            .await
            .expect("a session is created");
        until(&mut first, &created.handle, |text| !text.trim().is_empty()).await;
        for line in shell().1 {
            first
                .terminal_send(TerminalSend::line(created.handle.clone(), line))
                .await
                .expect("input is written");
            settle(&mut first, &created.handle).await;
        }

        // The window dies. Every connection it held goes with it.
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut second = harness.client("a-new-window").await;
        let screen = until(&mut second, &created.handle, |text| text.contains(TOKEN)).await;
        assert!(
            screen.contains(TOKEN),
            "the session should have survived losing every client, got {screen:?}"
        );
        let scrollback = second
            .terminal_read(TerminalRead::stream(
                created.handle.clone(),
                LineCursor::START,
            ))
            .await
            .expect("a stream read is answered");
        assert!(
            scrollback.lines.iter().any(|line| line.contains(TOKEN)),
            "and so should its scrollback, got {:?}",
            scrollback.lines
        );

        // And the session is still writable, not merely readable: it is live, not a corpse.
        second
            .terminal_send(TerminalSend::line(created.handle.clone(), "exit 3"))
            .await
            .expect("input is written");
        let result = second
            .terminal_wait(TerminalWait {
                handle: created.handle.clone(),
                wait_for: WaitFor::Exit,
                timeout_ms: Some(DEADLINE_MS),
            })
            .await
            .expect("the wait is answered");
        assert!(matches!(result.outcome, WaitOutcome::Exited { .. }));
        second
            .session_close(created.handle)
            .await
            .expect("the session closes");
    }

    #[tokio::test]
    async fn a_retry_replays_the_first_answer_rather_than_doing_the_work_twice() {
        // The one thing a caller cannot work out for itself: "the session was never created"
        // and "it was created and the reply was lost" need opposite responses.
        let harness = Harness::start("retry");
        let mut client = harness.client("window").await;

        let request = RequestPayload::SessionCreate(create_request());
        let envelope = RequestEnvelope::new(request.clone());
        let original = envelope.request_id.clone();
        client
            .writer
            .write_frame(&envelope)
            .await
            .expect("the request is written");
        let first: ResponseEnvelope = client
            .reader
            .read_frame()
            .await
            .expect("an answer arrives")
            .expect("an answer arrives");
        let receipt = first.receipt.expect("a mutation answers with a receipt");
        assert!(!receipt.replayed, "the first attempt is not a replay");

        let replayed = client
            .retry(request, original.clone())
            .await
            .expect("the retry is answered");
        assert_eq!(replayed, first.payload, "the stored answer comes back");
        assert_eq!(
            client
                .session_list()
                .await
                .expect("the list is answered")
                .len(),
            1,
            "a retried create must leave one session, not two"
        );

        let ResponsePayload::SessionCreate(created) = replayed else {
            panic!("a create answers with the ids it minted");
        };
        client
            .session_close(created.handle)
            .await
            .expect("the session closes");
    }

    #[tokio::test]
    async fn a_verb_naming_nothing_fails_with_something_to_do_about_it() {
        let harness = Harness::start("unknown");
        let mut client = harness.client("window").await;
        let handle: SessionHandle = "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
            .parse()
            .expect("a well-formed handle");

        let err = client
            .terminal_read(TerminalRead::screen(handle))
            .await
            .expect_err("no such session");
        let ClientError::Verb(envelope) = &err else {
            panic!("expected the verb to fail, got {err:?}");
        };
        assert_eq!(*envelope.code(), ErrorCode::UnknownSession);
        assert!(!envelope.next_steps().is_empty());
        assert!(
            envelope.next_command_args().is_some(),
            "§6.2 wants the argv of the obvious next command, not only prose"
        );
    }

    #[tokio::test]
    async fn attaching_without_a_stream_connection_says_to_open_one() {
        // Refusing beats queueing: a sink with nowhere to write fills, runs out of credit and
        // stalls the session, so a client that attached too early would freeze its own pane.
        let harness = Harness::start("attach-early");
        let mut client = harness.client("window").await;
        let created = client
            .session_create(create_request())
            .await
            .expect("a session is created");

        let err = client
            .stream_attach(created.handle.clone())
            .await
            .expect_err("there is no stream connection to route to");
        let ClientError::Verb(envelope) = &err else {
            panic!("expected the verb to fail, got {err:?}");
        };
        assert!(
            envelope.is_retryable(),
            "opening the connection would fix it"
        );
        assert!(
            envelope
                .next_steps()
                .iter()
                .any(|step| step.contains("stream")),
            "got {:?}",
            envelope.next_steps()
        );
        client
            .session_close(created.handle)
            .await
            .expect("the session closes");
    }

    #[tokio::test]
    async fn output_reaches_a_stream_connection_as_binary_frames() {
        let harness = Harness::start("stream");
        let stream = harness.stream_client("painter").await;
        let (leftover, mut reader, _writer) = stream.into_stream_parts();
        let mut client = harness.client("painter").await;

        let created = client
            .session_create(create_request())
            .await
            .expect("a session is created");
        let attached = attach(&mut client, &created.handle).await;

        let mut decoder = FrameDecoder::new();
        decoder.push(&leftover);
        let mut seen_grant = false;
        let mut output = Vec::new();
        let deadline = Instant::now() + DEADLINE;
        let mut buffer = [0u8; 8192];

        while Instant::now() < deadline && !(seen_grant && !output.is_empty()) {
            let Ok(Ok(read)) =
                tokio::time::timeout(Duration::from_millis(500), reader.read(&mut buffer)).await
            else {
                continue;
            };
            if read == 0 {
                break;
            }
            decoder.push(&buffer[..read]);
            while let Ok(Some(frame)) = decoder.next_frame() {
                assert_eq!(
                    frame.stream, attached.stream_id,
                    "frames carry their own id"
                );
                match frame.kind {
                    FrameKind::Credit => {
                        // The opening grant carries the window, so the client never holds its
                        // own copy of the constants.
                        let credit: CreditFrame =
                            serde_json::from_slice(&frame.payload).expect("a credit frame reads");
                        assert!(matches!(credit, CreditFrame::Grant(_)));
                        seen_grant = true;
                    }
                    FrameKind::Output => output.extend_from_slice(&frame.payload),
                    _ => {}
                }
            }
        }
        assert!(seen_grant, "an attach opens with a credit grant");
        assert!(!output.is_empty(), "a shell paints something");

        client
            .session_close(created.handle)
            .await
            .expect("the session closes");
    }

    #[tokio::test]
    async fn a_client_that_never_acks_stops_the_daemon_reading_rather_than_filling_its_memory() {
        // The `yes` flood, and the complete answer to it. A client that takes frames and never
        // acks spends its allowance and stops; the daemon then reads nothing, the kernel pty
        // buffer fills, and the child blocks in `write`. What must *not* happen is the daemon
        // absorbing an unbounded flood on the client's behalf and calling it flow control.
        let harness = Harness::start("backpressure");
        let stream = harness.stream_client("greedy").await;
        let (leftover, mut reader, mut writer) = stream.into_stream_parts();
        let mut client = harness.client("greedy").await;

        let created = client
            .session_create(create_request())
            .await
            .expect("a session is created");
        until(&mut client, &created.handle, |text| !text.trim().is_empty()).await;
        attach(&mut client, &created.handle).await;
        client
            .terminal_send(TerminalSend::line(created.handle.clone(), flood()))
            .await
            .expect("the flood starts");

        let window = nysia_proto::CreditWindow::DEFAULT;
        let mut decoder = FrameDecoder::new();
        decoder.push(&leftover);
        let mut delivered = 0usize;
        let mut buffer = [0u8; 16384];
        // Long enough that an unbounded daemon would have shipped many megabytes.
        let until_when = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until_when {
            let Ok(Ok(read)) =
                tokio::time::timeout(Duration::from_millis(250), reader.read(&mut buffer)).await
            else {
                continue;
            };
            if read == 0 {
                break;
            }
            decoder.push(&buffer[..read]);
            while let Ok(Some(frame)) = decoder.next_frame() {
                if frame.kind == FrameKind::Output {
                    delivered += frame.encoded_len();
                }
            }
        }

        // The allowance plus one frame of slack: the sink refuses only once the allowance has
        // gone, so the frame that crosses zero is delivered whole.
        let ceiling = window.per_stream_initial as usize + window.chunk as usize;
        assert!(
            delivered <= ceiling,
            "an unacked client received {delivered} bytes against a {ceiling}-byte allowance; \
             the daemon is buffering the flood instead of stopping"
        );

        // And the daemon is still serving — the stall is one session's, not the runtime's.
        assert_eq!(
            client
                .session_list()
                .await
                .expect("the daemon still answers")
                .len(),
            1
        );

        // One ack lets it go again, which is what makes the stall temporary rather than fatal.
        let ack = Frame::new(
            FrameKind::Credit,
            nysia_proto::StreamId::FIRST,
            serde_json::to_vec(&CreditFrame::Ack(CreditAck {
                bytes: window.ack_batch,
            }))
            .expect("a credit frame encodes"),
        );
        writer
            .write_all(&nysia_proto::encode(&ack).expect("the frame encodes"))
            .await
            .expect("the ack is written");
        writer.flush().await.expect("the ack is flushed");

        let mut more = 0usize;
        let until_when = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until_when && more == 0 {
            let Ok(Ok(read)) =
                tokio::time::timeout(Duration::from_millis(250), reader.read(&mut buffer)).await
            else {
                continue;
            };
            decoder.push(&buffer[..read]);
            while let Ok(Some(frame)) = decoder.next_frame() {
                if frame.kind == FrameKind::Output {
                    more += frame.payload.len();
                }
            }
        }
        assert!(more > 0, "an ack should have let the daemon read again");

        client
            .session_close(created.handle)
            .await
            .expect("the session closes");
    }
}
