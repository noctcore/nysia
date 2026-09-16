//! `nysia --daemon`: becoming nysiad.
//!
//! Everything the daemon does lives in `nysia-core`. This file is the argv-selected entry
//! point and nothing else — bind, report, serve — which is what keeps D-11's "one binary, two
//! modes" from becoming "one binary, two implementations".
//!
//! # Losing the bind is not a failure, **if somebody won it**
//!
//! Two clients can race to start a daemon, and the kernel decides which wins. A daemon that
//! finds the endpoint already held has not failed: the thing it was started to guarantee —
//! that a daemon is listening there — is true. It says so and exits zero, so a spawner that
//! checks the status is not told to panic about a race that resolved correctly.
//!
//! That is a claim about the *world*, and [`TransportError::AlreadyBound`] is not enough to
//! establish it. It is the transport's reading of a failed `bind`, and several things earn
//! it with nothing listening at all: a regular file or a symlink sitting where the socket
//! belongs, a liveness probe that failed with anything other than `ConnectionRefused`, a
//! `remove_file` that was refused. Reporting success for those is a process that exits zero
//! having served nobody, and telling a supervisor the one thing it must not be told wrongly.
//!
//! So the guarantee is **checked rather than inferred**: dial the endpoint and see whether a
//! daemon answers. A handshake that is refused still counts — a daemon too new to talk to
//! this build is a daemon that is listening, and this process has still not been asked to be
//! one.
//!
//! # Three answers, because two of them are claims and the third is the absence of one
//!
//! The dial is bounded, and what it cannot establish it does not assert. An endpoint that
//! refuses a connection is empty and exits non-zero saying so. An endpoint that **holds the
//! connection and says nothing** — a wedged daemon, or on Windows a pipe whose every
//! instance is taken — is neither empty nor confirmed, and [`Outcome::Unanswered`] is the
//! word for it. Calling that "nobody is home" is what made `nysia --daemon` exit non-zero
//! against a daemon that was serving clients perfectly well.

use std::io::Write;
use std::time::Duration;

use nysia_core::rpc::{
    Client, ClientError, Daemon, DaemonConfig, Endpoint, ServerError, TransportError,
};
use nysia_proto::{ClientId, ClientRole};

/// Bind the endpoint and serve until the daemon retires or is interrupted.
///
/// # Errors
///
/// Returns [`ServerError`] when the endpoint cannot be resolved or bound for any reason other
/// than another daemon already holding it, and when the adoption lease cannot be written.
pub async fn run(never_retire: bool) -> Result<Outcome, ServerError> {
    run_at(Endpoint::from_env()?, never_retire).await
}

