//! Nysia's wire types.
//!
//! Every message that crosses the daemon socket, and every type the webview is allowed to
//! know about, is defined here and exported to TypeScript with `ts-rs`. The generation is
//! one-way — Rust → TypeScript, never the reverse (D-13) — so this crate is the single
//! authority on the shape of the wire.
//!
//! The generated TypeScript is committed under `apps/web/src/generated/` and a drift guard
//! (`pnpm ts-drift`) fails the build when it stops matching what this crate produces.

pub mod credit;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod identity;
mod newtype;
pub mod session;
pub mod terminal;
pub mod version;

pub use credit::{CreditAck, CreditFrame, CreditGrant, CreditWindow};
pub use error::{ErrorCode, ErrorEnvelope, ErrorEnvelopeError, NextSteps};
pub use frame::{
    FRAME_HEADER_BYTES, Frame, FrameDecoder, FrameError, FrameKind, MAX_FRAME_PAYLOAD_BYTES,
    decode, encode, encode_into,
};
pub use handshake::{
    ClientId, ClientRole, DaemonIdentity, HandshakeError, HelloAccepted, HelloRejected,
    HelloRequest, HelloResponse, LaunchNonce, OkFalse, OkTrue, PidRecord, RejectReason,
};
pub use identity::{IdentityError, Incarnation, PaneKey, SessionHandle, SessionKind};
pub use session::{
    ExitStatus, SessionClose, SessionCreate, SessionCreated, SessionList, SessionSummary,
    ShellProfile,
};
pub use terminal::{
    LineCursor, ReadMode, TerminalError, TerminalRead, TerminalReadResult, TerminalResize,
    TerminalSend, TerminalWait, TerminalWaitResult, WaitFor, WaitOutcome,
};
pub use version::{
    EndpointError, MIN_ATTACHABLE_PROTOCOL_VERSION, PROTOCOL_VERSION, ProtocolRange,
    ProtocolVersion, endpoint_stem, unix_socket_file_name, windows_pipe_name,
};
