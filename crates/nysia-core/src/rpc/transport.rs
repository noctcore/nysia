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
                let listener = tokio::net::UnixListener::bind(path).map_err(|source| {
                    if source.kind() == io::ErrorKind::AddrInUse {
                        TransportError::AlreadyBound {
                            endpoint: path.display().to_string(),
                        }
                    } else {
                        TransportError::Bind {
                            endpoint: path.display().to_string(),
                            source,
                        }
                    }
                })?;
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