/// [`run`], on an endpoint that has already been resolved.
///
/// Split out so the two answers to a lost bind can be driven against a real endpoint without
/// the process environment having to be mutated — which no test can do safely while another
/// test in the same binary reads it.
///
/// # Errors
///
/// As [`run`], minus the resolution.
pub async fn run_at(endpoint: Endpoint, never_retire: bool) -> Result<Outcome, ServerError> {
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
        Err(ServerError::Transport(TransportError::AlreadyBound { endpoint: held })) => {
            return Ok(confirm(&endpoint, held).await);
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

/// Find out whether the daemon that took the endpoint actually answers there.
///
/// One dial, and the answer is read the way the module header says: anything that completes
/// a connection is a daemon, including one that refuses the handshake, and only a transport
/// failure is nobody.
///
/// # Bounded, because the dial is not
///
/// [`Client::connect`] writes a Hello and awaits a `HelloResponse` with no deadline of its
/// own. Without one here, `nysia --daemon` blocks **indefinitely** against an endpoint that
/// accepts and never answers — a wedged daemon still holding the pipe, or on Unix anything
/// at all that accepts on that path. Trading the wrong answer this used to give for a hang
/// would be an improvement and not a fix, so silence has a deadline and an outcome of its
/// own: something holds the endpoint, it did not answer, and neither of the two words beside
/// it is true of that.
async fn confirm(endpoint: &Endpoint, held: String) -> Outcome {
    let client_id = match PROBE_CLIENT_ID.parse::<ClientId>() {
        Ok(id) => id,
        // Unreachable: the constant is checked by `the_probe_client_id_is_well_formed`. A
        // daemon that could not name itself still has no business claiming somebody else is
        // listening, so the unverifiable answer is the honest one.
        Err(_) => return Outcome::NotListening { endpoint: held },
    };
    let dial = Client::connect(endpoint, &client_id, ClientRole::Control);
    match tokio::time::timeout(CONFIRM_DEADLINE, dial).await {
        Ok(Ok(_)) => Outcome::AlreadyRunning { endpoint: held },
        // **A busy endpoint is a daemon, and it is the one this used to get wrong.** The
        // accept loop replaces the pipe instance only after `pending.connect()` returns, so a
        // dial inside that window meets a pipe whose every instance is taken. `rpc::transport`
        // waits that window out, and what is still busy afterwards is a daemon serving
        // clients — never "nobody is home", which is the answer that starts a second daemon.
        Ok(Err(ClientError::Transport(TransportError::Busy { .. }))) => {
            Outcome::Unanswered { endpoint: held }
        }
        Ok(Err(ClientError::Transport(_))) => Outcome::NotListening { endpoint: held },
        // A daemon that refuses this build's handshake is a daemon. D-11 lets the two run
        // different versions on purpose, so a refusal here is the versioned handshake doing
        // its job, not an empty endpoint.
        Ok(Err(_)) => Outcome::AlreadyRunning { endpoint: held },
        Err(_elapsed) => Outcome::Unanswered { endpoint: held },
    }
}

/// How long the confirmation dial waits for a daemon to answer its handshake.
///
/// Far longer than a local handshake takes — one write and one read on a socket this machine
/// owns — and far shorter than the forever it was before.
const CONFIRM_DEADLINE: Duration = Duration::from_secs(5);

/// The client id the confirmation dial identifies itself with.
///
/// Its own name rather than the CLI's, so a daemon's log distinguishes "another daemon
/// checked whether I was here" from a person running a verb.
const PROBE_CLIENT_ID: &str = "nysiad-bind-probe";

/// How the daemon mode ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The daemon served and then retired or was interrupted.
    Retired,
    /// Another daemon already held the endpoint and answers there, so this process did
    /// nothing and did not need to.
    AlreadyRunning {
        /// Where that daemon is listening.
        endpoint: String,
    },
    /// The endpoint could not be bound and nothing answers on it.
    ///
    /// The case `AlreadyBound` cannot tell apart on its own: a regular file or a symlink
    /// where the socket belongs, a probe that failed for a reason other than
    /// `ConnectionRefused`, a `remove_file` that was refused. Nothing is listening, this
    /// process is not going to listen either, and the one thing a daemon must not do is say
    /// otherwise.
    NotListening {
        /// Where nothing is listening.
        endpoint: String,
    },
    /// The endpoint is held by something that would not answer.
    ///
    /// The third answer, and it exists because the other two are both claims this process
    /// cannot make about it. [`Self::AlreadyRunning`] would say a daemon is serving there,
    /// which is the one thing a supervisor must not be told wrongly; [`Self::NotListening`]
    /// would say the endpoint is empty and send somebody to delete what is at that path.
    /// What is actually known is narrower than either: something holds it, this build could
    /// not complete a handshake with it inside [`CONFIRM_DEADLINE`], and this process is not
    /// going to serve there either.
    ///
    /// Reached two ways: a peer that accepts and never answers, and — on Windows — a pipe
    /// whose every instance was still taken after `rpc::transport` waited out the accept
    /// loop's replacement window.
    Unanswered {
        /// Where something is holding the endpoint without answering.
        endpoint: String,
    },
}

/// How long a daemon started from argv stays up holding nothing.
///
/// Longer than the library default would need to be, because a daemon started by hand is
/// usually about to be talked to by a person, and a person is slower than a client.
const DEFAULT_IDLE_RETIRE: Duration = Duration::from_secs(300);

#[cfg(test)]
mod tests {
    use super::*;
    use nysia_core::rpc::EnvSource;

    /// An endpoint of this test's own, with nothing reading the process environment.
    fn scratch(tag: &str) -> Endpoint {
        let dir = std::env::temp_dir().join(format!("nysiad-bind-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Endpoint::resolve(
            nysia_proto::PROTOCOL_VERSION,
            EnvSource {
                runtime_dir_override: Some(dir),
                endpoint_override: None,
                home: None,
                xdg_runtime_dir: None,
                account: format!("bind-{tag}"),
                isolated: true,
            },
        )
        .expect("a scratch endpoint resolves")
    }

    #[test]
    fn the_probe_client_id_is_well_formed() {
        // `confirm` cannot report what it could not ask, so an id that will not parse turns
        // every lost bind into a failure. Pinned here rather than discovered at three in the
        // morning by a supervisor.
        assert!(PROBE_CLIENT_ID.parse::<ClientId>().is_ok());
    }

    /// Losing the bind to a daemon that **is** there is still a success.
    ///
    /// The half that has to keep working: a spawner racing another spawner must not be told
    /// to panic about a race that resolved correctly. Both legs.
    #[tokio::test]
    async fn a_bind_lost_to_a_daemon_that_answers_is_reported_as_success() {
        let endpoint = scratch("answering");
        let (daemon, listener) =
            Daemon::bind(DaemonConfig::new(endpoint.clone())).expect("the first daemon binds");
        let serving = tokio::spawn({
            let daemon = std::sync::Arc::clone(&daemon);
            async move {
                let _ = daemon.serve(listener).await;
            }
        });

        let outcome = run_at(endpoint.clone(), true)
            .await
            .expect("losing the bind is not an error");
        assert!(
            matches!(outcome, Outcome::AlreadyRunning { .. }),
            "got {outcome:?}"
        );

        daemon.shutdown();
        serving.abort();
        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// Losing the bind to **nothing at all** is a failure, and says so.
    ///
    /// # Which leg proves this
    ///
    /// **Unix only, and that is where the case is.** `bind_unix` answers `AlreadyBound` for a
    /// path it cannot bind and cannot prove is dead — a regular file is the simplest of those
    /// and the easiest to plant. On Windows the endpoint is a named pipe in a machine-global
    /// namespace with no file behind it, so there is nothing to put in the way: `AlreadyBound`
    /// there comes from `PermissionDenied` on a pipe somebody else holds, which is a daemon.
    /// The test above runs on both legs and is the one that covers the Windows answer.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_bind_lost_to_nothing_at_all_is_a_failure_rather_than_a_quiet_zero() {
        let endpoint = scratch("empty");
        let nysia_core::rpc::Listening::UnixSocket(path) = endpoint.listening() else {
            panic!("a unix endpoint is a socket path");
        };
        // Not a socket, not a daemon, and enough for `bind` to refuse: this is the shape a
        // stale file left by a crash has, and the one `AlreadyBound` cannot tell from a live
        // daemon on its own.
        std::fs::write(path, b"not a socket").expect("the file is planted");

        let outcome = run_at(endpoint.clone(), true)
            .await
            .expect("a taken endpoint is reported, not returned as an error");
        assert!(
            matches!(outcome, Outcome::NotListening { .. }),
            "a daemon that served nobody must not report success; got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// An endpoint that accepts and never answers is neither of the other two words.
    ///
    /// #96's fourth finding, the half about the deadline. [`Client::connect`] awaits a
    /// `HelloResponse` with no timeout of its own, so before this `nysia --daemon` blocked
    /// **forever** against a peer that completed the connection and said nothing — which is
    /// what a wedged daemon still holding the endpoint looks like from outside.
    ///
    /// The fixture is a bound [`Listener`] that nothing ever accepts on, which is that exact
    /// shape on both legs: the connection completes into the backlog (Unix) or onto the idle
    /// pipe instance (Windows), the Hello is written, and no answer ever comes.
    ///
    /// The whole call is wrapped in a deadline of its own, generously longer than the one
    /// under test, so that removing the bound fails this test instead of hanging the suite.
    #[tokio::test]
    async fn a_bind_lost_to_something_that_never_answers_is_neither_running_nor_empty() {
        let endpoint = scratch("silent");
        // Holds the endpoint and serves nobody: no `accept`, so no handshake is ever read.
        let _silent = nysia_core::rpc::Listener::bind(&endpoint).expect("the fixture binds");

        let outcome = tokio::time::timeout(CONFIRM_DEADLINE * 3, run_at(endpoint.clone(), true))
            .await
            .expect("the confirmation dial is bounded; unbounded, this never returns")
            .expect("a taken endpoint is reported, not returned as an error");
        assert!(
            matches!(outcome, Outcome::Unanswered { .. }),
            "silence is not a daemon serving and not an empty endpoint; got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }

    /// A pipe whose every instance is taken is a daemon, not an empty endpoint.
    ///
    /// The other half of #96's fourth finding, and Windows-only because the state is. The
    /// accept loop creates the replacement instance *after* `pending.connect()` returns, so a
    /// dial inside that window meets `ERROR_PIPE_BUSY` — which this folded into "nobody is
    /// home" and exited non-zero over, against a daemon that was listening perfectly well.
    ///
    /// Here nothing ever accepts, so the pipe stays busy past everything `rpc::transport`
    /// will wait: the answer must still not be [`Outcome::NotListening`].
    #[tokio::test]
    #[cfg(windows)]
    async fn a_bind_lost_to_a_pipe_whose_instances_are_all_taken_is_not_called_empty() {
        let endpoint = scratch("busy");
        let _held = nysia_core::rpc::Listener::bind(&endpoint).expect("the fixture binds");
        let nysia_core::rpc::Listening::NamedPipe(name) = endpoint.listening() else {
            panic!("a windows endpoint is a named pipe");
        };
        // Takes the only instance there is, the way a first client does.
        let _taken = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(name)
            .expect("the only instance is free until this takes it");

        let outcome = run_at(endpoint.clone(), true)
            .await
            .expect("a taken endpoint is reported, not returned as an error");
        assert!(
            matches!(outcome, Outcome::Unanswered { .. }),
            "a daemon with every instance taken is listening; got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(endpoint.runtime_dir());
    }
}
