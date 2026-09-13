//! The socket server and its wire framing.
//!
//! A versioned named pipe on Windows (`\.\pipe\nysiad-v1-<user>`) and a Unix socket on
//! macOS (`nysiad-v1.sock`). Every consumer — the GUI, `nysia <verb>`, `nysia hook` — is a
//! client of the same socket and the same verb surface. The window has no privileged path
//! (D-1).
//!
//! Will own:
//!
//! - the `hello` handshake and `daemonIdentity` response, with `retryable` set so a client
//!   knows whether to back off or die;
//! - newline-delimited JSON for control, length-prefixed binary
//!   (`[kind:u8][streamId:u32 BE][len:u32 BE][payload]`) for terminal output;
//! - caller authentication from kernel-supplied peer credentials plus PTY process-tree
//!   ancestry, never from an echoed token — the daemon spawned the process, so it can
//!   prove the caller's identity directly;
//! - the pid-record adoption lease (`pid` + `startedAtMs` + `launchNonce`), spawn-if-absent
//!   with a race lock, and idle retire.
//!
//! The wire types themselves live in `nysia-proto`, which is the sole authority on their
//! shape (D-13).
//!
//! Owned by wave 2 (W4).
