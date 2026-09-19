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
use std::time::Duration;

use nysia_core::rpc::{Endpoint, Listening};
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
pub trait Socket: Read + Write + Send {
    /// Ask that a read give up after `timeout` instead of blocking indefinitely.
    ///
    /// The output reader owns the only handle to its socket, so nothing outside can
    /// interrupt a read in progress; a deadline is what lets it notice a stop signal against
    /// a peer that has gone silent without closing.
    ///
    /// **Best effort, and honestly so.** A Win32 named pipe opened as a file has no
    /// equivalent of `SO_RCVTIMEO` — the timeouts in `CreateNamedPipe` belong to the server
    /// end — so the Windows implementation accepts the request and does nothing. Correctness
    /// does not rest on it: the reader ends on EOF when the daemon goes, and nothing waits
    /// for that thread. See `daemon::stream`.
    ///
    /// # Errors
    ///
    /// Whatever the platform said, for a socket that supports deadlines and refused one.
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

#[cfg(windows)]
impl Socket for std::fs::File {
    fn set_read_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
        // Deliberately a no-op — see the trait docs. Reporting success for something not
        // done is the lesser evil against an error the caller could only ignore, and the
        // reader is written not to need it.
        Ok(())
    }
}

#[cfg(not(windows))]
impl Socket for std::os::unix::net::UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        std::os::unix::net::UnixStream::set_read_timeout(self, timeout)
    }
}

/// Where this build expects to find the daemon.
///
/// **One resolver, and it is not this one.** [`nysia_core::rpc::Endpoint`] composes the
/// endpoint for the daemon that binds it and for every client that dials it, so the window
/// asks it rather than reimplementing the recipe. A previous version of this function did
/// reimplement it and got three things wrong at once: on Unix it composed
/// `$XDG_RUNTIME_DIR/nysiad-v1.sock` while the daemon binds under its own runtime
/// directory, so the macOS leg could never find a daemon at all; it ignored
/// [`nysia_core::rpc::RUNTIME_DIR_VAR`] and [`nysia_core::rpc::ENDPOINT_VAR`], which is how
/// every isolated daemon is pointed somewhere else; and it composed the Windows pipe from a
/// raw `%USERNAME%` without the sanitiser, so it matched only for plain account names.
///
/// None of those are the sort of mistake a reader catches. They are the sort two
/// implementations of one convention produce, which is why there is now one.
///
/// # Errors
///
/// [`DaemonError::Endpoint`] when no runtime directory can be chosen or created, or when
/// the composed name does not fit what the platform allows.
pub fn endpoint() -> Result<Endpoint, DaemonError> {
    Endpoint::from_env().map_err(|error| DaemonError::Endpoint(error.to_string()))
}

/// Cap what a pipe server may do with this process's identity: check it, never wear it.
///
/// `SecurityIdentification` shifted into the flag position `CreateFile` wants, as
/// `winbase.h` defines `SECURITY_IDENTIFICATION`. Spelled here rather than imported because
/// the `windows` crate in `[workspace.dependencies]` does not enable the feature that
/// carries it, and adding one would be an edit to a coordinator-owned file for a single
/// `u32`.
///
/// ## Why it is set
///
/// Rust's own documentation for `OpenOptionsExt::security_qos_flags` says it "should be
/// specified when opening a named pipe, to control to which degree a server process can act
/// on behalf of a client process", and that without it "a malicious program can gain the
/// elevated privileges of a privileged Rust process ... by tricking it into opening a named
/// pipe". That is not hypothetical here: the daemon's endpoint is a **fixed, predictable
/// name**, and any local process may create a pipe of that name *before* the real daemon
/// does. The owner-only ACL §3.1 puts on the daemon's pipe protects the daemon's pipe — it
/// cannot protect a name nobody has claimed yet.
///
/// With the default flags a squatter that wins that race gets `SecurityImpersonation` when
/// this process connects, and can then act with this user's token. With
/// `SecurityIdentification` it can learn who connected and nothing more. The connection
/// still fails — the handshake refuses a peer that cannot speak the protocol — but it fails
/// without having handed anything away first.
///
/// `security_qos_flags` sets `SECURITY_SQOS_PRESENT` itself, so the level below is the whole
/// declaration.
#[cfg(windows)]
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

