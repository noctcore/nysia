//! Finding the daemon's socket and completing the `hello` handshake.
//!
//! Blocking IO on purpose. Every Tauri command reaches this through `spawn_blocking`
//! (traps register #2), so a blocking socket is the *simple* choice here rather than the
//! naive one: there is no runtime to starve, no `tokio::spawn` inside a command waiting to
//! turn a panic into `abort()`, and the reader threads are exactly the shape a PTY stream
//! wants anyway.
//!
//! This is a **client**. Under D-1/D-2 the daemon owns the sessions, the PTYs and the
//! store; the window holds a socket and nothing else. Nothing in this module may outlive
//! the daemon or be authoritative about anything it says.

use std::io::{BufRead, BufReader, Read, Write};

use nysia_proto::handshake::{
    ClientId, ClientRole, DaemonIdentity, HelloRequest, HelloResponse, RejectReason,
};
use nysia_proto::version::{PROTOCOL_VERSION, ProtocolRange};

use super::DaemonError;

/// A duplex byte stream to the daemon.
///
/// Boxed rather than generic because the two platforms produce different concrete types —
/// a named pipe opened as a file on Windows, a `UnixStream` elsewhere — and every layer
/// above only ever reads and writes. Making the whole client generic over the socket would
/// spread `#[cfg]` through code that has no platform opinion.
pub trait Socket: Read + Write + Send {}
impl<T: Read + Write + Send> Socket for T {}

/// Where this build expects to find the daemon.
///
/// Composed by `nysia-proto`, which is the sole authority on the endpoint's spelling, so
/// that the window and the CLI cannot disagree about which pipe is v1's.
///
/// # Errors
///
/// [`DaemonError::Endpoint`] when the account name cannot appear in a pipe name — see
/// [`nysia_proto::version::windows_pipe_name`].
pub fn endpoint() -> Result<String, DaemonError> {
    #[cfg(windows)]
    {
        // `GetUserNameEx` can hand back `DOMAIN\user`, which proto refuses outright rather
        // than splicing into a different pipe namespace. USERNAME is the account half.
        let account = std::env::var("USERNAME").unwrap_or_default();
        nysia_proto::version::windows_pipe_name(PROTOCOL_VERSION, &account)
            .map_err(|error| DaemonError::Endpoint(error.to_string()))
    }
    #[cfg(not(windows))]
    {
        // The daemon owns the directory, because that is where the owner-only permissions
        // live (§7.3, traps register #14 — scrollback can contain secrets). The client
        // only composes the same path it would have chosen.
        let base = std::env::var("XDG_RUNTIME_DIR")
            .or_else(|_| std::env::var("TMPDIR"))
            .unwrap_or_else(|_| "/tmp".to_owned());
        let name = nysia_proto::version::unix_socket_file_name(PROTOCOL_VERSION);
        Ok(format!("{}/{name}", base.trim_end_matches('/')))
    }
}

/// Open a socket to the daemon at `path`.
///
/// # Errors
///
/// [`DaemonError::Unreachable`] when nothing is listening — which is the ordinary case on a
/// machine where the daemon has not been started, not a fault.
pub fn open(path: &str) -> Result<Box<dyn Socket>, DaemonError> {
    #[cfg(windows)]
    {
        // A Win32 named pipe is opened like a file. `read(true).write(true)` is what makes
        // it the duplex handle the protocol needs rather than a one-way reader.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(|pipe| Box::new(pipe) as Box<dyn Socket>)
            .map_err(|error| DaemonError::Unreachable {
                endpoint: path.to_owned(),
                cause: error.to_string(),
            })
    }
    #[cfg(not(windows))]
    {
        std::os::unix::net::UnixStream::connect(path)
            .map(|socket| Box::new(socket) as Box<dyn Socket>)
            .map_err(|error| DaemonError::Unreachable {
                endpoint: path.to_owned(),
                cause: error.to_string(),
            })
    }
}

