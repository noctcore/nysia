//! The socket server and its wire framing.
//!
//! A versioned named pipe on Windows (`\\.\pipe\nysiad-v<protocol>-<user>`) and a Unix socket on
//! macOS (`nysiad-v<protocol>.sock`). Every consumer — the GUI, `nysia <verb>`, `nysia hook` — is a
//! client of the same socket and the same verb surface. The window has no privileged path
//! (D-1).
//!
//! The wire types themselves live in `nysia-proto`, which is the sole authority on their
//! shape (D-13). Nothing here defines one.
//!
//! # The modules, in the order a connection meets them
//!
//! | Module | What it owns |
//! |---|---|
//! | [`endpoint`] | where the daemon listens, and the lease, lock and log beside it |
//! | [`discovery`] | connect, or spawn one daemon and only one (§12 Q5) |
//! | [`transport`] | binding, dialling, and the platform difference between a socket and a pipe |
//! | [`peer`] | who the kernel says is calling, and which session they descend from (§3.2) |
//! | [`control`] | newline-delimited JSON: the handshake and the verb envelopes |
//! | [`lease`] | the pid record that says which daemon this is |
//! | [`session`] | the sessions the daemon owns: PTY, terminal state, and the pump between |
//! | [`stream`] | length-prefixed binary output under a credit window (§7.3) |
//! | [`server`] | the accept loop, the verb dispatch and idle retire |
//! | [`client`] | the other half, which the CLI and the Tauri shell both use |
//!
//! # Two framings, two roles
//!
//! Control is newline-delimited JSON, request/response. Streams are length-prefixed binary
//! (`[kind:u8][streamId:u32 BE][len:u32 BE][payload]`) under a credit window. They are
//! carried on **separate connections**, told apart by the `role` in the handshake, because
//! their shapes are opposite: a slow control reply must never be delayed by a terminal
//! firehose, and a stalled terminal read must never delay a session close.
//!
//! # Credit, and which direction it flows
//!
//! **The producer grants and the consumer acks.** The daemon produces terminal output and
//! owns the window; the client consumes it and acknowledges what it has rendered. Naming the
//! two ends "reader" and "writer" is what made this ambiguous — the daemon is the reader of
//! the *pty* and the writer of the *socket*, and the two readings point opposite ways — so
//! nothing in this codebase describes credit in those terms.
//!
//! What this implementation does, and what §7.3's sentence requires:
//!
//! 1. On attach the daemon sends a [`nysia_proto::CreditGrant`] carrying the window
//!    constants and the opening allowance. It is the daemon that holds the window, so it is
//!    the daemon that announces it — the client is never expected to have its own copy. A
//!    grant arriving *from* a client is ignored: a consumer able to hand itself an allowance
//!    could turn the backpressure off from the outside.
//! 2. The daemon spends that allowance as it writes output frames, charged in **payload
//!    bytes**. Not the encoded length: the nine-byte frame header is transport overhead the
//!    consumer never receives as content, and the ack is emitted by the code that has just
//!    written a payload into a terminal. Two units here is a window that drains nine bytes
//!    per frame until a long-lived session stalls for good.
//! 3. The client sends a [`nysia_proto::CreditAck`] *after* xterm's `write()` callback, not
//!    on arrival, because the window tracks what has been rendered. Each ack replenishes the
//!    allowance, capped at [`nysia_proto::CreditWindow::per_stream_max`].
//! 4. **At zero allowance the daemon stops reading the pty.** The bounded queue behind
//!    [`crate::pty::PtyOutput`] fills, its reader thread blocks on the send, the kernel pty
//!    buffer fills, and the child blocks. That is the complete answer to a `yes` flood: the
//!    backpressure reaches the process producing the output rather than being absorbed by a
//!    buffer somewhere in between.
//!
//! # Attaching, and why the answer goes first
//!
//! A stream id is minted on the control connection and used on the stream connection, and
//! **nothing orders those two sockets**. A client can only record an id by parsing the attach
//! response, and proto's routing rule says a frame naming an id at or beyond the next one to
//! assign is a desync that costs the whole connection. So the daemon writes the response
//! before it enqueues one byte of the opening grant or the replay ring: the side that can
//! supply the ordering is the side that has to.

pub mod client;
pub mod control;
pub mod discovery;
pub mod endpoint;
pub(crate) mod errors;
pub mod lease;
pub mod peer;
pub mod server;
pub mod session;
pub mod stream;
pub mod transport;

#[cfg(test)]
mod interop;
#[cfg(test)]
pub(crate) mod testing;

pub use client::{Client, ClientError};
pub use control::{ControlError, ControlReader, ControlWriter, MAX_CONTROL_LINE_BYTES};
pub use discovery::{Discovered, DiscoveryError, SpawnPolicy, discover};
pub use endpoint::{
    ENDPOINT_VAR, Endpoint, EndpointResolveError, EnvSource, Listening, RUNTIME_DIR_VAR,
};
pub use lease::{LeaseError, PidRecordFile};
pub use peer::{CallerSession, PeerCredentials, PeerError, ancestry, parent_of};
pub use server::{Daemon, DaemonConfig, ServerError};
pub use session::{OwnedSession, SessionError, SessionRegistry};
pub use stream::{
    AttachedStream, BoundStream, ConnectionKey, SendOutcome, StreamRegistry, StreamSink,
};
pub use transport::{Connection, ConnectionReader, ConnectionWriter, Listener, TransportError};
