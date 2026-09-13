//! The control plane: request/response verbs as newline-delimited JSON.
//!
//! One worker thread owns the socket and serves requests in order. That is not a
//! simplification of a concurrent design — it is what the protocol asks for.
//! [`ClientRole::Control`](nysia_proto::handshake::ClientRole::Control) exists precisely so
//! that a slow terminal read cannot delay a session close: output went to its own
//! connection, and what is left here is short verbs that answer immediately. Pipelining
//! them would buy nothing and cost a correlation table.
//!
//! The thread is what keeps every Tauri command honest. A command hands a request to this
//! thread from inside `spawn_blocking` and waits for the reply; it never touches the
//! socket, never spawns anything, and cannot freeze the webview (traps register #2).

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use nysia_proto::envelope::{RequestEnvelope, RequestPayload, ResponseEnvelope, ResponsePayload};
use nysia_proto::handshake::{ClientRole, DaemonIdentity};

use super::DaemonError;
use super::endpoint::{self, Socket};

/// One queued request and where its answer goes.
struct Call {
    payload: RequestPayload,
    answer: Sender<Result<ResponsePayload, DaemonError>>,
}

/// A connected control plane.
pub struct Control {
    calls: Sender<Call>,
    worker: Option<JoinHandle<()>>,
    identity: DaemonIdentity,
}

impl Control {
    /// Connect, shake hands, and start serving.
    ///
    /// # Errors
    ///
    /// Every [`DaemonError`] the endpoint can produce: no daemon listening, a refused
    /// hello, a protocol mismatch.
    pub fn connect() -> Result<Self, DaemonError> {
        let path = endpoint::endpoint()?;
        let socket = endpoint::open(&path)?;
        let mut reader = BufReader::new(socket);
        let identity =
            endpoint::handshake(&mut reader, ClientRole::Control, &endpoint::client_id()?)?;

        let (calls, inbox) = mpsc::channel::<Call>();
        let worker = thread::Builder::new()
            .name("nysia-control".to_owned())
            .spawn(move || serve(&inbox, reader))
            .ok();

        Ok(Self {
            calls,
            worker,
            identity,
        })
    }

    /// A control plane with no socket behind it, for tests that need a `Connected` to exist.
    ///
    /// Every request it is handed fails with [`DaemonError::Disconnected`], because the
    /// worker it would queue to was never started — which is exactly what a caller should
    /// see from a connection that is not really there.
    #[cfg(test)]
    pub fn detached() -> Self {
        let (calls, inbox) = mpsc::channel::<Call>();
        drop(inbox);
        Self {
            calls,
            worker: None,
            identity: DaemonIdentity {
                pid: 0,
                started_at_ms: 0,
                launch_nonce: "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
                    .parse()
                    .expect("a well-formed nonce"),
                app_version: "0.1.0".to_owned(),
            },
        }
    }

    /// Who answered the handshake.
    pub fn identity(&self) -> &DaemonIdentity {
        &self.identity
    }

    /// Issue one verb and wait for its answer.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Disconnected`] if the worker has stopped, and whatever the daemon
    /// replied otherwise — including [`DaemonError::Daemon`], which carries the error
    /// envelope's `nextSteps` so the window can put a sentence a person can act on in front
    /// of them rather than a status code.
    pub fn request(&self, payload: RequestPayload) -> Result<ResponsePayload, DaemonError> {
        let (answer, reply) = mpsc::channel();
        self.calls
            .send(Call { payload, answer })
            .map_err(|_| DaemonError::Disconnected)?;
        reply.recv().map_err(|_| DaemonError::Disconnected)?
    }