/// Send `hello` and read the daemon's answer.
///
/// The version check is deliberately on the *client*: a daemon outside
/// [`ProtocolRange::attachable`] must be refused rather than talked to, because a
/// mismatched frame layout would corrupt sessions that daemon is still serving for someone
/// else's window.
///
/// # Errors
///
/// [`DaemonError::Io`] if the socket dies mid-handshake, [`DaemonError::Protocol`] if the
/// answer does not parse, and [`DaemonError::Refused`] if the daemon said no or is outside
/// the attachable range.
pub fn handshake(
    socket: &mut BufReader<Box<dyn Socket>>,
    role: ClientRole,
    client_id: &ClientId,
) -> Result<DaemonIdentity, DaemonError> {
    let hello = HelloRequest::new(PROTOCOL_VERSION, role, client_id.clone());
    let line =
        serde_json::to_string(&hello).map_err(|error| DaemonError::Protocol(error.to_string()))?;

    socket
        .get_mut()
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|error| DaemonError::Io(error.to_string()))?;
    socket
        .get_mut()
        .flush()
        .map_err(|error| DaemonError::Io(error.to_string()))?;

    let mut answer = String::new();
    let read = socket
        .read_line(&mut answer)
        .map_err(|error| DaemonError::Io(error.to_string()))?;
    if read == 0 {
        return Err(DaemonError::Io(
            "the daemon closed the socket during hello".to_owned(),
        ));
    }

    let response: HelloResponse = serde_json::from_str(answer.trim_end())
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    match response {
        HelloResponse::Rejected(rejected) => Err(DaemonError::Refused {
            reason: describe(&rejected.reason),
            // Proto owns the reason taxonomy, so proto decides whether another attempt
            // could work. A retiring daemon is replaced in a moment; giving up on it would
            // make the user restart the window for nothing.
            retryable: rejected.reason.retryable(),
        }),
        HelloResponse::Accepted(accepted) => {
            // The daemon answers with its own identity, not its protocol version, so the
            // range check is against what this build sent — a daemon that accepted a
            // version it cannot speak is the case this catches.
            let range = ProtocolRange::attachable();
            if !range.contains(PROTOCOL_VERSION) {
                return Err(DaemonError::Refused {
                    reason: format!(
                        "this build speaks protocol v{PROTOCOL_VERSION}, which is outside {range}"
                    ),
                    retryable: false,
                });
            }
            Ok(accepted.daemon_identity)
        }
    }
}

/// Turn a refusal into a sentence a person can act on.
///
/// `RejectReason` has no `Display`, deliberately: proto refuses to decide how a reason is
/// phrased for a user, because that is the client's business and a CLI and a window word it
/// differently. This is the window's wording, and it keeps the detail the daemon bothered
/// to send — an unauthorized refusal that reached the user as "refused" would tell them
/// nothing at all.
fn describe(reason: &RejectReason) -> String {
    match reason {
        RejectReason::UnsupportedVersion { daemon, attachable } => format!(
            "the running daemon speaks protocol v{daemon} and serves {attachable}; this build              speaks v{PROTOCOL_VERSION}"
        ),
        RejectReason::Unauthorized { detail } => {
            format!("the daemon did not recognise this caller: {detail}")
        }
        RejectReason::ShuttingDown => {
            "the daemon is retiring and is not taking new connections".to_owned()
        }
        RejectReason::Malformed { detail } => {
            format!("the daemon could not read this client's hello: {detail}")
        }
    }
}

