//! The socket server and its wire framing.
//!
//! A versioned named pipe on Windows (`\\.\pipe\nysiad-v1-<user>`) and a Unix socket on
//! macOS (`nysiad-v1.sock`). Every consumer — the GUI, `nysia <verb>`, `nysia hook` — is a
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
//! `nysia-proto` names the two credit frames from the reader's and the writer's point of
//! view, which is worth pinning down here because the daemon is the reader of the *pty* and
//! the writer of the *socket*, and the two readings point opposite ways.
//!
//! What this implementation does, and what §7.3's sentence requires:
//!
//! 1. On attach the daemon sends a [`nysia_proto::CreditGrant`] carrying the window
//!    constants and the opening allowance. It is the daemon that holds
//!    [`nysia_proto::CreditWindow::DEFAULT`], so it is the daemon that announces it — the
//!    client is never expected to have its own copy.
//! 2. The daemon spends that allowance byte for byte as it writes output frames.
//! 3. The client sends a [`nysia_proto::CreditAck`] *after* xterm's `write()` callback, not
//!    on arrival, because the window tracks what has been rendered. Each ack replenishes the
//!    allowance, capped at [`nysia_proto::CreditWindow::per_stream_max`].
//! 4. **At zero allowance the daemon stops reading the pty.** The bounded queue behind
//!    [`crate::pty::PtyOutput`] fills, its reader thread blocks on the send, the kernel pty
//!    buffer fills, and the child blocks. That is the complete answer to a `yes` flood: the
//!    backpressure reaches the process producing the output rather than being absorbed by a
//!    buffer somewhere in between.

pub mod control;
pub mod endpoint;
pub mod lease;
pub mod transport;

pub use control::{ControlError, ControlReader, ControlWriter, MAX_CONTROL_LINE_BYTES};
pub use endpoint::{
    ENDPOINT_VAR, Endpoint, EndpointResolveError, EnvSource, Listening, RUNTIME_DIR_VAR,
};
pub use lease::{LeaseError, PidRecordFile};
pub use transport::{Connection, ConnectionReader, ConnectionWriter, Listener, TransportError};