/// `ERROR_PIPE_BUSY`: every instance of the pipe is connected to somebody.
///
/// Matched by number, as `nysia_core::rpc::transport` matches it, because which
/// [`std::io::ErrorKind`] the standard library maps it to has changed between releases and
/// the number has not.
const ERROR_PIPE_BUSY: i32 = 231;

/// How many times a dial opens a pipe whose every instance is taken, and how long it waits
/// between tries.
///
/// **Core's numbers, for core's reason.** They are a copy of `PIPE_BUSY_ATTEMPTS` and
/// `PIPE_BUSY_BACKOFF` in `nysia_core::rpc::transport`, which are private to that module and
/// serve its async dial; nothing checks that the two stay equal, so change them together.
/// The daemon creates a pipe's next instance only after the last one has been accepted, so a
/// client that dials in that gap meets a busy pipe from a daemon that is listening perfectly
/// well. The gap is one `CreateNamedPipeW` long, and it is met: hooks dial on every agent
/// event and the window on every menu open and launch. With one dial and no retry, 2 of 300
/// calls the window made on connections of their own failed this way beside two hook-like
/// clients dialling in a loop.
///
/// A few hundred milliseconds at the outside, then it stops. Far too short to be a queue on
/// purpose: a daemon saturated with clients frees instances continuously, and a dial that
/// waited for one would turn a click into a hang. What is still busy after this is
/// [`DaemonError::Busy`].
#[cfg(windows)]
const PIPE_BUSY_ATTEMPTS: u32 = 10;

/// How long a dial waits between opens of a busy pipe. See [`PIPE_BUSY_ATTEMPTS`].
#[cfg(windows)]
const PIPE_BUSY_BACKOFF: Duration = Duration::from_millis(20);

/// Open a socket to the daemon at `listening`.
///
/// The variant decides which syscall, which is the whole reason
/// [`nysia_core::rpc::Listening`] is an enum rather than a path: a Windows pipe name is not
/// a filesystem path, and a `CreateFile` that treats one as such creates a file literally
/// named for the pipe instead of opening it.
///
/// A pipe whose every instance is taken is opened again, [`PIPE_BUSY_ATTEMPTS`] times in all.
/// Blocking, like the rest of this module: every caller is already on a blocking thread.
///
/// # Errors
///
/// [`DaemonError::Unreachable`] when nothing is listening — which is the ordinary case on a
/// machine where the daemon has not been started, not a fault — [`DaemonError::Busy`] when
/// the pipe stayed busy through every attempt, and [`DaemonError::Endpoint`] when the
/// endpoint names a transport this platform cannot dial.
pub fn open(listening: &Listening) -> Result<Box<dyn Socket>, DaemonError> {
    match listening {
        #[cfg(windows)]
        Listening::NamedPipe(name) => {
            use std::os::windows::fs::OpenOptionsExt;

            let mut attempt = 1;
            loop {
                // A Win32 named pipe is opened like a file. `read(true).write(true)` is what
                // makes it the duplex handle the protocol needs rather than a one-way reader.
                let opened = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .security_qos_flags(SECURITY_IDENTIFICATION)
                    .open(name);
                match opened {
                    Ok(pipe) => return Ok(Box::new(pipe)),
                    // The one failure worth trying again, and only until the daemon has had
                    // time to create its next instance.
                    Err(error)
                        if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                            && attempt < PIPE_BUSY_ATTEMPTS =>
                    {
                        attempt += 1;
                        std::thread::sleep(PIPE_BUSY_BACKOFF);
                    }
                    Err(error) => return Err(dial_failure(name, &error)),
                }
            }
        }
        #[cfg(not(windows))]
        Listening::UnixSocket(path) => std::os::unix::net::UnixStream::connect(path)
            .map(|socket| Box::new(socket) as Box<dyn Socket>)
            .map_err(|error| dial_failure(&path.display().to_string(), &error)),

        // Resolution is shared, so the wrong variant can only come from an explicit
        // `NYSIA_ENDPOINT` naming the other platform's transport. Refused with the endpoint
        // in the message rather than panicked on: it is a mistake in someone's environment,
        // and §6 keeps panics out of a path a person can reach.
        #[cfg(windows)]
        Listening::UnixSocket(path) => Err(DaemonError::Endpoint(format!(
            "{} is a Unix socket, which this Windows build cannot dial",
            path.display()
        ))),
        #[cfg(not(windows))]
        Listening::NamedPipe(name) => Err(DaemonError::Endpoint(format!(
            "{name} is a Windows named pipe, which this build cannot dial"
        ))),
    }
}

