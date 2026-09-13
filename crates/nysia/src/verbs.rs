//! Running a client verb against the daemon socket, and printing what comes back.
//!
//! Nothing in here touches a pty. Every verb is a request on the same socket the GUI uses,
//! which is what makes D-1 a property rather than a promise: there is one way in, and this
//! file is a user of it rather than an exception to it.
//!
//! # The output contract
//!
//! With `--json`: the result on **stdout**, one JSON value; the error envelope on **stderr**;
//! the exit code says which happened. A tool parsing stdout never has to decide whether what
//! it is holding is an answer or an apology.
//!
//! Without it: a short human line, and silence on success where there is nothing to say.
//! `session create` prints the new handle alone, so `nysia terminal read $(nysia session
//! create)` is the obvious thing and works.

use std::collections::BTreeMap;
use std::io::Write;

use nysia_core::rpc::{
    Client, ClientError, Discovered, DiscoveryError, Endpoint, SpawnPolicy, discover,
};
use nysia_proto::{
    ClientId, ClientRole, ErrorEnvelope, ExitStatus, LineCursor, SessionCreate, SessionCreated,
    SessionHandle, SessionKind, SessionSummary, TerminalRead, TerminalReadResult, TerminalResize,
    TerminalSend, TerminalWait, TerminalWaitResult, WaitOutcome,
};

use crate::cli::{CreateArgs, ReadArgs, ResizeArgs, SendArgs, Verb, WaitArgs};

/// Why a verb could not be run.
///
/// Every variant renders to an [`ErrorEnvelope`], because §6.2's "every error carries next
/// steps" is about what the *caller* sees, and a caller cannot tell whether the thing that
/// failed was the socket or the verb behind it.
#[derive(Debug, thiserror::Error)]
pub enum VerbError {
    /// The daemon could not be found or started.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// The call to the daemon failed.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// An argument was not the shape the wire needs.
    #[error("{message}")]
    Argument {
        /// What was wrong.
        message: String,
        /// What to do about it.
        next_step: String,
    },
}

impl VerbError {
    /// The envelope a caller sees, whichever layer failed.
    #[must_use]
    pub fn envelope(&self) -> ErrorEnvelope {
        match self {
            Self::Discovery(err) => err.envelope(),
            Self::Client(err) => err.envelope(),
            Self::Argument { message, next_step } => ErrorEnvelope::new(
                nysia_proto::ErrorCode::InvalidRequest,
                message.clone(),
                match nysia_proto::NextSteps::new(next_step.clone()) {
                    Ok(steps) => steps,
                    // Unreachable: every call site below passes a non-blank literal, and blank
                    // is the whole of what the constructor checks.
                    Err(_) => return generic_argument_envelope(message),
                },
            ),
        }
    }

    /// An argument that was not the shape the wire needs.
    fn argument(message: impl Into<String>, next_step: impl Into<String>) -> Self {
        Self::Argument {
            message: message.into(),
            next_step: next_step.into(),
        }
    }
}

/// The fallback envelope for an argument error whose next step went missing.
fn generic_argument_envelope(message: &str) -> ErrorEnvelope {
    let steps = nysia_proto::NextSteps::new("run `nysia <verb> --help` to see the verb's flags");
    match steps {
        Ok(steps) => ErrorEnvelope::new(nysia_proto::ErrorCode::InvalidRequest, message, steps),
        // Unreachable twice over; the literal above is not blank. Reported as an argument
        // error either way, because losing the whole error tells the caller nothing at all.
        Err(_) => unreachable!("a non-blank first step is the only thing NextSteps::new asks"),
    }
}

/// Connect to the daemon, starting one unless told not to.
///
/// # Errors
///
/// Returns [`VerbError::Discovery`] when no daemon can be reached or started.
pub async fn connect(no_spawn: bool) -> Result<Client, VerbError> {
    let endpoint = Endpoint::from_env()
        .map_err(|err| VerbError::Discovery(DiscoveryError::Client(ClientError::Endpoint(err))))?;
    let policy = if no_spawn {
        SpawnPolicy::Never
    } else {
        // The CLI *is* the daemon binary (D-11), so the program to run is this very
        // executable. Resolving it from the running process rather than from PATH means a
        // `nysia` that was run by absolute path starts the daemon it came with, not whichever
        // one happens to be installed.
        match std::env::current_exe() {
            Ok(program) => SpawnPolicy::IfAbsent { program },
            Err(err) => {
                tracing::debug!(%err, "cannot locate this executable; will not start a daemon");
                SpawnPolicy::Never
            }
        }
    };
    let Discovered {
        client,
        spawned,
        lease_matches,
    } = discover(&endpoint, &client_id(), ClientRole::Control, &policy).await?;
    if spawned {
        tracing::debug!(endpoint = %endpoint.listening(), "started a daemon");
    }
    if !lease_matches {
        tracing::warn!("the daemon answering does not match the lease beside its endpoint");
    }
    Ok(client)
}

