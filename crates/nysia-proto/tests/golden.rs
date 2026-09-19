//! Golden fixtures for the v0.1 wire surface.
//!
//! The unit tests prove that each type survives a round trip. They cannot prove that the
//! *shape* has not changed, because a rename on both sides round-trips perfectly and breaks
//! every peer. These fixtures are the other half: one committed JSON document per frame, so
//! a change to the wire arrives in review as a diff someone has to look at rather than as a
//! green build and a broken client.
//!
//! Two properties keep them honest:
//!
//! - **Nothing is generated.** Every id, timestamp and nonce is a literal, and
//!   `env_overrides` is a `BTreeMap`, so the JSON is byte-identical on every run and on
//!   every machine. A fixture that changed when the clock did would teach people to
//!   regenerate it without reading it.
//! - **Both directions are checked.** Serialising must produce the fixture, *and*
//!   deserialising the fixture must produce the value. Only the first would let a field the
//!   wire no longer carries live on in Rust.
//!
//! To update after a deliberate change:
//!
//! ```text
//! cd crates/nysia-proto
//! cargo test --test golden -- --ignored --nocapture print_fixtures
//! ```
//!
//! and paste each block into its file. Deliberately manual — the point is that someone
//! reads the diff.

// `clippy.toml` allows `expect` in tests, but that allowance is keyed on being *inside* a
// `#[test]` function — it does not reach the fixture helpers below, even though this whole
// file is test code and never links into the crate. Every `expect` here is on a literal
// this file also owns, so a panic is the assertion: the fixture is malformed.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use nysia_proto::agent::{
    AgentHook, AgentState, AgentStatus, AgentStatusChange, AgentStatusGet, AgentStatusList,
    AgentStatusRow, AgentStatusSubscribe, AgentStatusSubscribed, AgentStatusUnsubscribe, HookEvent,
    HookEventName, Notify, NotifySuppressed, StatusTarget, UnixMillis,
};
use nysia_proto::credit::{CreditAck, CreditFrame, CreditGrant, CreditWindow};
use nysia_proto::envelope::{
    MutationReceipt, RequestEnvelope, RequestId, RequestPayload, ResponseEnvelope, ResponsePayload,
};
use nysia_proto::error::{ErrorCode, ErrorEnvelope, NextSteps};
use nysia_proto::frame::{Frame, FrameKind, encode_into};
use nysia_proto::handshake::{
    ClientRole, DaemonIdentity, HelloAccepted, HelloRejected, HelloRequest, HelloResponse,
    PidRecord, RejectReason,
};
use nysia_proto::identity::{Incarnation, PaneKey, SessionHandle, SessionKind};
use nysia_proto::project::{
    Project, ProjectForget, ProjectId, ProjectList, ProjectRegister, ProjectRegistered,
    RegisterRefusal, Worktree,
};
use nysia_proto::session::{
    ExitStatus, SessionClose, SessionCreate, SessionCreated, SessionList, SessionSummary,
    ShellProfile, WorkingDirectory,
};
use nysia_proto::stream::{StreamAttach, StreamAttached, StreamDetach, StreamId};
use nysia_proto::tasks::{Issue, IssueState, TasksList};
use nysia_proto::terminal::{
    LineCursor, ReadMode, TerminalRead, TerminalReadResult, TerminalResize, TerminalSend,
    TerminalWait, TerminalWaitResult, WaitFor, WaitOutcome,
};
use nysia_proto::version::{PROTOCOL_VERSION, ProtocolRange, ProtocolVersion};

const HANDLE: &str = "sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60";
const REQUEST: &str = "req_11111111-1111-4111-8111-111111111111";
const RETRY: &str = "req_22222222-2222-4222-8222-222222222222";
const NONCE: &str = "9f8e7d6c-5b4a-4392-8180-7f6e5d4c3b2a";
const CREATED_AT_MS: u64 = 1_757_721_600_000;

/// The one subagent the roster fixtures carry.
const AGENT_ID: &str = "agent_7";