/// Which dial failures mean *nothing is listening*, and which mean something else went wrong.
///
/// **The same two kinds `nysia_core::rpc::transport::connect` treats that way, and no more.**
/// [`DaemonError::Unreachable`] is not a generic "could not connect": it is the one answer
/// that lets [`crate::state::Client::connect`] take the spawn lock and start a daemon, so
/// every failure folded into it is a failure that starts a process instead of reporting
/// itself.
///
/// The two that do mean it:
///
/// - **`NotFound`** — on Windows a pipe name that does not exist is the whole of "no daemon",
///   because the name *is* the object and stops existing with the last handle.
/// - **`ConnectionRefused`**, on Unix only, where a socket file outlives its daemon and
///   refuses the connection. Classifying only `NotFound` there would leave the macOS window
///   unable to start a daemon over a post-crash socket for as long as the file was there.
///
/// Everything else is retryable, so the window keeps dialling, but never a reason to spawn.
/// A busy pipe (`ERROR_PIPE_BUSY`) means a daemon is *right there* and all its instances are
/// in use, and is [`DaemonError::Busy`] so the sentence can say so; an access denial means
/// one is there and this caller may not talk to it, and is [`DaemonError::Io`]. Spawning
/// against either produced a daemon that could not bind, twenty seconds of waiting, and then
/// a complaint pointing at a log whose only line says the endpoint is already held.
///
/// **The endpoint goes to the log, never into the sentence.** A dial failure reaches the user
/// as a notice, and the pipe's name is this account's own endpoint on the machine: a notice
/// that repeats it tells the person nothing they can act on and puts it on screen.
fn dial_failure(endpoint: &str, error: &std::io::Error) -> DaemonError {
    tracing::debug!(endpoint, %error, "could not open the daemon's socket");
    let absent = matches!(error.kind(), std::io::ErrorKind::NotFound)
        || (cfg!(unix) && matches!(error.kind(), std::io::ErrorKind::ConnectionRefused));
    if absent {
        DaemonError::Unreachable {
            cause: error.to_string(),
        }
    } else if cfg!(windows) && error.raw_os_error() == Some(ERROR_PIPE_BUSY) {
        DaemonError::Busy
    } else {
        DaemonError::Io(format!("could not open the daemon's socket: {error}"))
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
        // A `kind` this build does not know, caught by proto's `#[serde(other)]`. The point
        // of that variant is that the client still learns it was refused instead of failing
        // the whole untagged frame and reporting "data did not match any variant" — so the
        // sentence here says exactly that much and no more. Inventing a specific reason
        // would be this build claiming to have understood something it did not.
        RejectReason::Unknown => {
            "the daemon refused this client for a reason this build does not recognise; it              is probably newer than the window"
                .to_owned()
        }
    }
}

