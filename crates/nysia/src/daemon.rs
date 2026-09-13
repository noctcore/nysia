//! `nysia --daemon`: becoming nysiad.
//!
//! Everything the daemon does lives in `nysia-core`. This file is the argv-selected entry
//! point and nothing else — bind, report, serve — which is what keeps D-11's "one binary, two
//! modes" from becoming "one binary, two implementations".
//!
//! # Losing the bind is not a failure
//!
//! Two clients can race to start a daemon, and the kernel decides which wins. A daemon that
//! finds the endpoint already held has **not** failed: the thing it was started to guarantee —
//! that a daemon is listening there — is true. It says so and exits zero, so a spawner that
//! checks the status is not told to panic about a race that resolved correctly.

use std::io::Write;
use std::time::Duration;

use nysia_core::rpc::{Daemon, DaemonConfig, Endpoint, ServerError, TransportError};

/// Bind the endpoint and serve until the daemon retires or is interrupted.
///
/// # Errors
///
/// Returns [`ServerError`] when the endpoint cannot be resolved or bound for any reason other
/// than another daemon already holding it, and when the adoption lease cannot be written.
pub async fn run(never_retire: bool) -> Result<Outcome, ServerError> {
    let endpoint = Endpoint::from_env()?;
    let config = DaemonConfig {
        idle_retire_after: if never_retire {
            None
        } else {
            Some(DEFAULT_IDLE_RETIRE)
        },
        ..DaemonConfig::new(endpoint.clone())
    };

    let (daemon, listener) = match Daemon::bind(config) {
        Ok(bound) => bound,
        Err(ServerError::Transport(TransportError::AlreadyBound { endpoint })) => {
            return Ok(Outcome::AlreadyRunning { endpoint });
        }
        Err(err) => return Err(err),
    };

    // The one line on stdout, after the endpoint is bound and the lease is written — so a
    // supervisor watching for it learns something that is already true rather than something
    // that is about to be attempted. A *spawned* daemon has this redirected into the runtime
    // directory's log, which is where it is read from afterwards.
    println!(
        "nysiad listening on {} (pid {}, nonce {})",
        endpoint.listening(),
        daemon.identity().pid,
        daemon.identity().launch_nonce
    );
    let _ = std::io::stdout().flush();

    daemon.serve(listener).await?;
    Ok(Outcome::Retired)
}

/// How the daemon mode ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The daemon served and then retired or was interrupted.
    Retired,
    /// Another daemon already held the endpoint, so this process did nothing.
    AlreadyRunning {
        /// Where that daemon is listening.
        endpoint: String,
    },
}

/// How long a daemon started from argv stays up holding nothing.
///
/// Longer than the library default would need to be, because a daemon started by hand is
/// usually about to be talked to by a person, and a person is slower than a client.
const DEFAULT_IDLE_RETIRE: Duration = Duration::from_secs(300);