/// A `PreToolUse{tool_name:"AskUserQuestion"}` carrying a question, which is the one event
/// that exercises every optional field at once: the tool name §2.1 branches on, the verbatim
/// payload, and a subagent id.
fn asking() -> HookEvent {
    HookEvent {
        hook_event_name: HookEventName::PreToolUse,
        tool_name: Some("AskUserQuestion".to_owned()),
        is_interrupt: false,
        source: None,
        trigger: None,
        agent_id: Some(AGENT_ID.to_owned()),
        tool_input: Some(serde_json::json!({
            "question": "Which migration should run first?",
            "options": ["0001_init", "0002_status"],
        })),
    }
}

/// The lead's row: the pane is `done`, with nothing waiting and nothing restored.
fn lead_row() -> AgentStatusRow {
    AgentStatusRow {
        pane: pane(),
        state: AgentState::Done,
        question: None,
        is_interrupt: false,
        session_boundary: false,
        agent_id: None,
        observed_at: UnixMillis(CREATED_AT_MS),
        restored_unconfirmed: false,
    }
}

/// One roster entry, which is the same eight fields with an `agentId` filled in.
fn subagent_row() -> AgentStatusRow {
    AgentStatusRow {
        state: AgentState::Waiting,
        question: asking().tool_input,
        agent_id: Some(AGENT_ID.to_owned()),
        ..lead_row()
    }
}

/// A pane's whole status: the lead, and one subagent waiting on a question.
fn agent_status() -> AgentStatus {
    AgentStatus {
        lead: lead_row(),
        subagents: vec![subagent_row()],
    }
}

fn handle() -> SessionHandle {
    HANDLE.parse().expect("the fixture handle is well formed")
}

fn pane() -> PaneKey {
    PaneKey::new("tab_1", "leaf_1").expect("the fixture pane key is well formed")
}

fn request_id() -> RequestId {
    REQUEST
        .parse()
        .expect("the fixture request id is well formed")
}

/// The fixture project's id, parsed rather than derived.
///
/// `ProjectId::from_canonical_path` folds case on Windows and deliberately does not on
/// Unix, so a derived id is a different string on the two CI legs — and a fixture that
/// differed by platform would be a fixture nobody could commit. What the derivation does is
/// `project.rs`'s to prove; what this pins is the shape on the wire.
fn project_id() -> ProjectId {
    "proj_9a8b4bcdaa346c4da0fe52b7dd15df9f"
        .parse()
        .expect("the fixture project id is well formed")
}

/// A project with two worktrees: the primary one, and a branch carrying a session.
fn project() -> Project {
    Project {
        id: project_id(),
        name: "nysia".to_owned(),
        group: Project::DEFAULT_GROUP.to_owned(),
        worktrees: vec![
            Worktree {
                branch: "main".to_owned(),
                is_primary: true,
                sessions: Vec::new(),
            },
            Worktree {
                branch: "Shironex/projects-on-the-wire".to_owned(),
                is_primary: false,
                sessions: vec![SessionSummary {
                    handle: handle(),
                    pane_key: pane(),
                    kind: SessionKind::Agent,
                    title: "claude".to_owned(),
                    created_at_ms: CREATED_AT_MS,
                    exit_status: None,
                }],
            },
        ],
    }
}

fn identity() -> DaemonIdentity {
    DaemonIdentity {
        pid: 4242,
        started_at_ms: CREATED_AT_MS,
        launch_nonce: NONCE.parse().expect("the fixture nonce is well formed"),
        app_version: "0.1.0".to_owned(),
    }
}

/// Check `value` against its committed fixture, in both directions.
fn check<T>(name: &str, value: &T, fixture: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let actual: Value = serde_json::to_value(value).expect("the value serialises");
    let expected: Value = serde_json::from_str(fixture)
        .unwrap_or_else(|error| panic!("tests/golden/{name}.json is not valid JSON: {error}"));

    assert_eq!(
        actual,
        expected,
        "\n\n{name} no longer matches tests/golden/{name}.json.\n\
         If the wire change is deliberate, replace that file with:\n\n{}\n",
        serde_json::to_string_pretty(&actual).unwrap_or_default()
    );

    let back: T = serde_json::from_value(expected)
        .unwrap_or_else(|error| panic!("tests/golden/{name}.json does not deserialise: {error}"));
    assert_eq!(
        &back, value,
        "{name} serialises to its fixture but does not read back from it"
    );
}