/// The id this window announces itself by, on **both** of its connections.
///
/// ## One id, or the stream connection is orphaned
///
/// `nysia-proto`'s stream module states the contract exactly: "The two connections are tied
/// together by the `ClientId` in their `hello` frames: a client uses the same id for both,
/// and that is how the daemon knows which stream connection an attach on the control
/// connection is talking about. Nothing else links them."
///
/// So announcing a different id per role — which this used to do, interpolating the role
/// into the string — leaves the daemon with a control caller whose id matches no stream hub.
/// It answers a retryable rejection, every `stream_attach` fails, the store throws inside
/// connect and reconnects forever. The failure is indistinguishable from the daemon being
/// down, which is what makes it expensive to find.
///
/// ## Per window, or two windows fight over one hub
///
/// The id is also the key the hub is bound under, so a constant would have a second window
/// supersede the first's binding, and either window's unbind would remove whichever hub
/// currently holds the key. The process id separates live windows — two cannot share one —
/// and the start time separates a window from a dead predecessor whose id the OS recycled,
/// which matters because a hub outlives the process that bound it until the daemon notices.
///
/// Not an authority in any case: §3.2 proves a caller's identity from peer credentials and
/// the PTY process tree, never from something the peer typed. This only has to route and to
/// read well in a log.
///
/// # Errors
///
/// [`DaemonError::Protocol`] if the composed id is not one proto accepts, which can only
/// happen if the format below is edited into something carrying whitespace.
pub fn client_id() -> Result<ClientId, DaemonError> {
    // Composed once. Recomposing per connection would reintroduce the bug in a subtler form
    // the moment anything in the recipe stopped being constant.
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    let id = ID.get_or_init(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        format!("nysia-desktop/{}-{started:x}", std::process::id())
    });

    id.parse()
        .map_err(|error: nysia_proto::handshake::HandshakeError| {
            DaemonError::Protocol(error.to_string())
        })
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

    impl Socket for Scripted {
        fn set_read_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
            Ok(())
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
    fn only_the_kinds_core_calls_absent_are_reported_as_nothing_listening() {
        // The window may start a daemon when — and only when — it sees this answer, so the
        // set has to be the one `nysia_core::rpc::transport::connect` uses. A busy pipe or an
        // access denial means something *is* listening: spawning against it produces a daemon
        // that cannot bind, twenty seconds of waiting, and a log line saying so.
        let dial = |kind: std::io::ErrorKind| {
            dial_failure("the endpoint", &std::io::Error::new(kind, "as it happens"))
        };

        assert!(
            matches!(
                dial(std::io::ErrorKind::NotFound),
                DaemonError::Unreachable { .. }
            ),
            "a name that does not exist is the whole of `no daemon`"
        );
        // Unix only: a socket file outlives its daemon and refuses. On Windows a refusal is
        // not how an absent pipe presents, so it stays an error there.
        let refused = dial(std::io::ErrorKind::ConnectionRefused);
        if cfg!(unix) {
            assert!(
                matches!(refused, DaemonError::Unreachable { .. }),
                "a stale socket refuses, and the window has to be able to replace its daemon"
            );
        } else {
            assert!(matches!(refused, DaemonError::Io(_)), "got {refused:?}");
        }

        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::Other,
        ] {
            let failure = dial(kind);
            assert!(
                matches!(failure, DaemonError::Io(_)),
                "{kind:?} became {failure:?}, which is a licence to start a second daemon"
            );
            assert!(
                failure.retryable(),
                "{kind:?} must still have the window keep dialling"
            );
        }
    }

    #[test]
    fn a_busy_pipe_is_busy_and_never_nothing_listening() {
        // `Unreachable` is the one answer that lets the window start a daemon, and starting
        // one beside a daemon whose instances are all in use leaves it unable to bind. Built
        // from the number, as the dial matches it, and not from a kind.
        let busy = dial_failure(
            "the endpoint",
            &std::io::Error::from_raw_os_error(ERROR_PIPE_BUSY),
        );
        if cfg!(windows) {
            assert!(matches!(busy, DaemonError::Busy), "got {busy:?}");
        } else {
            // No socket on Unix fails this way, and 231 is not an errno there. `Busy` is a
            // pipe's answer, and a Unix dial that produced it would be saying something false.
            assert!(matches!(busy, DaemonError::Io(_)), "got {busy:?}");
        }
        assert!(busy.retryable(), "a busy daemon is worth dialling again");
    }

    #[cfg(windows)]
    #[test]
    fn a_pipe_that_frees_within_the_wait_is_opened() {
        // The case the retry exists for: every instance taken for a moment, then the daemon's
        // accept loop makes the next one. One dial and no retry refused this, and a refused
        // menu question wrote the daemon off.
        let endpoint = crate::interop::scratch("frees");
        let occupied = crate::interop::Occupied::at(&endpoint);
        let freed = occupied.free_after(Duration::from_millis(60));

        let opened = open(endpoint.listening());
        let instance = freed.join();

        assert!(
            matches!(instance, Ok(Ok(_))),
            "the next instance was never made, so this proves nothing"
        );
        assert!(
            opened.is_ok(),
            "a daemon that made its next instance 60 ms later was refused: {:?}",
            opened.as_ref().err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_pipe_that_stays_busy_is_busy_after_the_wait_and_not_before() {
        let endpoint = crate::interop::scratch("busy");
        let _occupied = crate::interop::Occupied::at(&endpoint);

        let started = std::time::Instant::now();
        let opened = open(endpoint.listening());
        let waited = started.elapsed();

        assert!(
            matches!(opened, Err(DaemonError::Busy)),
            "a pipe with every instance taken became {:?}",
            opened.as_ref().err()
        );
        // Every sleep but the last one's. A dial that gave up at once is the defect this
        // closes: it took 244 µs, and a menu question that met it wrote the daemon off.
        assert!(
            waited >= PIPE_BUSY_BACKOFF * (PIPE_BUSY_ATTEMPTS - 1),
            "gave up after {waited:?}, without waiting for the next instance"
        );
        assert!(
            waited < Duration::from_secs(5),
            "a busy pipe is a bounded wait, not a queue: {waited:?}"
        );
    }

    #[test]
    fn a_dial_failure_does_not_put_the_endpoint_on_screen() {
        // The kinds test above used to assert the opposite. The sentence becomes a notice, and
        // on a `+` menu launch the notice read "could not open \\.\pipe\nysiad-v2-<account>:
        // …", which is this account's endpoint on the machine and nothing a person can act on.
        let endpoint = r"\\.\pipe\nysiad-v2-someone";
        let failures = [
            std::io::Error::from(std::io::ErrorKind::NotFound),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
            std::io::Error::from_raw_os_error(ERROR_PIPE_BUSY),
        ];
        for error in failures {
            let failure = super::super::CommandFailure::from(dial_failure(endpoint, &error));
            assert!(
                !failure.message.contains("nysiad-v2-someone"),
                "{error:?} names the endpoint: {}",
                failure.message
            );
            assert!(
                failure
                    .next_steps
                    .iter()
                    .all(|step| !step.contains("nysiad-v2-someone")),
                "{error:?} names the endpoint in its next steps: {:?}",
                failure.next_steps
            );
        }
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
            &client_id().expect("a valid id"),
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
            &client_id().expect("a valid id"),
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
            &client_id().expect("a valid id"),
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
            &client_id().expect("a valid id"),
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
            // Proto's `#[serde(other)]` catch-all. A daemon newer than this build can refuse
            // for a reason that did not exist when the window was compiled, and the user is
            // still owed a sentence rather than a blank notice.
            (RejectReason::Unknown, "does not recognise"),
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
    fn one_id_is_announced_on_both_connections() {
        // The whole link between them. A per-role id leaves the daemon holding a control
        // caller that matches no stream hub, so every attach is refused and the window
        // reconnects forever against a daemon that is working perfectly.
        let control = client_id().expect("a valid id");
        let stream = client_id().expect("a valid id");
        assert_eq!(control, stream);
        assert!(
            !control.as_str().contains(ClientRole::Control.as_str())
                && !control.as_str().contains(ClientRole::Stream.as_str()),
            "the id must not carry a role, or it cannot be the same on both: {control}"
        );
    }

    #[test]
    fn the_id_is_stable_for_the_life_of_the_window() {
        // A reconnect has to rebind the same hub, so an id that changed between connections
        // would strand the previous binding on the daemon until it timed the client out.
        let first = client_id().expect("a valid id");
        for _ in 0..8 {
            assert_eq!(client_id().expect("a valid id"), first);
        }
    }

    #[test]
    fn the_id_names_this_process_so_two_windows_cannot_share_a_hub() {
        // A constant would have the second window supersede the first's binding, and either
        // window's unbind would then remove whichever hub happened to hold the key.
        let id = client_id().expect("a valid id");
        assert!(
            id.as_str().contains(&std::process::id().to_string()),
            "{id} does not identify this process"
        );
    }

    #[test]
    fn the_endpoint_is_the_one_both_halves_resolve() {
        // Not a second spelling of the recipe — that is exactly what this module stopped
        // owning. What is worth asserting here is that the window asks `nysia-core` and gets
        // the same answer a daemon in this environment would bind, which is the property the
        // deleted copy quietly broke on the Unix leg.
        let mine = endpoint().expect("this environment can name an endpoint");
        let daemons = nysia_core::rpc::Endpoint::from_env().expect("core resolves it too");
        assert_eq!(mine.listening(), daemons.listening());
        assert!(
            mine.listening()
                .to_string()
                .contains(&nysia_proto::version::endpoint_stem(PROTOCOL_VERSION)),
            "the endpoint {} must carry proto's stem",
            mine.listening()
        );
    }
}
