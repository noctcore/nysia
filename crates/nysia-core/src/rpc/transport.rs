//! Binding, dialling, and the platform difference between a socket and a pipe.
//!
//! Everything above this module works in terms of one [`Connection`] — a byte stream with a
//! reader half, a writer half and, on the daemon's side, the kernel's word on who is at the
//! other end. Below it there are two quite different objects:
//!
//! | | Unix | Windows |
//! |---|---|---|
//! | The endpoint | a file in the runtime directory | a name in a machine-global namespace |
//! | Stale after a crash | **yes** — the file outlives the process | **no** — the name is the object |
//! | Exclusive creation | `bind` fails only if the file is there | `first_pipe_instance` |
//! | Accepting | one listener, many connections | one *instance* per connection |
//!
//! The Windows column is why [`Listener::accept`] creates the next pipe instance before it
//! services the one that just connected: an instance is consumed by the connection it
//! accepted, and a server that creates the replacement afterwards leaves a window in which
//! the endpoint does not exist and a dialling client gets `FILE_NOT_FOUND` from a daemon
//! that is running perfectly well.
//!
//! The Unix column is the whole of §12 Q5's "stale socket" problem, and it does not exist on
//! Windows at all — see [`crate::rpc::discovery`] for how it is answered.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, ReadHalf, WriteHalf};

use crate::rpc::endpoint::{Endpoint, Listening};
use crate::rpc::peer::PeerCredentials;

/// A byte stream this module can hand out halves of.
///
/// Private, and the reason [`ConnectionReader`] and [`ConnectionWriter`] are newtypes rather
/// than aliases: the caller works with two opaque halves, and swapping what is underneath —
/// a pipe instance for a socket, a socket for something a test invented — never reaches it.
trait Duplex: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Duplex for T {}

/// Why a connection could not be established or accepted.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The endpoint is already bound by a live daemon.
    ///
    /// Distinct from every other bind failure because it is the one a spawner *expects*:
    /// losing the race to bind means somebody else won it, and the right response is to
    /// connect to them rather than to report an error (§12 Q5).
    #[error("{endpoint} is already bound by another daemon")]
    AlreadyBound {
        /// Which endpoint.
        endpoint: String,
    },
    /// Nothing is listening there.
    #[error("nothing is listening on {endpoint}")]
    NotListening {
        /// Which endpoint.
        endpoint: String,
    },
    /// The endpoint could not be bound for some other reason.
    #[error("could not bind {endpoint}: {source}")]
    Bind {
        /// Which endpoint.
        endpoint: String,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },
    /// A connection could not be accepted.
    #[error("could not accept a connection on {endpoint}: {source}")]
    Accept {
        /// Which endpoint.
        endpoint: String,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },
    /// The peer could not be identified, so the connection cannot be served.
    #[error("could not identify the peer on {endpoint}: {source}")]
    Peer {
        /// Which endpoint.
        endpoint: String,
        /// The underlying failure.
        #[source]
        source: crate::rpc::peer::PeerError,
    },
}

/// The reading half of a connection.
pub struct ConnectionReader(ReadHalf<Box<dyn Duplex>>);

/// The writing half of a connection.
pub struct ConnectionWriter(WriteHalf<Box<dyn Duplex>>);

impl AsyncRead for ConnectionReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for ConnectionWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// One connection, and what the kernel said about its peer.
pub struct Connection {
    reader: ConnectionReader,
    writer: ConnectionWriter,
    peer: Option<PeerCredentials>,
}

impl Connection {
    /// Build a connection over `stream`, recording `peer` if the kernel named one.
    fn new(stream: Box<dyn Duplex>, peer: Option<PeerCredentials>) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            reader: ConnectionReader(reader),
            writer: ConnectionWriter(writer),
            peer,
        }
    }

    /// What the kernel said about the peer, on the daemon's side of a connection.
    ///
    /// `None` on the client's side: a client has no reason to ask who the daemon is running
    /// as, and the handshake's [`nysia_proto::DaemonIdentity`] answers the question it does
    /// have.
    #[must_use]
    pub fn peer(&self) -> Option<&PeerCredentials> {
        self.peer.as_ref()
    }

    /// Take the halves apart, so reading and writing can proceed concurrently.
    #[must_use]
    pub fn split(self) -> (ConnectionReader, ConnectionWriter) {
        (self.reader, self.writer)
    }
}