fn print_fixture<T: Serialize>(name: &str, value: &T) {
    let json = serde_json::to_value(value).expect("the value serialises");
    println!("===== tests/golden/{name}.json");
    println!(
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

/// Declare every fixture once, and get both the checks and the printer from it.
///
/// **One test per fixture**, not one test over all of them. A single test stops at its
/// first `assert_eq!`, so a change to a shared type — renaming a field on `SessionHandle`,
/// say — would surface as one diff, get fixed, and reveal the next one on the following
/// run. Twenty-three fixtures could take twenty-three rounds to walk through. Split, the
/// first run names every fixture the change touched.
macro_rules! goldens {
    ($($name:ident: $ty:ty = $value:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let value: $ty = $value;
                check::<$ty>(
                    stringify!($name),
                    &value,
                    include_str!(concat!("golden/", stringify!($name), ".json")),
                );
            }
        )*

        #[test]
        #[ignore = "prints the fixtures; run it only to regenerate them after a deliberate change"]
        fn print_fixtures() {
            $(
                let value: $ty = $value;
                print_fixture(stringify!($name), &value);
            )*
        }
    };
}

goldens! {
    hello_request: HelloRequest = HelloRequest::new(
        PROTOCOL_VERSION,
        ClientRole::Control,
        "nysia-window".parse().expect("the fixture client id is well formed"),
    );

    hello_accepted: HelloResponse = HelloResponse::Accepted(HelloAccepted::new(identity()));

    hello_rejected: HelloResponse = HelloResponse::Rejected(HelloRejected::new(
        RejectReason::UnsupportedVersion {
            daemon: ProtocolVersion(1),
            attachable: ProtocolRange::attachable(),
        },
    ));

    hello_rejected_shutting_down: HelloResponse =
        HelloResponse::Rejected(HelloRejected::new(RejectReason::ShuttingDown));

    hello_rejected_unauthorized: HelloResponse = HelloResponse::Rejected(HelloRejected::new(
        RejectReason::Unauthorized {
            detail: "caller is not in a session this daemon spawned".to_owned(),
        },
    ));

    hello_rejected_malformed: HelloResponse = HelloResponse::Rejected(HelloRejected::new(
        RejectReason::Malformed { detail: "first frame was not JSON".to_owned() },
    ));

    // The forward-compatibility valve. A fixture pins what this build *writes* for it;
    // what it reads is any `kind` it does not know, which `handshake.rs` covers.
    hello_rejected_unknown: HelloResponse =
        HelloResponse::Rejected(HelloRejected::new(RejectReason::Unknown));

    pid_record: PidRecord = identity().pid_record();

    request_session_create: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::SessionCreate(SessionCreate {
            kind: SessionKind::Shell,
            pane_key: Some(pane()),
            profile: Some(ShellProfile::Wsl { distro: Some("Ubuntu-24.04".to_owned()) }),
            cwd: Some(WorkingDirectory::Path { path: "C:/src/nysia".into() }),
            env_overrides: BTreeMap::from([
                ("NYSIA_PANE_KEY".to_owned(), "tab_1:leaf_1".to_owned()),
                ("RUST_LOG".to_owned(), "info".to_owned()),
            ]),
            cols: 120,
            rows: 30,
        }),
    };

    // What the window sends: it holds a project's id and never its folder.
    request_session_create_in_project: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::SessionCreate(SessionCreate {
            kind: SessionKind::Agent,
            pane_key: Some(pane()),
            profile: None,
            cwd: Some(WorkingDirectory::Project { project: project_id() }),
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        }),
    };

    request_session_create_retry: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: Some(RETRY.parse().expect("the fixture retry id is well formed")),
        payload: RequestPayload::SessionCreate(SessionCreate {
            kind: SessionKind::Agent,
            pane_key: None,
            profile: None,
            cwd: None,
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
        }),
    };

    request_session_list: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::SessionList(SessionList {}),
    };

    request_session_close: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::SessionClose(SessionClose { handle: handle() }),
    };

    request_terminal_read_screen: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::TerminalRead(TerminalRead::screen(handle())),
    };

    request_terminal_read_stream: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::TerminalRead(TerminalRead {
            handle: handle(),
            mode: ReadMode::Stream,
            cursor: Some(LineCursor(1024)),
            limit: Some(200),
        }),
    };

    request_terminal_send: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::TerminalSend(TerminalSend {
            handle: handle(),
            text: "cargo test".to_owned(),
            enter: true,
            interrupt: true,
        }),
    };

    request_terminal_resize: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::TerminalResize(TerminalResize {
            handle: handle(),
            cols: 120,
            rows: 30,
        }),
    };

    request_terminal_wait: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::TerminalWait(TerminalWait {
            handle: handle(),
            wait_for: WaitFor::Exit,
            timeout_ms: Some(30_000),
        }),
    };

    response_session_create: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::SessionCreate(SessionCreated {
            handle: handle(),
            pane_key: pane(),
            incarnation: Incarnation::new(&pane(), 0),
        }),
    )
    .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    response_session_create_replayed: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::SessionCreate(SessionCreated {
            handle: handle(),
            pane_key: pane(),
            incarnation: Incarnation::new(&pane(), 0),
        }),
    )
    .with_receipt(MutationReceipt {
        request_id: RETRY.parse().expect("the fixture retry id is well formed"),
        replayed: true,
    });

    response_session_list: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::SessionList {
            sessions: vec![
                SessionSummary {
                    handle: handle(),
                    pane_key: pane(),
                    kind: SessionKind::Shell,
                    title: "pwsh".to_owned(),
                    created_at_ms: CREATED_AT_MS,
                    exit_status: None,
                },
                SessionSummary {
                    handle: "sess_1b2c3d4e-5f60-4a7b-8c9d-0e1f2a3b4c5d"
                        .parse()
                        .expect("the fixture handle is well formed"),
                    pane_key: PaneKey::new("tab_1", "leaf_2")
                        .expect("the fixture pane key is well formed"),
                    kind: SessionKind::Shell,
                    title: "cargo test".to_owned(),
                    created_at_ms: CREATED_AT_MS,
                    exit_status: Some(ExitStatus::Exited { code: 101 }),
                },
            ],
        },
    );

    response_terminal_read: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::TerminalRead(TerminalReadResult {
            lines: vec!["PS C:\\src\\nysia> echo hello".to_owned(), "hello".to_owned()],
            cursor: LineCursor(2),
            mode: ReadMode::Screen,
            truncated: false,
        }),
    );

    response_terminal_resize: ResponseEnvelope =
        ResponseEnvelope::new(request_id(), ResponsePayload::TerminalResize)
            .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    response_terminal_wait: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::TerminalWait(TerminalWaitResult {
            handle: handle(),
            outcome: WaitOutcome::Exited { status: ExitStatus::Signaled { signal: 15 } },
        }),
    );

    response_terminal_wait_idle: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::TerminalWait(TerminalWaitResult {
            handle: handle(),
            outcome: WaitOutcome::Idle,
        }),
    );

    response_terminal_wait_timed_out: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::TerminalWait(TerminalWaitResult {
            handle: handle(),
            outcome: WaitOutcome::TimedOut,
        }),
    );

    request_stream_attach: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::StreamAttach(StreamAttach { handle: handle() }),
    };

    request_stream_detach: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::StreamDetach(StreamDetach { stream_id: StreamId(3) }),
    };

    response_stream_attach: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::StreamAttach(StreamAttached {
            handle: handle(),
            stream_id: StreamId(3),
        }),
    )
    .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    response_stream_detach: ResponseEnvelope =
        ResponseEnvelope::new(request_id(), ResponsePayload::StreamDetach)
            .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    response_error: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(
            ErrorEnvelope::new(
                ErrorCode::UnknownSession,
                "no session sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f60",
                NextSteps::new("List the sessions the daemon currently owns.")
                    .expect("the fixture next step is not blank")
                    .and("If the daemon restarted, the handle is stale; create a new session."),
            )
            .with_next_command_args(["nysia", "session", "list"]),
        ),
    );

    credit_grant: CreditFrame = CreditFrame::Grant(CreditGrant {
        bytes: 524_288,
        window: CreditWindow::DEFAULT,
    });

    credit_ack: CreditFrame = CreditFrame::Ack(CreditAck { bytes: 196_608 });

    // The one frame on this wire whose field names are Claude's rather than Nysia's. A
    // fixture is the only thing that catches a well-meant `rename_all = "camelCase"`, which
    // round-trips in Rust and stops reading every payload Claude writes.
    request_agent_hook: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::AgentHook(AgentHook {
            pane_hint: Some(pane()),
            event: asking(),
        }),
    };

    // The spool's case: a record that could not be sent when it was written, replayed with
    // the id it was first attempted under, so the daemon records one state change.
    request_agent_hook_retry: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: Some(RETRY.parse().expect("the fixture retry id is well formed")),
        payload: RequestPayload::AgentHook(AgentHook {
            pane_hint: None,
            event: HookEvent {
                hook_event_name: HookEventName::Stop,
                is_interrupt: true,
                ..HookEvent::new(HookEventName::Stop)
            },
        }),
    };

    request_agent_status_get: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::AgentStatusGet(AgentStatusGet { pane: pane() }),
    };

    request_agent_status_list: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::AgentStatusList(AgentStatusList {}),
    };

    request_agent_status_subscribe: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::AgentStatusSubscribe(AgentStatusSubscribe {}),
    };

    request_agent_status_unsubscribe: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::AgentStatusUnsubscribe(AgentStatusUnsubscribe {
            stream_id: StreamId(4),
        }),
    };

    response_agent_hook: ResponseEnvelope =
        ResponseEnvelope::new(request_id(), ResponsePayload::AgentHook)
            .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    response_agent_status_get: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::AgentStatusGet { status: Some(agent_status()) },
    );

    // A pane nothing has reported for. The ordinary case for a shell, and for an agent
    // whose first hook has not fired — an absent answer rather than an error.
    response_agent_status_get_absent: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::AgentStatusGet { status: None },
    );

    response_agent_status_list: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::AgentStatusList { statuses: vec![agent_status()] },
    );

    response_agent_status_subscribe: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::AgentStatusSubscribe(AgentStatusSubscribed { stream_id: StreamId(4) }),
    )
    .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    // The payload of a `FrameKind::AgentStatus` frame. It has no envelope, so this is the
    // only fixture that pins what a subscriber reads — including the notification decision,
    // which is on the wire precisely so the window does not have to re-derive it.
    agent_status_frame: AgentStatusChange = AgentStatusChange {
        status: agent_status(),
        changed: StatusTarget::Subagent { agent_id: AGENT_ID.to_owned() },
        notify: Notify::Permitted,
    };

    request_project_register: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::ProjectRegister(ProjectRegister {
            // Forward slashes and no drive letter, so the fixture is one document on both
            // runners. What a path *means* is the daemon's business; what the wire carries
            // is a string, and that is what this pins.
            path: "/src/nysia".into(),
        }),
    };

    request_project_list: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::ProjectList(ProjectList {}),
    };

    request_project_forget: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::ProjectForget(ProjectForget { id: project_id() }),
    };

    response_project_register: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::ProjectRegister(ProjectRegistered {
            project: project(),
            already_registered: false,
        }),
    )
    .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    // §3.2's idempotency, and the one fixture that shows `alreadyRegistered` and `replayed`
    // are different questions: this is a *first* attempt — no retry id, `replayed: false` —
    // that found the path already registered by somebody else, days ago.
    response_project_register_already: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::ProjectRegister(ProjectRegistered {
            project: project(),
            already_registered: true,
        }),
    )
    .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    response_project_list: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::ProjectList { projects: vec![project()] },
    );

    response_project_forget: ResponseEnvelope =
        ResponseEnvelope::new(request_id(), ResponsePayload::ProjectForget)
            .with_receipt(MutationReceipt { request_id: request_id(), replayed: false });

    // The three refusals §3.2 asks to be tellable apart. Fixtures rather than unit
    // assertions because "tellable apart" is a claim about what a *peer* reads: the code it
    // branches on and the steps it shows are the whole answer, and a reworded step that
    // dropped one is a change somebody should see in a diff.
    response_error_not_a_repository: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(RegisterRefusal::NotARepository.into_envelope()),
    );

    // Eight repositories with five named: the cap, and the total stated beside it.
    response_error_many_repositories: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(
            RegisterRefusal::ManyRepositories {
                found: ["nysia", "orca", "valve", "nightcore", "kirei", "pstack", "vt", "relay"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            }
            .into_envelope(),
        ),
    );

    response_error_path_unreadable: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(RegisterRefusal::Unreadable.into_envelope()),
    );

    request_tasks_list: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::TasksList(TasksList { project: project_id() }),
    };

    // The shape the Tasks screen has been parsing out of a hand-written mirror since it
    // shipped, now spelled by Rust (D-13). Pinned as a fixture because the window's reader
    // refuses a row missing `number`, `title`, `updatedAt` or `url` — so a rename here does
    // not break a test over there, it empties the screen at runtime.
    //
    // The two flattened fields are the point of the document: `author` is a **login string**
    // where gh sends an object, and `labels` are **names** where gh sends objects with a hex
    // colour. Both are decided by the daemon, and both would be invisible in a Rust-side
    // round trip that never saw gh's shape.
    response_tasks_list: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::TasksList {
            issues: vec![
                Issue {
                    number: 200,
                    title: "The issue list, served from the daemon".to_owned(),
                    state: IssueState::Open,
                    updated_at: "2026-09-16T09:12:44Z".to_owned(),
                    url: "https://github.com/Shironex/nysia/issues/200".to_owned(),
                    author: Some("Shironex".to_owned()),
                    labels: vec!["area:daemon".to_owned(), "area:tasks".to_owned()],
                },
                // An issue whose author's account is gone, and which carries no labels.
                // GitHub really does answer this way, and the window renders the row without
                // a name rather than refusing the list — so `null` has to be in a fixture,
                // or the only spelling anybody ever sees is the one with an author.
                Issue {
                    number: 87,
                    title: "A row whose author GitHub will not name".to_owned(),
                    state: IssueState::Open,
                    updated_at: "2026-09-02T17:40:11Z".to_owned(),
                    url: "https://github.com/Shironex/nysia/issues/87".to_owned(),
                    author: None,
                    labels: Vec::new(),
                },
            ],
        },
    );

    // **An empty list is a fact about the repository, not a failure**, and this is the
    // fixture that says so on the wire. The three refusals below it are the other three
    // endings; a client that could not tell this document from one of those is the lie the
    // whole verb exists to prevent.
    response_tasks_list_empty: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::TasksList { issues: Vec::new() },
    );

    // The three the Tasks screen draws distinct headings for. Fixtures rather than unit
    // assertions for the reason the registration refusals are: "tellable apart" is a claim
    // about what a *peer* reads, and the code it branches on is the whole answer.
    response_error_gh_missing: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(ErrorEnvelope::new(
            ErrorCode::GhMissing,
            "the GitHub CLI is not installed, so issues cannot be listed",
            NextSteps::new("Install the GitHub CLI from https://cli.github.com.")
                .expect("a literal next step")
                .and("Then run `gh auth login` to sign in."),
        )),
    );

    response_error_gh_unauthenticated: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(
            ErrorEnvelope::new(
                ErrorCode::GhUnauthenticated,
                "the GitHub CLI has no credentials for this repository",
                NextSteps::new("Run `gh auth login` to sign in.")
                    .expect("a literal next step")
                    .and("If you signed in before, the token may have expired; sign in again."),
            )
            .with_next_command_args(["gh", "auth", "login"]),
        ),
    );

    response_error_query_failed: ResponseEnvelope = ResponseEnvelope::new(
        request_id(),
        ResponsePayload::Error(
            ErrorEnvelope::new(
                ErrorCode::QueryFailed,
                "GitHub did not answer the issue query",
                NextSteps::new("Check your network connection and try again.")
                    .expect("a literal next step")
                    .and("If you are signed in and online, GitHub may be rate-limiting you."),
            )
            .retryable(true),
        ),
    );

    // The rule §2.1 puts in bold, pinned where a client actually reads it.
    agent_status_frame_boundary: AgentStatusChange = AgentStatusChange {
        status: AgentStatus::new(AgentStatusRow {
            session_boundary: true,
            ..lead_row()
        }),
        changed: StatusTarget::Lead,
        notify: Notify::Suppressed { reason: NotifySuppressed::SessionBoundary },
    };
}

