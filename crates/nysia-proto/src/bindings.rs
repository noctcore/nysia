//! The half of the TypeScript bindings ts-rs cannot express.
//!
//! Two things live here. Most of it is **values**: ts-rs exports *types*, and it has no
//! mechanism for exporting a value, but parts of this wire are numeric — the frame kind
//! bytes, the header size, the payload ceiling, the protocol version, the credit-window
//! defaults. A TypeScript decoder needs those numbers at runtime.
//!
//! The other thing is **[`OPEN_REJECT_REASON`]**, a type rather than a value, and it is
//! here for the same reason: ts-rs cannot widen a tagged enum. See that constant for the
//! five routes that were tried and why each one is a dead end.
//!
//! The alternatives are both worse. Hand-writing them in the transport layer is five magic
//! numbers copied across a language boundary — exactly the drift D-13 exists to prevent,
//! and worse than a type mismatch, because a wrong kind byte misroutes binary data instead
//! of failing to compile. Sending them over the wire cannot work for the frame kinds at
//! all: you need the kind byte to read the frame that would have carried it.
//!
//! So this module renders both — the constants the rest of the crate already defines, and
//! the one union ts-rs cannot widen — into one TypeScript module, and a test writes it
//! beside the ts-rs output. Three things make that a single source of truth, not a second:
//!
//! - Every number comes from the same Rust definition the daemon uses. Nothing here
//!   restates a value.
//! - The generated module `satisfies` the ts-rs types, so if the byte table and the
//!   `FrameKind` union ever disagree, `pnpm typecheck` fails on the generated file itself.
//! - It lands under the existing drift guard for free — `scripts/ts-drift.ts` compares
//!   every `.ts` file in the generated directory — and `scripts/prove-ts-drift.ts` proves
//!   the guard covers *this* file, not merely the type files beside it.
//!
//! The one thing Rust cannot reflect is a struct's field names, so
//! [`CREDIT_WINDOW_FIELDS`] spells them out. A test asserts that table is exactly what
//! serde produces for [`CreditWindow::DEFAULT`], key for key and value for value, so a
//! renamed or added field cannot quietly miss the export.

use crate::credit::CreditWindow;
use crate::frame::{FRAME_HEADER_BYTES, FrameKind, MAX_FRAME_PAYLOAD_BYTES};
use crate::version::{MIN_ATTACHABLE_PROTOCOL_VERSION, PROTOCOL_VERSION};

/// The default credit window's fields, in the camelCase spelling the wire uses.
///
/// The values are read from [`CreditWindow::DEFAULT`]; only the spellings are written here,
/// because Rust has no way to reflect them. `credit_window_fields_match_serde` is what keeps
/// the spellings honest.
const CREDIT_WINDOW_FIELDS: [(&str, u32); 7] = [
    ("perStreamInitial", CreditWindow::DEFAULT.per_stream_initial),
    ("perStreamMax", CreditWindow::DEFAULT.per_stream_max),
    ("totalInitial", CreditWindow::DEFAULT.total_initial),
    ("totalMax", CreditWindow::DEFAULT.total_max),
    ("pendingCap", CreditWindow::DEFAULT.pending_cap),
    ("ackBatch", CreditWindow::DEFAULT.ack_batch),
    ("chunk", CreditWindow::DEFAULT.chunk),
];

/// The name of the generated module, as it appears in `apps/web/src/generated`.
///
/// camelCase, where every ts-rs file is PascalCase, so it reads as "not a type" at a glance
/// in the directory listing and in an import.
pub const CONSTANTS_FILE_NAME: &str = "wireConstants.ts";

/// The open form of `RejectReason`, appended to the generated module.
///
/// `RejectReason` is open in Rust — [`crate::RejectReason::Unknown`] absorbs any `kind` a
/// newer daemon sends — but the union ts-rs exports is closed, which is the TypeScript twin
/// of the bug that valve fixed. A web client that writes an exhaustive `switch` with an
/// `assertNever` default compiles today and throws at runtime the first time a daemon it
/// does not recognise refuses it.
///
/// It is here rather than on `RejectReason` itself because ts-rs 12 cannot widen a tagged
/// enum, and every route was tried:
///
/// - a container `#[ts(type = …)]` is rejected outright — both `tag` and `rename_all` are
///   "not compatible with `type`", and an internally tagged enum needs both;
/// - a variant-level `#[ts(type = …)]` is parsed and then silently ignored;
/// - an extra open variant marked `#[serde(skip)]` is honoured by ts-rs and omitted;
/// - a field-level override on `HelloRejected::reason` compiles but drops the `import type`
///   for `RejectReason`, so the file it writes does not typecheck;
/// - `concat!` in the attribute fails with "expected literal", so the union cannot be
///   assembled from the variant list either.
///
/// So the closed union stays, describing exactly what *this build writes*, and the open one
/// is generated beside it describing what *may arrive*. Read a reason that came from a peer
/// through this; the closed type is right for one this build constructed itself.
const OPEN_REJECT_REASON: &str = "OpenRejectReason";