    /// Stop the worker and close the socket.
    pub fn shutdown(&mut self) {
        // Dropping the only sender is what ends the loop; there is no separate verb for it,
        // because a control plane that needed one could be left half-closed.
        let (dead, _) = mpsc::channel();
        let live = std::mem::replace(&mut self.calls, dead);
        drop(live);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Serve calls in order until every caller has gone away or the socket dies.
fn serve(inbox: &mpsc::Receiver<Call>, mut socket: BufReader<Box<dyn Socket>>) {
    while let Ok(call) = inbox.recv() {
        let outcome = exchange(&mut socket, RequestEnvelope::new(call.payload));
        let fatal = matches!(outcome, Err(DaemonError::Io(_) | DaemonError::Protocol(_)));
        // A caller that stopped waiting is not an error: the window may have closed while a
        // verb was in flight.
        let _ = call.answer.send(outcome);
        if fatal {
            // The socket is unusable. Every caller still queued gets told, rather than
            // blocking forever on a connection that will never answer.
            while let Ok(orphan) = inbox.try_recv() {
                let _ = orphan.answer.send(Err(DaemonError::Disconnected));
            }
            return;
        }
    }
}

/// Write one request and read the response that answers it.
///
/// The envelope is a parameter rather than minted here, because `RequestId` is generated
/// and a test cannot predict one. Passing it in is what lets the correlation tests drive
/// this exact function against an id they know, instead of a second copy of it that could
/// drift from the real path without anything failing.
fn exchange(
    socket: &mut BufReader<Box<dyn Socket>>,
    request: RequestEnvelope,
) -> Result<ResponsePayload, DaemonError> {
    let line = serde_json::to_string(&request)
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    socket
        .get_mut()
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|error| DaemonError::Io(error.to_string()))?;
    socket
        .get_mut()
        .flush()
        .map_err(|error| DaemonError::Io(error.to_string()))?;

    // Answers that do not match are skipped rather than returned. On a serial plane there
    // should be none, but a daemon that ever pushes an unsolicited line must not be able to
    // hand one verb's answer to another verb's caller.
    loop {
        let mut answer = String::new();
        let read = socket
            .read_line(&mut answer)
            .map_err(|error| DaemonError::Io(error.to_string()))?;
        if read == 0 {
            return Err(DaemonError::Io(
                "the daemon closed the control socket".to_owned(),
            ));
        }

        let response: ResponseEnvelope = serde_json::from_str(answer.trim_end())
            .map_err(|error| DaemonError::Protocol(error.to_string()))?;
        if !response.answers(&request) {
            tracing::debug!(
                request = %request.request_id,
                answered = %response.request_id,
                "skipping a control line that answers a different request"
            );
            continue;
        }

        return match response.payload {
            ResponsePayload::Error(envelope) => Err(DaemonError::Daemon(Box::new(envelope))),
            other => Ok(other),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::time::Duration;

    use nysia_proto::error::{ErrorCode, ErrorEnvelope, NextSteps};
    use nysia_proto::session::SessionList;

    use super::*;

    /// A socket preloaded with the lines the daemon is pretending to send.
    struct Replay {
        answers: Cursor<Vec<u8>>,
    }

    impl Read for Replay {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.answers.read(buf)
        }
    }

    impl Socket for Replay {
        fn set_read_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Write for Replay {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Drive [`exchange`] against a daemon that answers with `answer(request_id)`.
    ///
    /// The answers are composed from the id of the envelope actually being sent, so
    /// correlation is exercised rather than assumed.
    fn run(answer: impl Fn(&str) -> Vec<String>) -> Result<ResponsePayload, DaemonError> {
        let request = RequestEnvelope::new(RequestPayload::SessionList(SessionList {}));
        let lines = answer(&request.request_id.to_string());
        let preloaded = if lines.is_empty() {
            Vec::new()
        } else {
            format!(
                "{}
",
                lines.join(
                    "
"
                )
            )
            .into_bytes()
        };

        let mut socket = BufReader::new(Box::new(Replay {
            answers: Cursor::new(preloaded),
        }) as Box<dyn Socket>);
        exchange(&mut socket, request)
    }

    fn answering(id: &str, payload: ResponsePayload) -> String {
        serde_json::to_string(&ResponseEnvelope::new(
            id.parse().expect("a generated id parses"),
            payload,
        ))
        .expect("serialisable")
    }

    #[test]
    fn a_matching_answer_comes_back_to_its_caller() {
        let answer = run(|id| {
            vec![answering(
                id,
                ResponsePayload::SessionList {
                    sessions: Vec::new(),
                },
            )]
        })
        .expect("the daemon answered");
        assert!(matches!(answer, ResponsePayload::SessionList { .. }));
    }

    #[test]
    fn a_daemon_error_envelope_reaches_the_caller_with_its_next_steps() {
        // The whole point of the envelope: the user is shown something actionable. A client
        // that flattened this to "request failed" would throw away the one useful part.
        let error = run(|id| {
            vec![answering(
                id,
                ResponsePayload::Error(ErrorEnvelope::new(
                    ErrorCode::SpawnFailed,
                    "pwsh is not on PATH",
                    NextSteps::new("Install PowerShell 7, or pick cmd from the + menu.")
                        .expect("a non-empty step"),
                )),
            )]
        })
        .expect_err("the daemon refused");

        match error {
            DaemonError::Daemon(returned) => {
                assert_eq!(returned.message(), "pwsh is not on PATH");
                assert!(!returned.next_steps().is_empty(), "next steps must survive");
            }
            other => panic!("expected the daemon's envelope, got {other:?}"),
        }
    }

    #[test]
    fn a_line_answering_a_different_request_is_skipped_not_mistaken_for_the_answer() {
        // The failure this prevents: verb A's caller returning verb B's result. Rare on a
        // serial plane, silent when it happens, and wrong in a way nothing downstream can
        // detect.
        let answer = run(|id| {
            vec![
                answering(
                    "req_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
                    ResponsePayload::SessionClose,
                ),
                answering(
                    id,
                    ResponsePayload::SessionList {
                        sessions: Vec::new(),
                    },
                ),
            ]
        })
        .expect("the matching answer arrives second");

        assert!(matches!(answer, ResponsePayload::SessionList { .. }));
    }

    #[test]
    fn a_socket_that_closes_before_answering_is_reported_rather_than_waited_on() {
        let error = run(|_| Vec::new()).expect_err("nothing came back");
        assert!(matches!(error, DaemonError::Io(_)), "got {error:?}");
    }

    #[test]
    fn a_garbled_answer_is_a_protocol_failure() {
        let error = run(|_| vec!["not json at all".to_owned()]).expect_err("that does not parse");
        assert!(matches!(error, DaemonError::Protocol(_)), "got {error:?}");
    }
}