/// Trap 12: every gate ships with a proof that it trips.
///
/// A fixture comparison that silently passed would be worse than no fixtures, because it
/// would read as evidence the wire had not moved. This exercises both halves of [`check`]'s
/// job against a fixture that is committed and known good.
#[test]
fn the_fixture_comparison_catches_a_wire_change() {
    let hello = HelloRequest::new(
        PROTOCOL_VERSION,
        ClientRole::Control,
        "nysia-window"
            .parse()
            .expect("the fixture client id is well formed"),
    );
    let committed: Value = serde_json::from_str(include_str!("golden/hello_request.json"))
        .expect("the committed fixture is valid JSON");
    let live = serde_json::to_value(&hello).expect("the value serialises");
    assert_eq!(live, committed, "the fixture under test must start clean");

    // A renamed field: round-trips perfectly in Rust, breaks every peer.
    let mut renamed = committed.clone();
    let object = renamed.as_object_mut().expect("the fixture is an object");
    let value = object
        .remove("clientId")
        .expect("the fixture has a clientId");
    object.insert("client_id".to_owned(), value);
    assert_ne!(live, renamed, "a renamed field must fail the comparison");

    // A retyped field: same key, different shape.
    let mut retyped = committed.clone();
    retyped["version"] = Value::String("1".to_owned());
    assert_ne!(live, retyped, "a retyped field must fail the comparison");

    // And the reverse direction, which catches a field Rust still has and the wire lost.
    let mut dropped = committed;
    dropped
        .as_object_mut()
        .expect("the fixture is an object")
        .remove("role");
    assert!(
        serde_json::from_value::<HelloRequest>(dropped).is_err(),
        "a dropped field must fail the read-back"
    );
}