/// This process's client id.
///
/// Not an authority — §3.2 proves identity from peer credentials — so this only has to be
/// recognisable in a daemon log line and unique enough that two CLI invocations do not share
/// a stream outbox.
fn client_id() -> ClientId {
    let name = format!("nysia-cli-{}", std::process::id());
    match name.parse() {
        Ok(id) => id,
        // Unreachable: the string is a literal prefix and a decimal pid.
        Err(_) => match "nysia-cli".parse() {
            Ok(id) => id,
            Err(_) => unreachable!("`nysia-cli` is a well-formed client id"),
        },
    }
}

/// Run one verb and print its answer.
///
/// # Errors
///
/// Returns [`VerbError`] when an argument will not parse, when no daemon can be reached, or
/// when the daemon reports the verb failed.
pub async fn run(verb: Verb, no_spawn: bool) -> Result<(), VerbError> {
    let json = verb.json();
    let mut client = connect(no_spawn).await?;
    match verb {
        Verb::SessionCreate(args) => {
            let created = client.session_create(create_request(&args)?).await?;
            print_created(&created, json);
        }
        Verb::SessionList { .. } => {
            let sessions = client.session_list().await?;
            print_sessions(&sessions, json);
        }
        Verb::SessionClose { handle, .. } => {
            client.session_close(parse_handle(&handle)?).await?;
            print_done("closed", json);
        }
        Verb::TerminalRead(args) => {
            let result = client.terminal_read(read_request(&args)?).await?;
            print_read(&result, json);
        }
        Verb::TerminalSend(args) => {
            client.terminal_send(send_request(&args)?).await?;
            print_done("sent", json);
        }
        Verb::TerminalResize(args) => {
            client.terminal_resize(resize_request(&args)?).await?;
            print_done("resized", json);
        }
        Verb::TerminalWait(args) => {
            let result = client.terminal_wait(wait_request(&args)?).await?;
            print_wait(&result, json);
        }
    }
    Ok(())
}

/// Parse a session handle, saying what a good one looks like.
fn parse_handle(raw: &str) -> Result<SessionHandle, VerbError> {
    raw.parse().map_err(|err| {
        VerbError::argument(
            format!("{err}"),
            "run `nysia session list` to see the handles this daemon holds",
        )
    })
}

/// Turn `session create`'s flags into the wire request.
fn create_request(args: &CreateArgs) -> Result<SessionCreate, VerbError> {
    let pane_key = match &args.pane_key {
        Some(raw) => {
            let (tab, leaf) = raw.split_once(':').ok_or_else(|| {
                VerbError::argument(
                    format!("a pane key is `<tabId>:<leafId>`, got {raw:?}"),
                    "omit --pane-key and the daemon mints one",
                )
            })?;
            Some(nysia_proto::PaneKey::new(tab, leaf).map_err(|err| {
                VerbError::argument(err.to_string(), "omit --pane-key and the daemon mints one")
            })?)
        }
        None => None,
    };

    let mut env_overrides = BTreeMap::new();
    for entry in &args.env {
        let (key, value) = entry.split_once('=').ok_or_else(|| {
            VerbError::argument(
                format!("an --env entry is `KEY=VALUE`, got {entry:?}"),
                "pass each variable as one --env KEY=VALUE",
            )
        })?;
        env_overrides.insert(key.to_owned(), value.to_owned());
    }

    Ok(SessionCreate {
        // v0.1 serves shells. An agent session is refused by the daemon with next steps
        // naming the version that serves it, rather than by a flag this build cannot honour.
        kind: SessionKind::Shell,
        pane_key,
        profile: args
            .profile
            .map(|profile| profile.to_wire(args.distro.clone())),
        cwd: args.cwd.clone(),
        env_overrides,
        cols: args.cols,
        rows: args.rows,
    })
}

/// Turn `terminal read`'s flags into the wire request.
fn read_request(args: &ReadArgs) -> Result<TerminalRead, VerbError> {
    Ok(TerminalRead {
        handle: parse_handle(&args.handle)?,
        mode: args.mode(),
        cursor: args.cursor.map(LineCursor),
        limit: args.limit,
    })
}

