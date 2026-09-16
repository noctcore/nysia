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
    AgentStatus, AgentStatusRow, ClientId, ClientRole, ErrorEnvelope, ExitStatus, Issue,
    LineCursor, PaneKey, Project, ProjectId, ProjectRegistered, ProjectStart, ProjectStarted,
    SessionCreate, SessionCreated, SessionHandle, SessionKind, SessionSummary, TerminalRead,
    TerminalReadResult, TerminalResize, TerminalSend, TerminalWait, TerminalWaitResult, UnixMillis,
    WaitOutcome,
};

use crate::cli::{
    CreateArgs, ReadArgs, RegisterArgs, ResizeArgs, SendArgs, StartArgs, StatusArgs, Verb, WaitArgs,
};

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
    ///
    /// `pub(crate)` because `nysia hook` builds one too: it is not a verb, and it still owes a
    /// caller §6.2's next steps. Sharing the constructor is also what keeps the hook free of a
    /// second, hand-rolled envelope builder — see `crate::hook::refusal`.
    pub(crate) fn argument(message: impl Into<String>, next_step: impl Into<String>) -> Self {
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
pub fn client_id() -> ClientId {
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
    // The arguments are checked *before* a daemon is needed. Finding out that a handle is
    // malformed should not cost a process: without this ordering, `nysia terminal read
    // not-a-handle` starts a whole daemon and then refuses the handle it already had in hand.
    let request = prepare(verb)?;
    let mut client = connect(no_spawn).await?;
    match request {
        Prepared::SessionCreate(request) => {
            let created = client.session_create(request).await?;
            print_created(&created, json);
        }
        Prepared::SessionList => {
            let sessions = client.session_list().await?;
            print_sessions(&sessions, json);
        }
        Prepared::SessionClose(handle) => {
            client.session_close(handle).await?;
            print_done("closed", json);
        }
        Prepared::TerminalRead(request) => {
            let result = client.terminal_read(request).await?;
            print_read(&result, json);
        }
        Prepared::TerminalSend(request) => {
            client.terminal_send(request).await?;
            print_done("sent", json);
        }
        Prepared::TerminalResize(request) => {
            client.terminal_resize(request).await?;
            print_done("resized", json);
        }
        Prepared::TerminalWait(request) => {
            let result = client.terminal_wait(request).await?;
            print_wait(&result, json);
        }
        Prepared::AgentStatus(pane) => {
            // One shape, always: an array, filtered to one pane when a pane was named. A verb
            // that answered with an object here and an array there would be one a caller has
            // to branch on before it can read either.
            let statuses = match pane {
                Some(pane) => client.agent_status_get(pane).await?.into_iter().collect(),
                None => client.agent_status_list().await?,
            };
            print_statuses(&statuses, json);
        }
        Prepared::ProjectRegister(path) => {
            let registered = client.project_register(path).await?;
            print_registered(&registered, json);
        }
        Prepared::ProjectList => {
            let projects = client.project_list().await?;
            print_projects(&projects, json);
        }
        Prepared::ProjectForget(id) => {
            client.project_forget(id).await?;
            print_done("forgotten", json);
        }
        Prepared::ProjectStart(request) => {
            let started = client.project_start(request).await?;
            print_started(&started, json);
        }
        Prepared::TasksList(project) => {
            let issues = client.tasks_list(project).await?;
            print_issues(&issues, json);
        }
    }
    Ok(())
}

/// A verb whose arguments have been checked and turned into the wire request.
///
/// The type exists to make the ordering above impossible to get wrong again: there is no way
/// to reach the socket holding anything but a request that already parsed.
#[derive(Debug)]
enum Prepared {
    SessionCreate(SessionCreate),
    SessionList,
    SessionClose(SessionHandle),
    TerminalRead(TerminalRead),
    TerminalSend(TerminalSend),
    TerminalResize(TerminalResize),
    TerminalWait(TerminalWait),
    AgentStatus(Option<PaneKey>),
    ProjectRegister(std::path::PathBuf),
    ProjectList,
    ProjectForget(ProjectId),
    ProjectStart(ProjectStart),
    TasksList(ProjectId),
}

/// Check a verb's arguments, without touching the socket.
fn prepare(verb: Verb) -> Result<Prepared, VerbError> {
    Ok(match verb {
        Verb::SessionCreate(args) => Prepared::SessionCreate(create_request(&args)?),
        Verb::SessionList { .. } => Prepared::SessionList,
        Verb::SessionClose { handle, .. } => Prepared::SessionClose(parse_handle(&handle)?),
        Verb::TerminalRead(args) => Prepared::TerminalRead(read_request(&args)?),
        Verb::TerminalSend(args) => Prepared::TerminalSend(send_request(&args)?),
        Verb::TerminalResize(args) => Prepared::TerminalResize(resize_request(&args)?),
        Verb::TerminalWait(args) => Prepared::TerminalWait(wait_request(&args)?),
        Verb::AgentStatus(args) => Prepared::AgentStatus(status_pane(&args)?),
        Verb::ProjectRegister(args) => Prepared::ProjectRegister(register_path(&args)?),
        Verb::ProjectList { .. } => Prepared::ProjectList,
        Verb::ProjectForget { id, .. } => Prepared::ProjectForget(parse_project_id(&id)?),
        Verb::ProjectStart(args) => Prepared::ProjectStart(start_request(&args)?),
        Verb::TasksList { id, .. } => Prepared::TasksList(parse_project_id(&id)?),
    })
}

/// Turn `project start`'s flags into the wire request.
///
/// The branch is checked for emptiness and **passed through otherwise**. What a branch may
/// be called is `git check-ref-format`'s answer, which the daemon asks git for — restating a
/// subset of those rules here would refuse names git accepts, in the client, where the person
/// cannot see why.
fn start_request(args: &StartArgs) -> Result<ProjectStart, VerbError> {
    if args.branch.trim().is_empty() {
        return Err(VerbError::argument(
            "a branch to start cannot be blank",
            "pass the branch the worktree is keyed by, as in `--branch feat/projects`",
        ));
    }
    Ok(ProjectStart {
        project: parse_project_id(&args.id)?,
        branch: args.branch.clone(),
        // v0.1 serves shell sessions and the daemon refuses an agent one with next steps
        // naming the version that serves it. Offering `--kind agent` here would be a flag
        // this build cannot honour.
        kind: SessionKind::Shell,
        profile: args
            .profile
            .map(|profile| profile.to_wire(args.distro.clone())),
    })
}

/// The folder `project register` was pointed at.
///
/// Checked for emptiness and **not resolved**: the daemon canonicalises, because the id is
/// derived from the canonical path and two clients resolving it two ways would be two
/// projects for one folder. A blank argument is caught here rather than travelling to the
/// daemon to come back as "that path does not exist", which is true and unhelpful.
fn register_path(args: &RegisterArgs) -> Result<std::path::PathBuf, VerbError> {
    if args.path.as_os_str().is_empty() {
        return Err(VerbError::argument(
            "a folder to register cannot be blank",
            "pass the folder to register, as in `nysia project register .`",
        ));
    }
    Ok(args.path.clone())
}

/// Parse a project id, saying what a good one looks like.
fn parse_project_id(raw: &str) -> Result<ProjectId, VerbError> {
    raw.parse().map_err(|err| {
        VerbError::argument(
            format!("{err}"),
            "run `nysia project list` to see the ids this daemon holds",
        )
    })
}

/// Which pane `agent status` was asked about, if it was asked about one.
fn status_pane(args: &StatusArgs) -> Result<Option<PaneKey>, VerbError> {
    args.pane
        .as_deref()
        .map(|raw| {
            raw.parse::<PaneKey>().map_err(|err| {
                VerbError::argument(
                    err.to_string(),
                    "a pane key is `<tabId>:<leafId>`; omit --pane for every pane",
                )
            })
        })
        .transpose()
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

/// Print what `project register` did.
///
/// Without `--json`, the id alone on stdout — so `nysia project forget $(nysia project
/// register .)` is the obvious thing and works, which is the same reason `session create`
/// prints its handle alone. Whether the folder was **already** a project goes to stderr: it
/// is the thing a person needs to be told and the thing a pipe must not be given.
fn print_registered(registered: &ProjectRegistered, json: bool) {
    if json {
        emit_json(registered);
        return;
    }
    println!("{}", registered.project.id);
    let _ = writeln!(
        std::io::stderr(),
        "{} {}",
        if registered.already_registered {
            "already registered:"
        } else {
            "registered"
        },
        registered.project.name
    );
}

/// Print `project list`.
///
/// The JSON shape is the one `crates/nysia/tests/projects.rs` pins: a bare array on stdout,
/// one object per project, each with a string `id` — the same shape `session list --json`
/// uses. That test was written before this code and may not be bent to fit it.
fn print_projects(projects: &[Project], json: bool) {
    if json {
        emit_json(&projects);
        return;
    }
    if projects.is_empty() {
        let _ = writeln!(std::io::stderr(), "no projects");
        return;
    }
    for project in projects {
        println!(
            "{}  {:<20}  {:<8}  {}",
            project.id,
            project.name,
            project.group,
            branches(project)
        );
    }
}

/// Print `tasks list`.
///
/// The JSON shape is `project list`'s: a **bare array** on stdout, one object per issue. That
/// is also what the window's reader takes, so `nysia tasks list --json` and the Tasks screen
/// are reading the same document — which is what makes the CLI a real test of the verb
/// rather than a second rendering of it.
///
/// **An empty list is not silence.** It prints `[]` under `--json` and says "no open issues"
/// otherwise, because the whole point of the verb is that a repository with nothing to do and
/// a machine that could not ask are different answers. A refusal never reaches here at all —
/// it arrives as a `ClientError` carrying the daemon's own code and next steps.
fn print_issues(issues: &[Issue], json: bool) {
    if json {
        emit_json(&issues);
        return;
    }
    if issues.is_empty() {
        let _ = writeln!(std::io::stderr(), "no open issues");
        return;
    }
    for issue in issues {
        println!(
            "{:<6}  {:<12}  {:<50}  {}",
            issue.number,
            // The login, or a word rather than a blank: GitHub reports no author for a
            // deleted account, and an empty column reads as a rendering fault.
            plain(issue.author.as_deref().unwrap_or("(no author)")),
            plain(&issue.title),
            plain(&issue.labels.join(", "))
        );
    }
}

/// Somebody else's text, with the characters a terminal acts on rather than shows.
///
/// Every field [`print_issues`] writes but the number is text out of a repository this machine
/// does not own, and `println!` goes to a terminal. The daemon already scrubs `GH_FORCE_TTY`
/// and `CLICOLOR_FORCE` out of gh's environment for precisely this reason — so that escape
/// sequences cannot reach a stream something else parses — and nothing was doing the same for
/// the fields on their way back out.
///
/// # What was measured, and what was not
///
/// **Not measured against GitHub.** 1,000 real titles from `cli/cli` and `microsoft/vscode`
/// were scanned and not one carried a C0, a DEL or a C1 character, so whether GitHub would
/// store such a title is unproven in both directions — and finding out would mean writing to
/// somebody's repository, which is not a thing to do to answer a question. So this is reasoned
/// rather than demonstrated: the text is another repository's, the sink is a terminal, and one
/// pass over a string is cheap enough not to need an exploit first.
///
/// 23 of those 1,000 titles carry legitimate non-ASCII, which is why this removes **only the
/// control classes**: `char::is_control` is Unicode `Cc`, exactly C0, DEL and C1. A title in
/// Japanese, or with an emoji in it, comes through untouched.
///
/// **Bidirectional overrides are deliberately not handled.** U+202E and its neighbours can
/// reorder a rendered line without being control characters, and they are a real spoofing
/// class — but they are category `Cf`, alongside the zero-width joiner that ordinary emoji
/// sequences are built from, so removing that category wholesale would corrupt titles that are
/// merely expressive. Narrowing it to the override range is a separate change with its own
/// evidence, and claiming it here without doing it would be worse than leaving it named.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
}

/// A project's worktrees, as one line names them.
///
/// The primary first and marked, because that is the checkout the project was registered
/// from and the one a person is usually looking for. An empty list is said in words rather
/// than left blank: it means git did not describe the repository — it was not reached in
/// time, or the folder is no longer one — and a blank column reads as "no information" when
/// the information is that there is a problem.
fn branches(project: &Project) -> String {
    if project.worktrees.is_empty() {
        return "(git did not describe it)".to_owned();
    }
    let mut named: Vec<String> = project
        .worktrees
        .iter()
        .map(|worktree| {
            if worktree.is_primary {
                format!("{}*", worktree.branch)
            } else {
                worktree.branch.clone()
            }
        })
        .collect();
    named.sort_by_key(|branch| !branch.ends_with('*'));
    named.join(", ")
}

/// Print what `project start` opened.
///
/// The handle alone on stdout, so `nysia terminal read $(nysia project start …)` works — the
/// same contract `session create` keeps. The branch, whether the worktree was adopted, and
/// the pane go to stderr, where they inform a person without polluting a pipe.
fn print_started(started: &ProjectStarted, json: bool) {
    if json {
        emit_json(started);
        return;
    }
    println!("{}", started.handle);
    let _ = writeln!(
        std::io::stderr(),
        "{} worktree on {}, pane {}",
        if started.adopted {
            "adopted the"
        } else {
            "created a"
        },
        started.branch,
        started.pane_key
    );
}

/// Print `agent status`.
///
/// The JSON shape is the one `crates/nysia/tests/agent_status.rs` pins: a bare array on
/// stdout, one entry per pane, each an `AgentStatus` — so `lead.pane` names the pane and
/// `lead.state` is its dot. It is the same shape the window and, later, the phone read,
/// because it is the same verb.
fn print_statuses(statuses: &[AgentStatus], json: bool) {
    if json {
        emit_json(&statuses);
        return;
    }
    if statuses.is_empty() {
        let _ = writeln!(std::io::stderr(), "no agent status");
        return;
    }
    let now = now_ms();
    for status in statuses {
        println!("{}", status_line(&status.lead, now));
        for subagent in &status.subagents {
            println!("  {}", status_line(subagent, now));
        }
    }
}

/// One status row as a person reads it.
///
/// Staleness is spelled out here rather than stored, because §2.3's decay is derived at read
/// time and "active" is not a fifth state — a row that says `working` and is half an hour old
/// is still `working`, and the only thing that changed is the clock it is being read against.
fn status_line(row: &AgentStatusRow, now: UnixMillis) -> String {
    let mut line = format!("{:<12}  {}", row.state, row.pane);
    if let Some(agent_id) = &row.agent_id {
        line.push_str(&format!("  agent {agent_id}"));
    }
    if row.is_stale(now) {
        line.push_str("  (stale)");
    }
    if row.restored_unconfirmed {
        line.push_str("  (restored, unconfirmed)");
    }
    if row.session_boundary {
        line.push_str("  (session boundary)");
    }
    line
}

/// This process's clock, for the read-time staleness above.
fn now_ms() -> UnixMillis {
    UnixMillis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
    )
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

    #[test]
    fn another_repositorys_text_cannot_drive_the_terminal_it_is_printed_to() {
        // `tasks list` prints an issue's title, author and labels, and all three are text
        // from a repository this machine does not own. The daemon keeps `GH_FORCE_TTY` and
        // `CLICOLOR_FORCE` away from gh so that escapes cannot reach a parsed stream; this is
        // the same rule applied to the fields rather than to the environment.
        //
        // The sequences below are the ones that matter to a terminal and not to a reader:
        // erase-display, a cursor jump, an OSC that retitles the window, and a bell.
        let steered = plain("Fix\u{1b}[2J\u{1b}[1;1H\u{1b}]0;owned\u{7}the parser\u{7}");
        for obeyed in ['\u{1b}', '\u{7}'] {
            assert!(
                !steered.contains(obeyed),
                "{obeyed:?} survived into a line a terminal reads: {steered:?}"
            );
        }
        // A C1 introducer is the same attack in one byte, and it is a control character by
        // the same definition rather than by a second list someone has to remember.
        assert!(!plain("Fix\u{9b}2Jthe parser").contains('\u{9b}'));
        // And a newline, which would otherwise turn one row into two and let a title forge a
        // row of its own.
        assert!(!plain("Fix the parser\n500   nobody       anything at all").contains('\n'));

        // What must **not** change: 23 of 1,000 real titles carry non-ASCII, and a guard that
        // flattened them would be a bug reported by everybody rather than an attack stopped.
        for ordinary in [
            "修正: パーサーを直す",
            "Fix the parser 🎉",
            "Use an em dash — like this",
        ] {
            assert_eq!(plain(ordinary), ordinary);
        }
    }
}