/// Render the numeric wire constants as a TypeScript module.
///
/// Pure: it returns the text and writes nothing. The test below is what puts it on disk,
/// which keeps this crate free of IO while still generating in the same
/// `cargo test export_bindings` step the ts-rs export runs in.
#[must_use]
pub fn typescript_constants() -> String {
    let mut by_name = String::new();
    let mut by_byte = String::new();
    for kind in FrameKind::ALL {
        by_name.push_str(&format!("  \"{}\": {},\n", kind.as_str(), kind.as_byte()));
        by_byte.push_str(&format!("  {}: \"{}\",\n", kind.as_byte(), kind.as_str()));
    }

    let mut window = String::new();
    for (field, value) in CREDIT_WINDOW_FIELDS {
        window.push_str(&format!("  {field}: {value},\n"));
    }

    format!(
        "\
// This file was generated by nysia-proto's `bindings` module. Do not edit this file
// manually. ts-rs exports types and not values, so the numbers a TypeScript decoder needs
// at runtime are generated here from the same Rust definitions the daemon uses (D-13).
//
// Regenerate with: cd crates/nysia-proto && cargo test export_bindings
import type {{ CreditWindow }} from \"./CreditWindow\";
import type {{ FrameKind }} from \"./FrameKind\";
import type {{ RejectReason }} from \"./RejectReason\";

/**
 * The protocol version this build speaks and sends in its `hello`.
 */
export const PROTOCOL_VERSION = {protocol_version};

/**
 * The oldest daemon protocol this build will attach to. A client that finds a daemon
 * outside `[MIN_ATTACHABLE_PROTOCOL_VERSION, PROTOCOL_VERSION]` must refuse rather than
 * proceed, because a mismatched frame layout corrupts sessions the daemon is still serving
 * for someone else.
 */
export const MIN_ATTACHABLE_PROTOCOL_VERSION = {min_attachable};

/**
 * The byte that names each kind in a frame header.
 *
 * Zero is deliberately absent, so a zero-filled buffer is rejected rather than read as a
 * run of empty frames. `satisfies Record<FrameKind, number>` is load-bearing: it fails the
 * typecheck if this table and the generated `FrameKind` union ever disagree.
 */
export const FRAME_KIND = {{
{by_name}}} as const satisfies Record<FrameKind, number>;

/**
 * The inverse of {{@link FRAME_KIND}}, for the decoder's hot path.
 *
 * Indexing it yields `FrameKind | undefined`: an unknown byte is an unusable stream, not a
 * kind, and the caller has to say what it does about that.
 */
export const FRAME_KIND_BY_BYTE: Readonly<Record<number, FrameKind | undefined>> = {{
{by_byte}}};

/**
 * The bytes a frame header occupies: one kind byte plus a four-byte big-endian length.
 */
export const FRAME_HEADER_BYTES = {frame_header_bytes};

/**
 * The largest payload one frame may carry. Not a limit on the writer — a legitimate frame
 * is an order of magnitude under it — but the bound that stops a corrupt length prefix
 * asking for a four-gigabyte allocation.
 */
export const MAX_FRAME_PAYLOAD_BYTES = {max_frame_payload_bytes};

/**
 * The credit window in force before the first {{@link FRAME_KIND}} `credit` grant arrives.
 *
 * A grant carries the window it is issued under, so this is a starting point rather than a
 * constant to depend on: read the window off the grant once one has arrived.
 */
export const CREDIT_WINDOW_DEFAULT = {{
{window}}} as const satisfies CreditWindow;

/**
 * A `RejectReason` as it may *arrive*, rather than as this build writes it.
 *
 * Rust absorbs an unrecognised `kind` into `RejectReason::Unknown`, so the union ts-rs
 * exports is closed — it describes what this build produces. A daemon newer than this one
 * can send a kind that is in neither list, and an exhaustive `switch` over the closed type
 * with an `assertNever` default would compile and then throw the first time that happened.
 *
 * Use this wherever a reason came from a peer. The open tail makes the default branch a
 * type error until it is handled, which is the whole point: the failure moves from runtime
 * to the compiler.
 */
export type {open} = RejectReason | {{ \"kind\": string & {{}} }};
",
        protocol_version = PROTOCOL_VERSION.get(),
        min_attachable = MIN_ATTACHABLE_PROTOCOL_VERSION.get(),
        frame_header_bytes = FRAME_HEADER_BYTES,
        max_frame_payload_bytes = MAX_FRAME_PAYLOAD_BYTES,
        open = OPEN_REJECT_REASON,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write the constants module beside the ts-rs output.
    ///
    /// Named to match the `export_bindings` filter the drift guard runs, so the values and
    /// the types are produced by one command and neither can be regenerated without the
    /// other. `TS_RS_EXPORT_DIR` is read at run time, exactly as ts-rs reads it, so the
    /// guard's redirection into a temp directory redirects this file too — which is the
    /// whole reason the guard covers it.
    #[test]
    fn export_bindings_for_wire_constants() {
        let dir = std::env::var("TS_RS_EXPORT_DIR").unwrap_or_else(|_| "./bindings".to_owned());
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("the export directory can be created");
        std::fs::write(dir.join(CONSTANTS_FILE_NAME), typescript_constants())
            .expect("the constants module can be written");
    }

    #[test]
    fn credit_window_fields_match_serde() {
        // Rust cannot reflect field names, so `CREDIT_WINDOW_FIELDS` spells them out. This
        // is what stops that table drifting from the type ts-rs exports: both come from the
        // same serde attributes, and a renamed or added field fails here rather than
        // silently vanishing from the generated module.
        let expected = serde_json::to_value(CreditWindow::DEFAULT).unwrap();
        let expected = expected.as_object().unwrap();

        assert_eq!(
            CREDIT_WINDOW_FIELDS.len(),
            expected.len(),
            "CreditWindow has {} fields but CREDIT_WINDOW_FIELDS lists {}",
            expected.len(),
            CREDIT_WINDOW_FIELDS.len()
        );
        for (field, value) in CREDIT_WINDOW_FIELDS {
            assert_eq!(
                expected.get(field).and_then(serde_json::Value::as_u64),
                Some(u64::from(value)),
                "{field} is not what serde writes for CreditWindow::DEFAULT"
            );
        }
    }

    #[test]
    fn every_frame_kind_reaches_the_generated_table() {
        let generated = typescript_constants();
        for kind in FrameKind::ALL {
            assert!(
                generated.contains(&format!("\"{}\": {},", kind.as_str(), kind.as_byte())),
                "{kind:?} is missing from FRAME_KIND"
            );
            assert!(
                generated.contains(&format!("{}: \"{}\",", kind.as_byte(), kind.as_str())),
                "{kind:?} is missing from FRAME_KIND_BY_BYTE"
            );
        }
        // The byte values are 1..=5 and zero is not a kind. A generator that emitted a
        // zero-based table would compile, typecheck, and misroute every frame.
        assert!(!generated.contains("\"output\": 0,"));
        assert!(generated.contains("\"output\": 1,"));
    }

    #[test]
    fn the_generated_module_opens_the_reject_reason_union() {
        // The TypeScript twin of the `RejectReason::Unknown` valve. Rust absorbs an
        // unrecognised kind; the exported union cannot, so a web client's exhaustive switch
        // would compile and then throw the first time a newer daemon refused it.
        let generated = typescript_constants();
        assert!(
            generated.contains(&format!(
                "export type {OPEN_REJECT_REASON} = RejectReason | {{ \"kind\": string & {{}} }};"
            )),
            "the open reject reason is missing from the generated module"
        );
        // It is a type alias over the ts-rs type, not a copy of it, so the variant shapes
        // and their doc comments stay derived and cannot drift from the Rust enum.
        assert!(generated.contains("import type { RejectReason } from \"./RejectReason\";"));
        assert!(
            !generated.contains("\"kind\": \"unauthorized\""),
            "the open form must alias RejectReason, never restate its variants"
        );
    }

    #[test]
    fn the_generated_module_carries_the_numbers_a_client_cannot_derive() {
        let generated = typescript_constants();
        assert!(generated.contains("export const PROTOCOL_VERSION = 1;"));
        assert!(generated.contains("export const MIN_ATTACHABLE_PROTOCOL_VERSION = 1;"));
        assert!(generated.contains("export const FRAME_HEADER_BYTES = 5;"));
        assert!(generated.contains("export const MAX_FRAME_PAYLOAD_BYTES = 1048576;"));
        assert!(generated.contains("  ackBatch: 196608,"));
        assert!(generated.contains("  chunk: 49152,"));
        // The `satisfies` clauses are what make a disagreement between this module and the
        // ts-rs types a typecheck failure rather than a runtime surprise.
        assert!(generated.contains("as const satisfies Record<FrameKind, number>;"));
        assert!(generated.contains("as const satisfies CreditWindow;"));
    }
}