/// Turn `terminal send`'s flags into the wire request.
fn send_request(args: &SendArgs) -> Result<TerminalSend, VerbError> {
    Ok(TerminalSend {
        handle: parse_handle(&args.handle)?,
        text: args.text.clone(),
        enter: args.enter,
        interrupt: args.interrupt,
    })
}

/// Turn `terminal resize`'s flags into the wire request.
fn resize_request(args: &ResizeArgs) -> Result<TerminalResize, VerbError> {
    Ok(TerminalResize {
        handle: parse_handle(&args.handle)?,
        cols: args.cols,
        rows: args.rows,
    })
}

/// Turn `terminal wait`'s flags into the wire request.
fn wait_request(args: &WaitArgs) -> Result<TerminalWait, VerbError> {
    Ok(TerminalWait {
        handle: parse_handle(&args.handle)?,
        wait_for: args.wait_for.into(),
        timeout_ms: args.timeout_ms,
    })
}

/// Write `value` to stdout as one JSON line.
fn emit_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(text) => println!("{text}"),
        Err(err) => {
            // Nothing else can be said about a value that will not serialise, and saying it
            // on stdout would put an apology where the caller is parsing an answer.
            let _ = writeln!(
                std::io::stderr(),
                "could not render the result as JSON: {err}"
            );
        }
    }
}

/// Print what `session create` minted.
fn print_created(created: &SessionCreated, json: bool) {
    if json {
        emit_json(created);
        return;
    }
    // The handle alone, so `nysia terminal read $(nysia session create)` is the obvious thing
    // and works. The rest goes to stderr, where it informs a person without polluting a pipe.
    println!("{}", created.handle);
    let _ = writeln!(
        std::io::stderr(),
        "pane {}, incarnation {}",
        created.pane_key,
        created.incarnation
    );
}

/// Print `session list`.
fn print_sessions(sessions: &[SessionSummary], json: bool) {
    if json {
        emit_json(&sessions);
        return;
    }
    if sessions.is_empty() {
        let _ = writeln!(std::io::stderr(), "no sessions");
        return;
    }
    for session in sessions {
        println!(
            "{}  {:<6}  {:<20}  {}",
            session.handle,
            kind_label(session.kind),
            session.title,
            status_label(session.exit_status.as_ref())
        );
    }
}

/// How a session's kind reads in a list.
fn kind_label(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Shell => "shell",
        SessionKind::Agent => "agent",
    }
}

/// How a session's exit status reads in a list.
fn status_label(status: Option<&ExitStatus>) -> String {
    match status {
        None => "running".to_owned(),
        Some(ExitStatus::Exited { code }) => format!("exited {code}"),
        Some(ExitStatus::Signaled { signal }) => format!("signalled {signal}"),
    }
}

/// Print `terminal read`.
fn print_read(result: &TerminalReadResult, json: bool) {
    if json {
        emit_json(result);
        return;
    }
    for line in &result.lines {
        println!("{line}");
    }
    if result.truncated {
        let _ = writeln!(
            std::io::stderr(),
            "more lines are available; read again from --cursor {}",
            result.cursor.get()
        );
    }
}

/// Print `terminal wait`.
fn print_wait(result: &TerminalWaitResult, json: bool) {
    if json {
        emit_json(result);
        return;
    }
    // The outcome, never the process's own exit code as this one's: a wait that *worked* and
    // found a failing command has done its job, and conflating the two would have a script
    // treat "the command failed" and "the wait failed" as one thing.
    match result.outcome {
        WaitOutcome::Exited {
            status: ExitStatus::Exited { code },
        } => println!("exited {code}"),
        WaitOutcome::Exited {
            status: ExitStatus::Signaled { signal },
        } => println!("signalled {signal}"),
        WaitOutcome::Idle => println!("idle"),
        WaitOutcome::TimedOut => println!("timed out"),
    }
}

/// Print a verb whose whole answer is "it happened".
fn print_done(what: &str, json: bool) {
    if json {
        emit_json(&serde_json::json!({ "ok": true }));
        return;
    }
    // Silence is success. The word goes to stderr so a pipeline sees nothing at all.
    let _ = writeln!(std::io::stderr(), "{what}");
}

