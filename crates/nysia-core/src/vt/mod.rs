//! Terminal state: the authoritative grid, the replay ring and the logical line log.
//!
//! Terminal state lives in Rust and the webview is a display cache (D-7). That is what
//! makes thirty sessions cheap, makes an agent's read of its own output a Rust API call
//! rather than a scrape, and makes the UI genuinely disposable.
//!
//! Will own:
//!
//! - the `alacritty_terminal` grid, fed by the PTY reader thread;
//! - a bounded raw replay ring, so a reattaching client can be handed exactly the bytes it
//!   missed rather than a re-render;
//! - a logical line log, which is what `nysia terminal read --screen` (the default) serves,
//!   as opposed to `--stream`.
//!
//! Scrollback can contain secrets, so anything this module persists is owner-only and is
//! excluded from exports and diagnostics bundles.
//!
//! Owned by wave 1 (W2).

mod line_log;
mod osc;
mod replay;
mod state;

pub use line_log::{LineLog, LogicalLine};
pub use osc::{CommandState, OscSniffer, ShellState};
pub use replay::ReplayRing;
pub use state::{FEED_SLICE, ReadMode, TerminalRead, TerminalSize, TerminalState, VtConfig};