/// A bound endpoint, accepting connections.
pub struct Listener {
    endpoint: Endpoint,
    inner: Inner,
}

/// The platform half of a [`Listener`].
enum Inner {
    /// A Unix domain socket listener and the path it must unlink on the way out.
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    /// The pipe name and the instance currently waiting for a client.
    ///
    /// Never an `Option`. `accept` is polled inside a `select!`, so it can be dropped part
    /// way through; taking the instance out before awaiting would leave the listener holding
    /// nothing whenever another branch of that select won, and the endpoint would briefly
    /// stop existing for reasons that have nothing to do with the endpoint.
    #[cfg(windows)]
    Pipe {
        name: String,
        pending: tokio::net::windows::named_pipe::NamedPipeServer,
    },
}

impl Listener {
    /// Bind `endpoint`, refusing to displace a daemon that is already there.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::AlreadyBound`] when another daemon holds the endpoint —
    /// which a spawner treats as "somebody else won, connect to them" — and
    /// [`TransportError::Bind`] for anything else.
    pub fn bind(endpoint: &Endpoint) -> Result<Self, TransportError> {
        let inner = match endpoint.listening() {
            #[cfg(unix)]
            Listening::UnixSocket(path) => {
                let listener = bind_unix(path)?;
                restrict_socket_to_owner(path);
                Inner::Unix(listener)
            }
            #[cfg(windows)]
            Listening::NamedPipe(name) => {
                // `first_pipe_instance` is the race lock, and a better one than any file
                // could be: the kernel refuses the *second* creator outright, so two daemons
                // starting in the same microsecond cannot both believe they bound the
                // endpoint. It is set only here, on the first instance; every later instance
                // must not claim it or the server could not accept a second client.
                let server = tokio::net::windows::named_pipe::ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(name)
                    .map_err(|source| {
                        if source.kind() == io::ErrorKind::PermissionDenied {
                            TransportError::AlreadyBound {
                                endpoint: name.clone(),
                            }
                        } else {
                            TransportError::Bind {
                                endpoint: name.clone(),
                                source,
                            }
                        }
                    })?;
                Inner::Pipe {
                    name: name.clone(),
                    pending: server,
                }
            }
            #[cfg(unix)]
            Listening::NamedPipe(name) => {
                return Err(TransportError::Bind {
                    endpoint: name.clone(),
                    source: io::Error::other("named pipes are a Windows endpoint"),
                });
            }
            #[cfg(windows)]
            Listening::UnixSocket(path) => {
                return Err(TransportError::Bind {
                    endpoint: path.display().to_string(),
                    source: io::Error::other("unix sockets are not a Windows endpoint"),
                });
            }
        };
        Ok(Self {
            endpoint: endpoint.clone(),
            inner,
        })
    }