/// Print an error: the envelope on stderr, whichever format was asked for.
pub fn print_error(error: &VerbError, json: bool) {
    let envelope = error.envelope();
    let mut stderr = std::io::stderr();
    if json {
        match serde_json::to_string(&envelope) {
            Ok(text) => {
                let _ = writeln!(stderr, "{text}");
            }
            Err(err) => {
                let _ = writeln!(stderr, "{envelope}\n(could not render as JSON: {err})");
            }
        }
        return;
    }
    let _ = writeln!(stderr, "error: {}", envelope.message());
    // Never optional, which is the point of §6.2: an agent told only what failed does not
    // stop, it guesses, and the guess is a flag that does not exist.
    let _ = writeln!(stderr, "next steps:");
    for step in envelope.next_steps() {
        let _ = writeln!(stderr, "  - {step}");
    }
    if let Some(args) = envelope.next_command_args() {
        let _ = writeln!(stderr, "try: {}", args.join(" "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ProfileArg;

    fn create_args() -> CreateArgs {
        CreateArgs {
            profile: None,
            distro: None,
            cwd: None,
            pane_key: None,
            env: Vec::new(),
            cols: 120,
            rows: 30,
            json: false,
        }
    }

    #[test]
    fn a_pane_key_must_be_tab_and_leaf() {
        let good = create_request(&CreateArgs {
            pane_key: Some("tab_1:leaf_1".to_owned()),
            ..create_args()
        })
        .expect("a well-formed pane key is taken");
        assert_eq!(
            good.pane_key.map(|key| key.as_str().to_owned()),
            Some("tab_1:leaf_1".to_owned())
        );

        for bad in ["no-separator", "tab only:", ":leaf only"] {
            let err = create_request(&CreateArgs {
                pane_key: Some(bad.to_owned()),
                ..create_args()
            })
            .expect_err("{bad} is not a pane key");
            assert!(!err.envelope().next_steps().is_empty());
        }
    }

    #[test]
    fn an_env_entry_must_be_key_equals_value() {
        let good = create_request(&CreateArgs {
            env: vec!["A=1".to_owned(), "B=has=equals".to_owned()],
            ..create_args()
        })
        .expect("KEY=VALUE is taken");
        assert_eq!(good.env_overrides.get("A").map(String::as_str), Some("1"));
        // Only the first `=` splits, so a value may contain one.
        assert_eq!(
            good.env_overrides.get("B").map(String::as_str),
            Some("has=equals")
        );

        let err = create_request(&CreateArgs {
            env: vec!["NOT_AN_ASSIGNMENT".to_owned()],
            ..create_args()
        })
        .expect_err("a bare name is not an assignment");
        assert!(!err.envelope().next_steps().is_empty());
    }

    #[test]
    fn a_create_from_the_cli_names_no_pane_and_asks_for_a_shell() {
        let request = create_request(&create_args()).expect("the defaults are valid");
        assert_eq!(request.kind, SessionKind::Shell);
        assert!(
            request.pane_key.is_none(),
            "the CLI has no pane; the daemon mints the key"
        );
        assert!(request.env_overrides.is_empty());
    }

    #[test]
    fn a_profile_and_its_distro_reach_the_wire_together() {
        let request = create_request(&CreateArgs {
            profile: Some(ProfileArg::Wsl),
            distro: Some("Ubuntu-24.04".to_owned()),
            ..create_args()
        })
        .expect("a wsl profile is valid");
        assert_eq!(
            request.profile,
            Some(nysia_proto::ShellProfile::Wsl {
                distro: Some("Ubuntu-24.04".to_owned())
            })
        );
    }

    #[test]
    fn a_handle_that_is_not_one_is_refused_with_somewhere_to_look() {
        let err = parse_handle("not-a-handle").expect_err("that is not a handle");
        let envelope = err.envelope();
        assert!(
            envelope
                .next_steps()
                .iter()
                .any(|step| step.contains("session list")),
            "got {:?}",
            envelope.next_steps()
        );
    }

    #[test]
    fn every_verb_error_carries_next_steps() {
        // The guarantee §6.2 asks for, checked at the layer the caller actually meets. A
        // client-side failure is no less in need of them than a daemon-side one.
        let errors = [
            VerbError::argument("bad argument", "pass a good one"),
            VerbError::Discovery(DiscoveryError::Absent {
                endpoint: "nowhere".to_owned(),
            }),
            VerbError::Client(ClientError::NoAnswer),
        ];
        for error in errors {
            assert!(!error.envelope().next_steps().is_empty(), "{error}");
        }
    }
}