/// The id this window announces itself by.
///
/// Not an authority — §3.2 proves a caller's identity from peer credentials and the PTY
/// process tree, never from something the peer typed. It exists so a line in the daemon's
/// log says "the window" rather than "connection 4".
///
/// # Errors
///
/// [`DaemonError::Protocol`] if the composed id is not one proto accepts, which can only
/// happen if the constant below is edited into something with whitespace in it.
pub fn client_id(role: ClientRole) -> Result<ClientId, DaemonError> {
    format!("nysia-desktop/{role}").parse().map_err(
        |error: nysia_proto::handshake::HandshakeError| DaemonError::Protocol(error.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use nysia_proto::handshake::{HelloAccepted, HelloRejected};

    use super::*;

    /// A socket that replays a canned answer and records what was written to it.
    struct Scripted {
        outgoing: Vec<u8>,
        incoming: Cursor<Vec<u8>>,
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.incoming.read(buf)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.outgoing.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn identity() -> DaemonIdentity {
        DaemonIdentity {
            pid: 4242,
            started_at_ms: 1_757_721_600_000,
            launch_nonce: "0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60"
                .parse()
                .expect("a well-formed nonce"),
            app_version: "0.1.0".to_owned(),
        }
    }

    fn answering(json: &str) -> BufReader<Box<dyn Socket>> {
        BufReader::new(Box::new(Scripted {
            outgoing: Vec::new(),
            incoming: Cursor::new(format!("{json}\n").into_bytes()),
        }) as Box<dyn Socket>)
    }

    #[test]
    fn an_accepted_hello_yields_the_daemons_identity() {
        let accepted =
            serde_json::to_string(&HelloResponse::Accepted(HelloAccepted::new(identity())))
                .expect("serialisable");
        let mut socket = answering(&accepted);

        let learned = handshake(
            &mut socket,
            ClientRole::Control,
            &client_id(ClientRole::Control).expect("a valid id"),
        )
        .expect("the daemon accepted");
        assert_eq!(learned, identity());
    }

    #[test]
    fn a_rejected_hello_carries_the_daemons_reason_to_the_user() {
        // The reason is what ends up in front of a person, so losing it in favour of a
        // generic "could not connect" is the failure mode worth a test.
        let rejected = serde_json::to_string(&HelloResponse::Rejected(HelloRejected::new(
            RejectReason::ShuttingDown,
        )))
        .expect("serialisable");
        let mut socket = answering(&rejected);

        let error = handshake(
            &mut socket,
            ClientRole::Control,
            &client_id(ClientRole::Control).expect("a valid id"),
        )
        .expect_err("the daemon said no");
        match error {
            DaemonError::Refused { reason, retryable } => {
                assert!(retryable, "a retiring daemon is replaced in a moment");
                assert!(
                    reason.contains("retiring"),
                    "the reason must survive to the user, got {reason:?}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_socket_that_closes_mid_handshake_is_an_io_failure_not_a_parse_failure() {
        // Distinguishable on purpose: "the daemon went away" is retryable and "the daemon
        // speaks nonsense" is not, and the store shows the user different next steps.
        let mut socket = BufReader::new(Box::new(Scripted {
            outgoing: Vec::new(),
            incoming: Cursor::new(Vec::new()),
        }) as Box<dyn Socket>);

        let error = handshake(
            &mut socket,
            ClientRole::Stream,
            &client_id(ClientRole::Stream).expect("a valid id"),
        )
        .expect_err("nothing came back");
        assert!(matches!(error, DaemonError::Io(_)), "got {error:?}");
    }

    #[test]
    fn a_garbled_answer_is_reported_as_a_protocol_failure() {
        let mut socket = answering("{\"ok\":\"maybe\"}");
        let error = handshake(
            &mut socket,
            ClientRole::Control,
            &client_id(ClientRole::Control).expect("a valid id"),
        )
        .expect_err("that is not a HelloResponse");
        assert!(matches!(error, DaemonError::Protocol(_)), "got {error:?}");
    }

    #[test]
    fn every_refusal_is_phrased_for_a_person_and_keeps_the_daemons_detail() {
        // A refusal that reached the user as "refused" would tell them nothing. Each of
        // these has a detail the daemon bothered to send, and it has to survive.
        let reasons = [
            (
                RejectReason::UnsupportedVersion {
                    daemon: PROTOCOL_VERSION,
                    attachable: ProtocolRange::attachable(),
                },
                "protocol",
            ),
            (
                RejectReason::Unauthorized {
                    detail: "peer credentials did not resolve".to_owned(),
                },
                "peer credentials did not resolve",
            ),
            (RejectReason::ShuttingDown, "retiring"),
            (
                RejectReason::Malformed {
                    detail: "hello was not json".to_owned(),
                },
                "hello was not json",
            ),
        ];

        for (reason, expected) in reasons {
            let sentence = describe(&reason);
            assert!(
                sentence.contains(expected),
                "{reason:?} became {sentence:?}, which drops {expected:?}"
            );
        }
    }

    #[test]
    fn both_roles_produce_an_id_proto_accepts() {
        for role in [ClientRole::Control, ClientRole::Stream] {
            let id = client_id(role).expect("a valid id");
            assert!(id.as_str().contains(role.as_str()));
        }
    }

    #[test]
    fn the_endpoint_is_the_one_proto_names() {
        let endpoint = endpoint().expect("this account can name a pipe");
        assert!(
            endpoint.contains(&nysia_proto::version::endpoint_stem(PROTOCOL_VERSION)),
            "the endpoint {endpoint:?} must be proto's, not a second spelling"
        );
    }
}