    /// The endpoint this listener is bound to.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Wait for the next connection and identify its peer.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Accept`] when the accept itself fails and
    /// [`TransportError::Peer`] when the kernel will not say who connected — which is fatal
    /// for that connection rather than for the listener, because the uid check is the only
    /// thing standing between another account and a shell (§3.2).
    pub async fn accept(&mut self) -> Result<Connection, TransportError> {
        let name = self.endpoint.listening().to_string();
        match &mut self.inner {
            #[cfg(unix)]
            Inner::Unix(listener) => {
                let (stream, _) =
                    listener
                        .accept()
                        .await
                        .map_err(|source| TransportError::Accept {
                            endpoint: name.clone(),
                            source,
                        })?;
                let peer = crate::rpc::peer::credentials_of(&stream).map_err(|source| {
                    TransportError::Peer {
                        endpoint: name,
                        source,
                    }
                })?;
                Ok(Connection::new(Box::new(stream), Some(peer)))
            }
            #[cfg(windows)]
            Inner::Pipe {
                name: pipe,
                pending,
            } => {
                // Nothing is mutated before this await, which is what makes the accept
                // cancel-safe: a `select!` that drops this future leaves the instance exactly
                // where it was, still waiting for a client.
                pending
                    .connect()
                    .await
                    .map_err(|source| TransportError::Accept {
                        endpoint: name.clone(),
                        source,
                    })?;
                // The instance that just connected belongs to this client now. The
                // replacement is created *before* the connection is served, so the endpoint
                // never stops existing — a client dialling in the gap would otherwise get
                // "file not found" from a daemon that is running perfectly well.
                let server = std::mem::replace(pending, next_instance(pipe)?);

                let peer = crate::rpc::peer::credentials_of(&server).map_err(|source| {
                    TransportError::Peer {
                        endpoint: name,
                        source,
                    }
                })?;
                Ok(Connection::new(Box::new(server), Some(peer)))
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // The socket file outlives the process that bound it, which is the whole of the
        // stale-socket problem. Unlinking it on a clean shutdown does not *solve* that — a
        // crash still leaves one behind, and `discovery` has to cope either way — but it
        // keeps the common case tidy.
        #[cfg(unix)]
        if let Listening::UnixSocket(path) = self.endpoint.listening() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Create the next pipe instance, which is what lets a second client connect.
#[cfg(windows)]
fn next_instance(
    name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, TransportError> {
    tokio::net::windows::named_pipe::ServerOptions::new()
        .create(name)
        .map_err(|source| TransportError::Bind {
            endpoint: name.to_owned(),
            source,
        })
}

/// Dial `endpoint`.
///
/// # Errors
///
/// Returns [`TransportError::NotListening`] when nothing is there — the answer a spawner
/// acts on — and [`TransportError::Bind`] for anything else.
pub async fn connect(endpoint: &Endpoint) -> Result<Connection, TransportError> {
    match endpoint.listening() {
        #[cfg(unix)]
        Listening::UnixSocket(path) => {
            let stream =
                tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|source| match source.kind() {
                        // A socket file with no daemon behind it refuses the connection. That is
                        // the signal `discovery` treats as "stale", and it is a stronger one than
                        // the file's existence could ever be.
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                            TransportError::NotListening {
                                endpoint: path.display().to_string(),
                            }
                        }
                        _ => TransportError::Bind {
                            endpoint: path.display().to_string(),
                            source,
                        },
                    })?;
            Ok(Connection::new(Box::new(stream), None))
        }
        #[cfg(windows)]
        Listening::NamedPipe(name) => {
            let client = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(name)
                .map_err(|source| match source.kind() {
                    io::ErrorKind::NotFound => TransportError::NotListening {
                        endpoint: name.clone(),
                    },
                    _ => TransportError::Bind {
                        endpoint: name.clone(),
                        source,
                    },
                })?;
            Ok(Connection::new(Box::new(client), None))
        }
        #[cfg(unix)]
        Listening::NamedPipe(name) => Err(TransportError::Bind {
            endpoint: name.clone(),
            source: io::Error::other("named pipes are a Windows endpoint"),
        }),
        #[cfg(windows)]
        Listening::UnixSocket(path) => Err(TransportError::Bind {
            endpoint: path.display().to_string(),
            source: io::Error::other("unix sockets are not a Windows endpoint"),
        }),
    }
}

/// Bind a Unix socket, binding past one a crashed daemon left behind.
///
/// # What `AddrInUse` actually means
///
/// On Unix the endpoint is a file, and `bind` refuses whenever that file exists. A daemon
/// that was killed skips the [`Drop`] above, so the file outlives it — and the replacement
/// then reads `AddrInUse` as "another daemon holds the endpoint", answers
/// [`TransportError::AlreadyBound`], and `nysia --daemon` **exits zero having served
/// nobody**. Every client that follows is told nothing is listening by a process that
/// reported success. The Windows column of this module's table is why this has never been
/// seen there: a pipe name *is* the object and stops existing with the last handle to it.
///
/// So `AddrInUse` is not the answer, it is the question. The answer is the one
/// [`crate::rpc::discovery`] already gives for a stale lease: **decide by trying to reach
/// what it names, never by the fact that it is there.** A socket that refuses a connection
/// has nothing behind it.
///
/// # What is unlinked, and what is not
///
/// Only a **socket** whose `(dev, ino)` is still the one that was probed. Not a regular
/// file, not a symlink, not a socket that has been replaced since — each of those leaves
/// the file alone and reports `AlreadyBound`, which is the conservative answer and the one
/// this function had before. A probe that fails for any reason other than "refused" is also
/// `AlreadyBound`: an endpoint that cannot be reasoned about is not one to delete.
///
/// The identity check is what keeps a race between two starting daemons from costing one of
/// them its socket. Both probe the stale file and find it refused; the first unlinks it and
/// binds; the second sees an inode that is no longer the one it probed, declines to unlink,
/// and its retry answers `AlreadyBound` — which is true, and was not true a moment earlier.
/// Without the check the second would unlink the *first daemon's live socket* and bind over
/// it, leaving a daemon listening on a path no client can name.
///
/// A window remains between the identity check and the unlink, in which another daemon
/// could bind. It is a few instructions wide, it needs two daemons started in the same
/// instant on one runtime directory, and the spawn lock in [`crate::rpc::discovery`] already
/// serialises every daemon Nysia starts for itself — two at once means two people running
/// `nysia --daemon` by hand. Closing it properly needs a lock this module does not own, and
/// the failure it would prevent is one idle daemon exiting on its own timer.
#[cfg(unix)]
fn bind_unix(path: &std::path::Path) -> Result<tokio::net::UnixListener, TransportError> {
    let already_bound = || TransportError::AlreadyBound {
        endpoint: path.display().to_string(),
    };
    let bind = |source: io::Error| {
        if source.kind() == io::ErrorKind::AddrInUse {
            already_bound()
        } else {
            TransportError::Bind {
                endpoint: path.display().to_string(),
                source,
            }
        }
    };

    match tokio::net::UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(source) if source.kind() == io::ErrorKind::AddrInUse => {}
        Err(source) => return Err(bind(source)),
    }

    // Sampled before the probe, so the comparison after it spans the whole of the window in
    // which the file could have been replaced.
    let probed = socket_identity(path);
    match std::os::unix::net::UnixStream::connect(path) {
        // Something answered. This is the case `AlreadyBound` was written for, and the only
        // one in which it is true.
        Ok(_) => return Err(already_bound()),
        Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => {}
        // The file went away between the bind and the probe — most likely the daemon that
        // held it shutting down cleanly. There is nothing to unlink and the retry below is
        // the whole of what is left to do.
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(already_bound()),
    }

    if let Some(probed) = probed {
        if socket_identity(path) != Some(probed) {
            return Err(already_bound());
        }
        tracing::info!(
            endpoint = %path.display(),
            "the endpoint was a socket with nothing behind it; removing it and binding"
        );
        if std::fs::remove_file(path).is_err() {
            return Err(already_bound());
        }
    }

    // A second `AddrInUse` here is somebody who won the race in the moment the unlink opened,
    // and `AlreadyBound` is the true answer to it.
    tokio::net::UnixListener::bind(path).map_err(bind)
}

/// A socket's `(device, inode)`, or `None` for anything that is not one.
///
/// [`std::fs::symlink_metadata`] rather than `metadata`, so a symlink is judged as itself
/// rather than as whatever it points at. A symlink at the endpoint is not a socket this
/// function will report, and so is never a file [`bind_unix`] will remove.
#[cfg(unix)]
fn socket_identity(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_socket() {
        return None;
    }
    Some((metadata.dev(), metadata.ino()))
}

/// Narrow a bound socket to its owner.
///
/// Best effort, and the directory is already 0700 — this is the second lock on the same
/// door, because a socket anyone can connect to is a shell anyone can have.
#[cfg(unix)]
fn restrict_socket_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_client_reaches_the_daemon_and_the_daemon_learns_who_it_is() {
        let endpoint = crate::rpc::endpoint::scratch("roundtrip");
        let mut listener = Listener::bind(&endpoint).expect("binds");

        let dialling = tokio::spawn({
            let endpoint = endpoint.clone();
            async move {
                let connection = connect(&endpoint).await.expect("dials");
                let (_, mut writer) = connection.split();
                writer.write_all(b"ping\n").await.expect("writes");
                writer.flush().await.expect("flushes");
            }
        });

        let accepted = listener.accept().await.expect("accepts");
        let peer = accepted
            .peer()
            .expect("the daemon side knows its peer")
            .clone();
        // The peer is this very test process, so the account check must pass. If it does
        // not, every connection on this platform would be refused at the handshake.
        assert!(
            peer.authorize(),
            "a connection from this process must authorise, got {peer}"
        );

        let (mut reader, _) = accepted.split();
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(
            &mut tokio::io::BufReader::new(&mut reader),
            &mut line,
        )
        .await
        .expect("reads");
        assert_eq!(line, "ping\n");
        dialling.await.expect("the dialling task finishes");
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_second_daemon_cannot_bind_an_endpoint_that_is_already_held() {
        // The race lock, and the reason two clients spawning a daemon at once cannot both
        // win (§12 Q5). It is the kernel that refuses, not a file anyone could delete.
        let endpoint = crate::rpc::endpoint::scratch("exclusive");
        let _held = Listener::bind(&endpoint).expect("the first binds");
        assert!(matches!(
            Listener::bind(&endpoint),
            Err(TransportError::AlreadyBound { .. })
        ));
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// The crash-recovery path, which had never been exercised anywhere.
    ///
    /// A killed daemon skips [`Listener`]'s `Drop`, so its socket file outlives it. `bind`
    /// refuses any path that exists, and before this the refusal was reported as
    /// `AlreadyBound` — which `daemon.rs` turns into `Outcome::AlreadyRunning` and `main.rs`
    /// into a **zero exit**, so `nysia --daemon` succeeded having served nobody.
    ///
    /// `std::os::unix::net::UnixListener` is the one that leaves the file behind: std does
    /// not unlink on drop, which is exactly what a killed process does not get to do either.
    /// The assertion that the file is still there is not decoration — without it a std that
    /// started unlinking would leave this test binding a clean path and passing for nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_a_killed_daemon_left_behind_does_not_stop_its_replacement() {
        let endpoint = crate::rpc::endpoint::scratch("stale");
        let Listening::UnixSocket(path) = endpoint.listening() else {
            panic!("a unix endpoint is a socket");
        };

        let crashed = std::os::unix::net::UnixListener::bind(path).expect("the first binds");
        drop(crashed);
        assert!(
            std::fs::symlink_metadata(path).is_ok(),
            "std leaves the socket file behind, which is the whole premise of this test"
        );

        let mut listener = Listener::bind(&endpoint).expect("the replacement binds past it");

        // Bound, and *serving*: a `bind` that returned `Ok` on a path no client can reach
        // would be the same failure one layer down.
        let dialling = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { connect(&endpoint).await.map(|_| ()) }
        });
        listener.accept().await.expect("accepts");
        dialling.await.expect("joins").expect("a client reaches it");

        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// The mutation that proves the test above trips for its own reason (traps register #12).
    ///
    /// Unlinking whatever is in the way would pass that test perfectly and would also let a
    /// second daemon displace a live one. This is the same path with something *behind* the
    /// socket, and the answer has to be the opposite: `AlreadyBound`, with the live socket
    /// still there afterwards. `a_second_daemon_cannot_bind_an_endpoint_that_is_already_held`
    /// checks the error; this checks that the file survived, which is the half a careless
    /// unlink would break while still returning the right error.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_with_a_daemon_behind_it_is_neither_taken_nor_unlinked() {
        use std::os::unix::fs::MetadataExt;

        let endpoint = crate::rpc::endpoint::scratch("live-socket");
        let Listening::UnixSocket(path) = endpoint.listening() else {
            panic!("a unix endpoint is a socket");
        };
        let held = Listener::bind(&endpoint).expect("the first binds");
        let before = std::fs::symlink_metadata(path)
            .expect("the socket is there")
            .ino();

        assert!(
            matches!(
                Listener::bind(&endpoint),
                Err(TransportError::AlreadyBound { .. })
            ),
            "a daemon is answering there, so the endpoint is genuinely taken"
        );
        let after = std::fs::symlink_metadata(path)
            .expect("the live socket must still be there")
            .ino();
        assert_eq!(
            before, after,
            "the loser of the race unlinked the winner's socket"
        );

        drop(held);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// The second half of the mutation: only a *socket* is ever removed.
    ///
    /// A regular file at the endpoint also makes `bind` answer `AddrInUse`, and an
    /// implementation that unlinked on that answer would delete it. It is inside the daemon's
    /// own 0700 runtime directory and it is still not this function's to remove — "it was in
    /// the way" is the reasoning that turns a bind into a delete of something somebody meant
    /// to keep.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_that_is_not_a_socket_is_left_where_it_is() {
        let endpoint = crate::rpc::endpoint::scratch("not-a-socket");
        let Listening::UnixSocket(path) = endpoint.listening() else {
            panic!("a unix endpoint is a socket");
        };
        std::fs::write(path, b"not a socket").expect("writes the file");

        assert!(matches!(
            Listener::bind(&endpoint),
            Err(TransportError::AlreadyBound { .. })
        ));
        assert_eq!(
            std::fs::read(path).expect("the file must still be there"),
            b"not a socket"
        );

        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn dialling_an_endpoint_nobody_holds_says_so_rather_than_hanging() {
        let endpoint = crate::rpc::endpoint::scratch("absent");
        assert!(matches!(
            connect(&endpoint).await,
            Err(TransportError::NotListening { .. })
        ));
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn the_endpoint_keeps_existing_while_a_client_is_being_served() {
        // On Windows an instance is consumed by the connection it accepted. If the
        // replacement were created after the connection was served, a second client dialling
        // in that gap would be told nothing is listening — by a daemon that is running.
        let endpoint = crate::rpc::endpoint::scratch("second-client");
        let mut listener = Listener::bind(&endpoint).expect("binds");

        let first = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { connect(&endpoint).await.map(|_| ()) }
        });
        let held = listener.accept().await.expect("accepts the first");
        first.await.expect("joins").expect("the first dials");

        let second = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { connect(&endpoint).await.map(|_| ()) }
        });
        listener.accept().await.expect("accepts the second");
        second.await.expect("joins").expect("the second dials");

        drop(held);
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    #[tokio::test]
    async fn a_write_reaches_the_peer_in_both_directions() {
        let endpoint = crate::rpc::endpoint::scratch("duplex");
        let mut listener = Listener::bind(&endpoint).expect("binds");
        let dialling = tokio::spawn({
            let endpoint = endpoint.clone();
            async move {
                let (mut reader, mut writer) = connect(&endpoint).await.expect("dials").split();
                writer.write_all(b"up").await.expect("writes");
                writer.flush().await.expect("flushes");
                let mut back = [0u8; 4];
                reader.read_exact(&mut back).await.expect("reads");
                back
            }
        });
        let (mut reader, mut writer) = listener.accept().await.expect("accepts").split();
        let mut up = [0u8; 2];
        reader.read_exact(&mut up).await.expect("reads");
        assert_eq!(&up, b"up");
        writer.write_all(b"down").await.expect("writes");
        writer.flush().await.expect("flushes");
        assert_eq!(&dialling.await.expect("joins"), b"down");
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }
}