/// The binary framing has no JSON form, so its golden is the bytes themselves.
///
/// Hex rather than a `.bin`, so a change to the header layout is legible in a diff instead
/// of showing up as "binary files differ". It pins byte order independently of the codec's
/// own helpers: the literal `0000000b` below is what proves the stream id is big-endian, and
/// it would still catch a swap that `encode` and `decode` agreed on between themselves.
///
/// The frames deliberately use two different stream ids, so the golden covers the
/// multiplexed case rather than a single-session one, and the last of them is the replay
/// boundary — nine bytes and no payload, which is the entire message. A marker whose kind
/// byte moved would be read as some other kind by a peer and the boundary would land in the
/// wrong place, so it is pinned here rather than left to the enum's own round trip. The
/// status frame after it is pinned for the same reason, on its own stream id.
#[test]
fn the_framing_still_produces_the_committed_bytes() {
    let frames = [
        Frame::new(FrameKind::Output, StreamId(11), b"hello\r\n".as_slice()),
        Frame::empty(FrameKind::Bell, StreamId(258)),
        Frame::new(
            FrameKind::Exit,
            StreamId(11),
            br#"{"outcome":"exited","code":0}"#.as_slice(),
        ),
        Frame::empty(FrameKind::ReplayEnd, StreamId(11)),
        // Byte 7, on a stream id a subscription minted rather than an attach. It is pinned
        // here for `ReplayEnd`'s reason and one of its own: the kind is additive only as
        // long as it stays off session streams, so the byte a router keys that rule on is
        // worth a golden.
        Frame::new(
            FrameKind::AgentStatus,
            StreamId(12),
            br#"{"notify":{"decision":"permitted"}}"#.as_slice(),
        ),
    ];
    let mut wire = Vec::new();
    for frame in &frames {
        encode_into(frame, &mut wire).expect("the fixture frames are within the ceiling");
    }

    let actual: String = wire.iter().map(|byte| format!("{byte:02x}")).collect();
    let expected = include_str!("golden/frames.hex").trim();
    assert_eq!(
        actual, expected,
        "\n\nthe framing changed. If that is deliberate, replace tests/golden/frames.hex \
         with:\n\n{actual}\n"
    );
}
