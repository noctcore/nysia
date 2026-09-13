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
use nysia_proto::session::{
    ExitStatus, SessionClose, SessionCreate, SessionCreated, SessionList, SessionSummary,
    ShellProfile,
};
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

/// Declare every fixture once, and get both the check and the printer from it.
macro_rules! goldens {
    ($($name:ident: $ty:ty = $value:expr;)*) => {
        #[test]
        fn committed_fixtures_still_describe_the_wire() {
            $(
                let value: $ty = $value;
                check::<$ty>(
                    stringify!($name),
                    &value,
                    include_str!(concat!("golden/", stringify!($name), ".json")),
                );
            )*
        }

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

    pid_record: PidRecord = identity().pid_record();

    request_session_create: RequestEnvelope = RequestEnvelope {
        request_id: request_id(),
        retry_request: None,
        payload: RequestPayload::SessionCreate(SessionCreate {
            kind: SessionKind::Shell,
            pane_key: Some(pane()),
            profile: Some(ShellProfile::Wsl { distro: Some("Ubuntu-24.04".to_owned()) }),
            cwd: Some("C:/src/nysia".into()),
            env_overrides: BTreeMap::from([
                ("NYSIA_PANE_KEY".to_owned(), "tab_1:leaf_1".to_owned()),
                ("RUST_LOG".to_owned(), "info".to_owned()),
            ]),
            cols: 120,
            rows: 30,
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
        handle: handle(),
        bytes: 524_288,
        window: CreditWindow::DEFAULT,
    });

    credit_ack: CreditFrame = CreditFrame::Ack(CreditAck { handle: handle(), bytes: 196_608 });
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
/// of showing up as "binary files differ".
#[test]
fn the_framing_still_produces_the_committed_bytes() {
    let frames = [
        Frame::new(FrameKind::Output, b"hello\r\n".as_slice()),
        Frame::empty(FrameKind::Bell),
        Frame::new(
            FrameKind::Exit,
            br#"{"outcome":"exited","code":0}"#.as_slice(),
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
