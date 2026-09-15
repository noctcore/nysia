//! Agent status: the hook event that arrives, the state it maps to, and the row it writes.
//!
//! The contract is `docs/plans/v0.2-delivery-plan.md` §2, which is the single authority and
//! is **cited here rather than re-derived**. Three workers touch this shape — proto spells
//! it on the wire, the store persists it, the daemon serves it — and the whole reason it
//! lives in one crate is that nobody defines a second [`AgentState`].
//!
//! # Two vocabularies, deliberately
//!
//! Everything else on this wire is camelCase, because Nysia chose the names. [`HookEvent`]
//! is **snake_case**, because Claude chose them: it is the JSON Claude writes to `nysia
//! hook`'s stdin, and renaming fields on the way in would mean something had to rewrite
//! every hook payload before the daemon could read it. So this one type speaks Claude's
//! spelling and everything downstream of it speaks Nysia's.
//!
//! Claude writes more than this type names — `session_id`, `transcript_path`, `cwd`,
//! `permission_mode` — and those are **dropped**, not carried. Nothing in §2.1's mapping
//! reads them, a transcript path is a file the daemon has no business remembering (trap
//! 13), and the hook is on the agent's critical path, so the cheapest thing it can hand the
//! daemon is the right thing. Unknown fields are ignored rather than refused for the same
//! reason: a hook that failed because Claude added a field would stop reporting status at
//! exactly the moment the version changed.
//!
//! # What is deliberately *not* here
//!
//! - **No provider abstraction.** D-3/D-4: Claude is the only agent in v1, and the trait
//!   gets extracted from two real implementations in v0.6+, not from one now.
//! - **No notification policy beyond the one rule §2.1 fixes.** [`Notify`] answers "may
//!   this row raise a notification at all"; everything it permits is the window's business
//!   (W5). It is an enum rather than a `bool` for the reason written on it.
//! - **No second streaming mechanism.** A subscription is an attach that the daemon answers
//!   with a [`StreamId`], and its frames ride the stream connection terminal output already
//!   uses. See [`AgentStatusSubscribe`].

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize, Serializer};
use ts_rs::TS;

use crate::identity::PaneKey;
use crate::stream::StreamId;

/// How many states one agent keeps, §2.3.
///
/// Copied from Orca, which proves the shape. It lives here rather than in the store because
/// the store enforces the cap and the window renders what survives it, and two numbers that
/// have to agree are one number that should only be written once (D-13).
pub const AGENT_STATUS_HISTORY_CAP: u32 = 20;

/// How long a status stays fresh, in milliseconds. §2.3: thirty minutes.
///
/// A `working` row older than this is **stale**, which the design paints as a decayed dot
/// rather than a live one. See [`AgentStatusRow::is_stale`] for why that decay is not a
/// fifth [`AgentState`].
pub const AGENT_STATUS_STALE_AFTER_MS: u64 = 30 * 60 * 1_000;

/// The `trigger` a `PostCompact` must carry to mean the agent finished (§2.1).
///
/// One place, because it is the only string in the mapping that a typo would turn into a
/// dot that never lights.
const MANUAL_COMPACT: &str = "manual";

/// The `tool_name` that turns a `PreToolUse` into `waiting` rather than `working` (§2.1).
const ASK_USER_QUESTION: &str = "AskUserQuestion";

/// Why a string could not be read as one of the agent-status types.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// An agent state was not one of the four wire spellings.
    #[error("an agent state is `working`, `waiting`, `done` or `interrupted`, got {0:?}")]
    AgentStateShape(String),
}

/// What an agent is doing, as the sidebar and the tab strip show it.
///
/// **Exactly four**, from §2.1. No others: a fifth state is a dot nobody specified a colour
/// for, and the design spec's §1 status palette has four entries for a reason.
///
/// Staleness is not a fifth state — see [`AgentStatusRow::is_stale`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentState {
    /// The agent is doing something: a prompt was submitted, or a tool ran.
    Working,
    /// The agent needs the person: a permission request, or an `AskUserQuestion`.
    Waiting,
    /// The agent stopped of its own accord, or a session boundary was crossed.
    Done,
    /// The agent stopped because it was interrupted.
    Interrupted,
}

impl AgentState {
    /// Every state, in the order §2.1's table lists them.
    ///
    /// The list the exported TypeScript union is checked against, so "exactly four" is a
    /// fact a test holds rather than a sentence in a doc comment.
    pub const ALL: [Self; 4] = [Self::Working, Self::Waiting, Self::Done, Self::Interrupted];

    /// The wire spelling, which is also what this type exports to TypeScript.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Interrupted => "interrupted",
        }
    }
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentState {
    type Err = AgentError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| AgentError::AgentStateShape(s.to_owned()))
    }
}

/// Define [`HookEventName`] and everything keyed on its variants from one list.
///
/// The same shape as `error_codes!` in [`crate::error`], and for the same reason: a list
/// restated once per function is a list where only the exhaustive matches are checked, so a
/// variant can exist and still be missing from the table a test reads. One list removes that
/// class of mistake rather than catching it.
macro_rules! hook_events {
    ($( $(#[$doc:meta])* $variant:ident => $wire:literal, )+) => {
        /// The name Claude puts in `hook_event_name`.
        ///
        /// **Open**, and that is the whole point. §5.2 puts this on the agent's critical
        /// path: a hook that refuses an event name it has not met does not merely lose one
        /// status, it stops reporting status at all the day Claude adds an event. An
        /// unrecognised name lands in [`HookEventName::Other`], maps to no state
        /// ([`HookEvent::state`] answers `None`) and is dropped — the same treatment
        /// `PreCompact` gets, which is what §2.1 specifies for an event with no row in its
        /// table.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
        #[ts(
            export,
            type = "\"UserPromptSubmit\" | \"PreToolUse\" | \"PostToolUse\" \
                    | \"PostToolUseFailure\" | \"PermissionRequest\" | \"Stop\" \
                    | \"StopFailure\" | \"SubagentStart\" | \"SubagentStop\" \
                    | \"TeammateIdle\" | \"SessionStart\" | \"PostCompact\" \
                    | \"PreCompact\" | (string & {})"
        )]
        pub enum HookEventName {
            $( $(#[$doc])* $variant, )+
            /// A name this build does not know, carried verbatim and mapped to nothing.
            Other(String),
        }

        impl HookEventName {
            /// Every name this build knows, excluding [`HookEventName::Other`].
            ///
            /// Generated from the same list as the variants, so it cannot fall behind them.
            /// `the_exported_event_union_lists_every_known_name` reads this, and that test
            /// is only worth something because this cannot lie.
            #[must_use]
            pub fn known() -> [Self; [$(stringify!($variant)),+].len()] {
                [$(Self::$variant),+]
            }

            /// The wire spelling.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Other(name) => name,
                }
            }

            /// The name a wire spelling denotes, falling back to [`HookEventName::Other`].
            ///
            /// Total by design: there is no unreadable event name, because refusing to read
            /// one would fail the hook rather than drop the event.
            #[must_use]
            pub fn from_wire(name: &str) -> Self {
                match name {
                    $($wire => Self::$variant,)+
                    other => Self::Other(other.to_owned()),
                }
            }
        }
    };
}

hook_events! {
    /// The person sent a prompt. Maps to `working`.
    UserPromptSubmit => "UserPromptSubmit",
    /// A tool is about to run. `waiting` for `AskUserQuestion`, `working` otherwise.
    PreToolUse => "PreToolUse",
    /// A tool finished. Maps to `working`.
    PostToolUse => "PostToolUse",
    /// A tool failed. Maps to `working`: the agent is still the one working on it.
    PostToolUseFailure => "PostToolUseFailure",
    /// The agent asked for permission. Maps to `waiting`.
    PermissionRequest => "PermissionRequest",
    /// The agent finished. `done`, or `interrupted` with `is_interrupt`.
    Stop => "Stop",
    /// The agent finished badly. `done`, or `interrupted` with `is_interrupt`.
    StopFailure => "StopFailure",
    /// A subagent started. Installed by §5.1, and §2.1 gives it no state.
    SubagentStart => "SubagentStart",
    /// A subagent finished. Installed by §5.1, and §2.1 gives it no state.
    SubagentStop => "SubagentStop",
    /// A teammate went idle. Installed by §5.1, and §2.1 gives it no state.
    TeammateIdle => "TeammateIdle",
    /// A session began. `done` with `session_boundary`, and it must never notify.
    SessionStart => "SessionStart",
    /// Context was compacted. `done` only when the person asked for it.
    PostCompact => "PostCompact",
    /// About to compact. **Deliberately unmapped** (§2.1) — dropping it is the specified
    /// behaviour, not an oversight, and `pre_compact_is_deliberately_unmapped` says so by
    /// name so nobody later "fixes" it.
    PreCompact => "PreCompact",
}

impl fmt::Display for HookEventName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HookEventName {
    /// Reading an event name cannot fail — an unrecognised one is
    /// [`HookEventName::Other`], not an error.
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_wire(s))
    }
}

impl Serialize for HookEventName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HookEventName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&String::deserialize(deserializer)?))
    }
}

/// A `null`, absent or blank `agent_id` reads as absent.
///
/// It is the only field that needs this, because it is the only one that is a **key**: a
/// blank one would collapse every subagent in a pane into a single row flickering between
/// their states. The other optional fields are compared against a literal, where a blank
/// already means "not that literal" and needs no help.
///
/// Normalised rather than refused, because refusing would fail the hook on the agent's
/// critical path (§5.2) and lose a status to protect a field nothing reads.
fn blank_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.filter(|value| !value.trim().is_empty()))
}

/// What `nysia hook` reads on stdin and hands the daemon.
///
/// Field names are Claude's, not Nysia's — see the module docs. Every field but the event
/// name is optional and defaulted, because a payload carries only what its event has: a
/// `Stop` has no `tool_name`, a `PreToolUse` has no `trigger`, and a type that demanded the
/// fields an event does not have would fail the moment Claude stopped writing one.
///
/// # `--event` is the CLI's business and never reaches the daemon
///
/// `hook_event_name` is **required** here, so a payload that omits it does not deserialize
/// into this type at all, and `nysia hook --event <name>` is not on the wire: [`AgentHook`]
/// carries `pane_hint` and this event, and nothing else. The daemon therefore never sees the
/// flag and cannot compare anything to it.
///
/// So the two rules below are the **CLI's**, not the wire's. They are written here because
/// this is where they will be looked for, and they are written as instructions rather than
/// guarantees because this type cannot enforce either:
///
/// 1. Fill a missing `hook_event_name` in from the flag *before* building this type.
/// 2. Refuse when the payload and the flag both name an event and disagree. A hook entry
///    wired to the wrong event name misclassifies every status it reports, and picking a
///    winner would make that silent — but the CLI holds both values at the same instant and
///    is the only place they can be compared at all.
///
/// # What the wire does guarantee
///
/// `tool_input` is dropped, in both directions, unless the event is one that maps to
/// `waiting` — see [`HookEvent::question`]. A blank `agent_id` is read as absent, in both
/// directions too — see [`HookEvent::subagent`].
///
/// Serde is hand-written here rather than derived, so those two rules have one home each;
/// the private `HookEventWire` beneath this type says what else that bought. The
/// consequence for a reader is that the
/// `#[serde(default)]` this type used to carry on every optional field now lives there — the
/// defaulting behaviour is unchanged, and a payload carrying nothing but an event name still
/// reads.
#[derive(Debug, Clone, PartialEq, TS)]
#[ts(export)]
pub struct HookEvent {
    /// Which event fired.
    pub hook_event_name: HookEventName,
    /// The tool, on the events that have one. `AskUserQuestion` is the one value §2.1
    /// branches on.
    pub tool_name: Option<String>,
    /// Whether the agent was interrupted rather than stopping on its own.
    ///
    /// The only thing separating `interrupted` from `done` (§2.2). Defaults to `false`: an
    /// event that does not say it was interrupted was not.
    pub is_interrupt: bool,
    /// `SessionStart`'s `source` — `startup`, `resume` or `clear`.
    ///
    /// Carried, never branched on: all three map alike, so a typed enum would buy nothing
    /// and would refuse a source a later Claude adds, on the critical path.
    pub source: Option<String>,
    /// `PostCompact`'s `trigger`. §2.1 maps the compaction to `done` only when it is
    /// `"manual"`, because an automatic compaction is not the agent finishing.
    pub trigger: Option<String>,
    /// The subagent this event is about, when it is about one.
    ///
    /// §2.1: any event carrying it updates the roster, and the lead keeps its own state.
    /// [`HookEvent::target`] is that rule as a shape, so a caller cannot write a subagent's
    /// state onto the lead's row by forgetting to look.
    ///
    /// Read it through [`HookEvent::subagent`], which is where a blank one becomes absent.
    pub agent_id: Option<String>,
    /// The tool's input, verbatim, and the source of the `waiting` question.
    ///
    /// Claude's spelling and Claude's shape: this is `tool_input`, which is what a
    /// `PreToolUse` actually carries, and on an `AskUserQuestion` it is the question. A
    /// field called `question` would have read a key Claude never writes, and every waiting
    /// row would have carried nothing.
    ///
    /// `unknown` in TypeScript rather than a shape: this crate has no authority over the
    /// fields of a tool's input, and a type that claimed to know them would be a wire
    /// contract Nysia cannot hold up. A reader narrows it.
    ///
    /// **It does not reach the wire unless the event is one that maps to `waiting`.** Read
    /// it through [`HookEvent::question`], which is where that rule and its reason are; the
    /// field itself is what a caller who just read stdin is holding, before the wire has had
    /// a say.
    #[ts(type = "unknown")]
    pub tool_input: Option<serde_json::Value>,
}

/// The shape serde actually reads and writes for a [`HookEvent`].
///
/// Private, and it does two jobs that happen to have one answer.
///
/// **It is where `tool_input` is confined.** §2.2 wants the `waiting` payload and nothing
/// else, and doing that only in [`HookEvent::to_row`] would confine the *row* while leaving
/// the *frame* fat: a `PreToolUse{tool_name:"Write"}` carries the whole file being written,
/// the control line cap is a megabyte, and the status would be lost to
/// `ControlError::Truncated` exactly where the payload was never wanted. Applying it here
/// means a caller cannot fill the field wrongly and send it, because the value never reaches
/// the socket — and the daemon applies the same rule on the way in, so neither end has to
/// trust the other. [`HookEvent::to_row`] keeps its own check: a `HookEvent` built in
/// process never passed through serde at all.
///
/// **It is also where `deserialize_with` lives.** ts-rs 12 cannot parse that attribute and
/// prints `warning: failed to parse serde attribute` on every build of the crate carrying
/// it. This struct derives no `TS`, so the warning has nowhere to come from, and the
/// exported TypeScript is unchanged — it is derived from [`HookEvent`], which keeps the
/// field list and the doc comments.
///
/// The field list is restated once, and the compiler holds the two together: every field of
/// [`HookEvent`] is named in the conversions below, so adding one to either is a build
/// failure rather than a field that silently stops crossing the wire.
#[derive(Deserialize)]
struct HookEventWire {
    hook_event_name: HookEventName,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    is_interrupt: bool,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default, deserialize_with = "blank_as_none")]
    agent_id: Option<String>,
    #[serde(default)]
    tool_input: Option<serde_json::Value>,
}

impl Serialize for HookEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut event = serializer.serialize_struct("HookEvent", 7)?;
        event.serialize_field("hook_event_name", &self.hook_event_name)?;
        event.serialize_field("tool_name", &self.tool_name)?;
        event.serialize_field("is_interrupt", &self.is_interrupt)?;
        event.serialize_field("source", &self.source)?;
        event.serialize_field("trigger", &self.trigger)?;
        // The two confinements, applied on the way out rather than trusted to the caller.
        event.serialize_field("agent_id", &self.subagent())?;
        event.serialize_field("tool_input", &self.question())?;
        event.end()
    }
}

impl<'de> Deserialize<'de> for HookEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = HookEventWire::deserialize(deserializer)?;
        let event = Self {
            hook_event_name: wire.hook_event_name,
            tool_name: wire.tool_name,
            is_interrupt: wire.is_interrupt,
            source: wire.source,
            trigger: wire.trigger,
            agent_id: wire.agent_id,
            tool_input: wire.tool_input,
        };
        Ok(Self {
            // Again on the way in. A peer that did not confine is not a peer to trust with
            // what it sent, and the cheap answer is to drop it rather than to refuse a hook.
            tool_input: event.question().cloned(),
            ..event
        })
    }
}

impl HookEvent {
    /// An event with nothing but its name, for a caller filling the rest in.
    #[must_use]
    pub fn new(hook_event_name: HookEventName) -> Self {
        Self {
            hook_event_name,
            tool_name: None,
            is_interrupt: false,
            source: None,
            trigger: None,
            agent_id: None,
            tool_input: None,
        }
    }

    /// Whether this event is a session boundary — `SessionStart` (§2.1).
    #[must_use]
    pub fn is_session_boundary(&self) -> bool {
        self.hook_event_name == HookEventName::SessionStart
    }

    /// The `waiting` payload §2.2 asks for: `tool_input`, but only on an event that maps to
    /// `waiting`.
    ///
    /// **The confinement is the point, and it is about size before it is about secrets.**
    /// A `PreToolUse{tool_name:"Write"}` maps to `working` and its `tool_input` is the whole
    /// file being written. The control line cap is a megabyte, so an unconfined event does
    /// not merely carry a payload nobody wanted — past the cap it is refused as
    /// `ControlError::Truncated` and **the status is lost**, exactly where the payload was
    /// never needed. The row would also have persisted it, which is trap 13, but that is the
    /// second problem rather than the first.
    ///
    /// This is what the wire uses in both directions, so the field cannot be filled wrongly
    /// and sent. `PermissionRequest` maps to `waiting` too and keeps its input; its payload
    /// shape is not verified against Claude's documentation, which is worth knowing before
    /// anything depends on the fields inside.
    #[must_use]
    pub fn question(&self) -> Option<&serde_json::Value> {
        if self.state() == Some(AgentState::Waiting) {
            self.tool_input.as_ref()
        } else {
            None
        }
    }

    /// The subagent this event is about, with a blank id read as absent.
    ///
    /// The only reader of the field, because the id is a **key**: one blank id would collapse
    /// every subagent in a pane into a single row flickering between their states. Normalised
    /// rather than refused, because refusing would fail the hook on the agent's critical path
    /// (§5.2) and lose a status to protect a field nothing else reads.
    #[must_use]
    pub fn subagent(&self) -> Option<&str> {
        self.agent_id.as_deref().filter(|id| !id.trim().is_empty())
    }

    /// The state §2.1 maps this event to, or `None` when it maps to nothing.
    ///
    /// `None` is a specified outcome and not a failure. Four groups reach it:
    ///
    /// - **`PreCompact`**, which §2.1 calls deliberately unmapped.
    /// - **`SubagentStart`, `SubagentStop`, `TeammateIdle`**, which §5.1 lists among the
    ///   twelve installed events and §2.1's table gives no row. They reach the roster the
    ///   way every other event does, through `agent_id` — and an event with no state updates
    ///   no entry, which is what the two documents together say and neither contradicts.
    /// - **A `PostCompact` that nobody triggered by hand.** §2.1 maps
    ///   `PostCompact{trigger:"manual"}`; an automatic compaction is the agent carrying on.
    /// - **Anything this build does not know**, for the reason on [`HookEventName`].
    #[must_use]
    pub fn state(&self) -> Option<AgentState> {
        match self.hook_event_name {
            HookEventName::UserPromptSubmit
            | HookEventName::PostToolUse
            | HookEventName::PostToolUseFailure => Some(AgentState::Working),
            HookEventName::PreToolUse => {
                Some(if self.tool_name.as_deref() == Some(ASK_USER_QUESTION) {
                    AgentState::Waiting
                } else {
                    AgentState::Working
                })
            }
            HookEventName::PermissionRequest => Some(AgentState::Waiting),
            HookEventName::Stop | HookEventName::StopFailure => Some(self.stopped()),
            HookEventName::PostCompact => {
                (self.trigger.as_deref() == Some(MANUAL_COMPACT)).then(|| self.stopped())
            }
            // A boundary, never an interruption: `is_interrupt` on a session start would
            // claim that starting was interrupted, which is not a thing that happens.
            HookEventName::SessionStart => Some(AgentState::Done),
            HookEventName::PreCompact
            | HookEventName::SubagentStart
            | HookEventName::SubagentStop
            | HookEventName::TeammateIdle
            | HookEventName::Other(_) => None,
        }
    }

    /// `done`, or `interrupted` when the event says it was interrupted (§2.1).
    fn stopped(&self) -> AgentState {
        if self.is_interrupt {
            AgentState::Interrupted
        } else {
            AgentState::Done
        }
    }

    /// Which entry this event updates: the lead's row, or one subagent's.
    ///
    /// §2.1's roster rule as a shape rather than a comment. A caller has to name the arm it
    /// is in, so "the lead keeps its own state" cannot be lost by forgetting to check a
    /// field that is `None` most of the time.
    #[must_use]
    pub fn target(&self) -> StatusTarget {
        match self.subagent() {
            Some(agent_id) => StatusTarget::Subagent {
                agent_id: agent_id.to_owned(),
            },
            None => StatusTarget::Lead,
        }
    }

    /// The row this event writes for `pane`, or `None` when [`state`](Self::state) is.
    ///
    /// `observed_at` is the daemon's clock at the moment the event **arrived**, never a time
    /// out of the payload (§2.2). A hook that spooled to disk and drained ten minutes later
    /// is ten minutes old, and a timestamp the hook chose would say otherwise.
    ///
    /// The row is always fresh: `restored_unconfirmed` is what a rehydrated row carries, and
    /// there is no way to reach this function without a live event.
    ///
    /// **`tool_input` reaches the row only when the row is `waiting`**, through
    /// [`HookEvent::question`]. The wire applies the same rule, so an event that arrived over
    /// the socket has already been confined — this check is for the one that did not: a
    /// `HookEvent` built in process never passed through serde at all, and the row is what
    /// the daemon persists, the window renders and the spool writes to disk (trap 13).
    #[must_use]
    pub fn to_row(&self, pane: PaneKey, observed_at: UnixMillis) -> Option<AgentStatusRow> {
        let state = self.state()?;
        Some(AgentStatusRow {
            pane,
            state,
            question: self.question().cloned(),
            // Derived, never copied off the event: §2.2 says this field distinguishes
            // `interrupted` from `done`, so a row whose flag disagrees with its state is a
            // row two readers would disagree about.
            is_interrupt: state == AgentState::Interrupted,
            session_boundary: self.is_session_boundary(),
            agent_id: self.subagent().map(str::to_owned),
            observed_at,
            restored_unconfirmed: false,
        })
    }
}

/// Which entry in a pane's status an event updates.
///
/// Two arms rather than an `Option<String>`, for the reason [`HookEvent::target`] gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "target", rename_all = "snake_case")]
#[ts(export)]
pub enum StatusTarget {
    /// The lead agent's own row — the pane's status.
    Lead,
    /// One subagent's roster entry.
    Subagent {
        /// Which subagent, as Claude named it.
        ///
        /// Spelled `agentId` on the wire, matching [`AgentStatusRow::agent_id`]. A container
        /// `rename_all` renames an enum's *variants* and not its fields, so without this the
        /// one place a client joins a target to a row would be the one place the two spell
        /// the same id differently.
        #[serde(rename = "agentId")]
        agent_id: String,
    },
}

/// A point in time, in Unix milliseconds.
///
/// A newtype because §2.2 names the field `observed_at`, and a bare `u64` called
/// `observed_at` is a number whose unit lives only in prose. The spelling stays the plan's;
/// the unit moves into the type, where it cannot be read past.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnixMillis(pub u64);

impl UnixMillis {
    /// The value as the bare number it is on the wire.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// How long after `self` `later` is, or `None` when `later` is the earlier of the two.
    ///
    /// `None` rather than a saturating zero: a clock that went backwards is not "no time has
    /// passed", and a staleness test that treated it as such would call a row from the
    /// future permanently fresh.
    #[must_use]
    pub const fn elapsed_to(self, later: Self) -> Option<u64> {
        later.0.checked_sub(self.0)
    }
}

impl fmt::Display for UnixMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One pane's status, or one subagent's: the eight fields of §2.2, in its order.
///
/// The same type serves the lead and the roster, because §2.2 makes `agent_id` one of the
/// eight fields rather than a second shape. A row with an `agent_id` is a roster entry; one
/// without is the lead's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusRow {
    /// The row's identity.
    pub pane: PaneKey,
    /// What the agent is doing.
    pub state: AgentState,
    /// The `waiting` payload, verbatim, or absent. See [`HookEvent::question`].
    #[ts(type = "unknown")]
    pub question: Option<serde_json::Value>,
    /// Distinguishes `interrupted` from `done`.
    ///
    /// Redundant with `state` on a row this crate built, and held that way by
    /// `a_rows_interrupt_flag_agrees_with_its_state`. It is on the wire because §2.2 puts it
    /// there, and because a reader that branches on the flag and a reader that branches on
    /// the state have to reach the same answer.
    pub is_interrupt: bool,
    /// True for `SessionStart`. **Suppresses the notification** (§2.1).
    ///
    /// Read it through [`AgentStatusRow::notify`] rather than directly. A `bool` beside
    /// seven other fields is a thing a consumer forgets; an arm it has to name is not.
    pub session_boundary: bool,
    /// The subagent this row belongs to, or absent on the lead's row.
    pub agent_id: Option<String>,
    /// When the **daemon received** the event, not when the hook fired (§2.2).
    pub observed_at: UnixMillis,
    /// True for a row rehydrated from the spool, until a live hook arrives (§2.3).
    ///
    /// Such a row never counts as fresh, which is why [`AgentStatusRow::notify`] suppresses
    /// a notification for it.
    pub restored_unconfirmed: bool,
}

impl AgentStatusRow {
    /// Whether this row may raise a notification.
    ///
    /// The rules §2 fixes, made unforgettable. §2.1 says a `SessionStart` maps to `done`
    /// **and must never notify**; §2.3 says a rehydrated row never counts as fresh, and a
    /// notification is the freshest possible use of a row. Everything else is
    /// [`Notify::Permitted`], and what the window does with a permitted row is §5's
    /// business, not this crate's.
    #[must_use]
    pub fn notify(&self) -> Notify {
        if self.session_boundary {
            return Notify::Suppressed {
                reason: NotifySuppressed::SessionBoundary,
            };
        }
        if self.restored_unconfirmed {
            return Notify::Suppressed {
                reason: NotifySuppressed::RestoredUnconfirmed,
            };
        }
        Notify::Permitted
    }

    /// Whether this row is older than §2.3's thirty minutes, as of `now`.
    ///
    /// **Staleness is not a fifth state.** §2.1 fixes [`AgentState`] at four and §2.3 says a
    /// stale `working` dot decays to "active" — which only disagree if "active" is read as a
    /// state. It is not: it is what the window paints for a row that is still `working` and
    /// is no longer fresh, which is `state == Working && is_stale(now)`. Keeping the decay
    /// derived is what stops a fifth variant appearing in a type §2.1 says has four.
    ///
    /// A row from the future is not stale: see [`UnixMillis::elapsed_to`].
    #[must_use]
    pub fn is_stale(&self, now: UnixMillis) -> bool {
        self.observed_at
            .elapsed_to(now)
            .is_some_and(|elapsed| elapsed > AGENT_STATUS_STALE_AFTER_MS)
    }
}

/// Whether a status row may raise a notification.
///
/// An enum and not a `bool`, because §2.1's rule is one a consumer forgets and the cost of
/// forgetting it is a toast on every session start — on resume, on clear, and on every
/// window that reconnects. A caller has to name the arm it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "decision", rename_all = "snake_case")]
#[ts(export)]
pub enum Notify {
    /// Nothing in the status contract forbids a notification for this row. What to do with
    /// one is the window's decision (W5), not this crate's.
    Permitted,
    /// The contract forbids one, and says which rule.
    Suppressed {
        /// Which rule suppressed it.
        reason: NotifySuppressed,
    },
}

/// Why a row may not raise a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NotifySuppressed {
    /// `SessionStart`: §2.1 says a session boundary must never notify.
    SessionBoundary,
    /// A row rehydrated from the spool that no live hook has confirmed (§2.3).
    RestoredUnconfirmed,
}

/// One pane's whole status: the lead's row and the subagent roster.
///
/// The roster is a list of rows rather than a map, because JSON object keys carry no order
/// worth relying on and a client renders the roster in the order the daemon sends it. Every
/// entry carries an `agent_id`; the lead's row does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatus {
    /// The agent in the pane.
    pub lead: AgentStatusRow,
    /// Its subagents, each keyed by its own `agent_id`.
    pub subagents: Vec<AgentStatusRow>,
}

impl AgentStatus {
    /// A pane whose lead has a status and whose roster is empty.
    #[must_use]
    pub fn new(lead: AgentStatusRow) -> Self {
        Self {
            lead,
            subagents: Vec::new(),
        }
    }

    /// The pane this status is for.
    #[must_use]
    pub fn pane(&self) -> &PaneKey {
        &self.lead.pane
    }
}

/// Hand a hook event to the daemon.
///
/// The ingest verb, and the other half of [`HookEvent`]: the envelope says what `nysia hook`
/// reads, this says how it hands it over. `nysia hook` prints `{}` on stdout before it sends
/// one (§5.2), so nothing waits on the answer — but there is an answer, because a mutation
/// that reported nothing could not be retried safely and retrying is the spool's whole job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentHook {
    /// Which pane the event is about, as far as the caller knows.
    ///
    /// A **hint** the daemon is free to overrule, and never the proof. §3.2: pane identity
    /// comes from socket peer credentials and the PTY process tree, and `NYSIA_PANE_KEY` is
    /// a hint for speed. A caller that could name any pane here could write status into
    /// somebody else's.
    pub pane_hint: Option<PaneKey>,
    /// What Claude wrote on stdin.
    pub event: HookEvent,
}

/// Read one pane's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusGet {
    /// Which pane.
    pub pane: PaneKey,
}

/// Read every pane's status.
///
/// Deliberately empty, like [`crate::SessionList`]: the daemon holds tens of panes, and a
/// filter that only ever ran against a small list would be a wire shape to support forever
/// for no measured benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusList {}

/// Start receiving status changes on this client's stream connection.
///
/// **The mechanism terminal output already uses**, not a second one. The request goes on the
/// control connection, the daemon picks the [`StreamId`], and changes arrive as frames
/// carrying it on the client's one stream connection — exactly
/// [`StreamAttach`](crate::StreamAttach)'s shape, out of the same per-connection counter, so
/// [`StreamId::classify_unattached`] governs a status id and a session id identically and a
/// router needs no second table of rules.
///
/// Its frames are [`FrameKind::AgentStatus`](crate::FrameKind::AgentStatus), each carrying an
/// [`AgentStatusChange`] — the pane's whole status rather than a diff, which entry moved, and
/// whether the change may notify — and they spend no credit. See that variant.
///
/// No filter, for [`AgentStatusList`]'s reason: the sidebar wants every pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusSubscribe {}

/// The id the daemon assigned to a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusSubscribed {
    /// The id its frames will carry, on this client's stream connection only.
    pub stream_id: StreamId,
}

/// One status change, as a subscriber reads it off an
/// [`AgentStatus`](crate::FrameKind::AgentStatus) frame.
///
/// **This is why [`Notify`] exists on the wire rather than only in Rust.** The row carries
/// `session_boundary` because §2.2 fixes its eight fields, and a `bool` on a row is exactly
/// the thing a consumer forgets — and the consumer that would forget it is the window, which
/// is where notifications are raised and which reads this crate through TypeScript. So the
/// decision travels with the change, already made, and a client that renders a toast reads an
/// arm rather than remembering a rule. D-13 in its plainest form: Rust decides, TypeScript
/// displays.
///
/// Only a *change* carries it. [`AgentStatusGet`] and [`AgentStatusList`] answer with a bare
/// [`AgentStatus`], because reading a status is not an event and nothing notifies on a read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusChange {
    /// The pane's whole status after the change, never a diff.
    pub status: AgentStatus,
    /// Which entry moved: the lead's row, or one subagent's.
    pub changed: StatusTarget,
    /// Whether this change may raise a notification, and why not when it may not.
    ///
    /// [`AgentStatusRow::notify`] is what produces it, so the rule lives in one place and
    /// every subscriber gets the same answer.
    pub notify: Notify,
}

/// Stop receiving status changes, retiring the id for the life of the connection.
///
/// Its own verb rather than [`StreamDetach`](crate::StreamDetach), which releases a
/// *session* stream: the two share one id space and one retirement rule, and nothing else. A
/// client that released a subscription through the session verb would be relying on the
/// daemon conflating two tables, and the day it stopped, the failure would be a subscription
/// that quietly kept streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentStatusUnsubscribe {
    /// Which id to release.
    pub stream_id: StreamId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> PaneKey {
        PaneKey::new("tab_1", "leaf_1").unwrap()
    }

    fn at() -> UnixMillis {
        UnixMillis(1_757_721_600_000)
    }

    fn event(name: HookEventName) -> HookEvent {
        HookEvent::new(name)
    }

    // ---------------------------------------------------------------- the four states

    #[test]
    fn there_are_exactly_four_states_and_the_wire_spells_them_in_snake_case() {
        // §2.1 says "exactly `working`, `waiting`, `done`, `interrupted`. No others." A
        // stated total is a fact about the list beneath it, so this holds the two together
        // rather than trusting the doc comment.
        assert_eq!(AgentState::ALL.len(), 4);
        let spellings: Vec<&str> = AgentState::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(spellings, ["working", "waiting", "done", "interrupted"]);
        for state in AgentState::ALL {
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{state}\"")
            );
            assert_eq!(state.to_string().parse::<AgentState>().unwrap(), state);
        }
        assert!("Working".parse::<AgentState>().is_err());
        assert!("active".parse::<AgentState>().is_err());
        assert!(serde_json::from_str::<AgentState>("\"active\"").is_err());
    }

    // ------------------------------------------------------------ §2.1's mapping table

    #[test]
    fn the_working_events_map_to_working() {
        for name in [
            HookEventName::UserPromptSubmit,
            HookEventName::PostToolUse,
            HookEventName::PostToolUseFailure,
        ] {
            assert_eq!(
                event(name.clone()).state(),
                Some(AgentState::Working),
                "{name} should be working"
            );
        }
        // A `PreToolUse` for anything but the question tool.
        let mut read = event(HookEventName::PreToolUse);
        read.tool_name = Some("Read".to_owned());
        assert_eq!(read.state(), Some(AgentState::Working));
        // And one with no tool name at all: §2.1's row is "not AskUserQuestion".
        assert_eq!(
            event(HookEventName::PreToolUse).state(),
            Some(AgentState::Working)
        );
    }

    #[test]
    fn the_waiting_events_map_to_waiting_and_keep_the_question() {
        assert_eq!(
            event(HookEventName::PermissionRequest).state(),
            Some(AgentState::Waiting)
        );

        let mut ask = event(HookEventName::PreToolUse);
        ask.tool_name = Some(ASK_USER_QUESTION.to_owned());
        ask.tool_input = Some(serde_json::json!({
            "question": "Which migration?",
            "options": ["up", "down"],
        }));
        assert_eq!(ask.state(), Some(AgentState::Waiting));

        // Verbatim: the row carries what Claude wrote, not a re-shaped copy of it.
        let row = ask.to_row(pane(), at()).unwrap();
        assert_eq!(row.question, ask.tool_input);
    }

    #[test]
    fn a_tool_input_that_is_not_a_question_never_reaches_the_wire() {
        // The correction this test exists for. Confining `tool_input` in `to_row` alone
        // confines the *row* and leaves the *frame* fat: the control line cap is a megabyte,
        // so a `Write` with a large input is refused as `ControlError::Truncated` and the
        // status is lost exactly where the payload was never wanted. So the wire drops it,
        // in both directions, and a caller cannot fill the field wrongly and send it.
        let mut writing = event(HookEventName::PreToolUse);
        writing.tool_name = Some("Write".to_owned());
        writing.tool_input = Some(serde_json::json!({ "content": "x".repeat(4096) }));

        // Out: the value is in the Rust struct and not in the JSON.
        assert!(writing.tool_input.is_some());
        assert_eq!(writing.question(), None);
        let json = serde_json::to_value(&writing).unwrap();
        assert_eq!(json["tool_input"], serde_json::Value::Null);
        assert!(serde_json::to_string(&writing).unwrap().len() < 200);

        // In: a peer that did not confine gets confined here rather than refused, because
        // refusing would lose the status this whole path exists to deliver.
        let unconfined = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "tool_input": { "content": "x".repeat(4096) },
        });
        let parsed: HookEvent = serde_json::from_value(unconfined).unwrap();
        assert_eq!(parsed.tool_input, None);
        assert_eq!(parsed.state(), Some(AgentState::Working));

        // And the question still crosses, so this is not "always absent" wearing a disguise.
        let mut asking = writing;
        asking.tool_name = Some(ASK_USER_QUESTION.to_owned());
        assert!(asking.question().is_some());
        let json = serde_json::to_value(&asking).unwrap();
        assert_eq!(json["tool_input"], *asking.tool_input.as_ref().unwrap());
        assert_eq!(serde_json::from_value::<HookEvent>(json).unwrap(), asking);
    }

    #[test]
    fn a_hook_event_still_defaults_every_field_an_event_does_not_carry() {
        // Hand-written serde replaced the derive, and `#[serde(default)]` moved to the
        // private wire struct with it. This is what holds that move honest: a payload
        // carrying nothing but an event name still reads, which is the property every event
        // with a different field set depends on.
        let parsed: HookEvent = serde_json::from_str(r#"{"hook_event_name":"Stop"}"#).unwrap();
        assert_eq!(parsed, HookEvent::new(HookEventName::Stop));

        // And the serialised form still names all seven fields, so a reader sees the same
        // shape whichever event produced it.
        let json = serde_json::to_value(&parsed).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "agent_id",
                "hook_event_name",
                "is_interrupt",
                "source",
                "tool_input",
                "tool_name",
                "trigger",
            ]
        );
    }

    #[test]
    fn a_tool_input_reaches_the_row_only_when_the_row_is_waiting() {
        // Trap 13 arriving through a field nobody would look at. A `Write`'s `tool_input`
        // is the whole file being written, and that event maps to `working` — so an
        // unconditional copy would put file contents into a row the daemon persists, the
        // window renders and the spool writes to disk.
        const SECRET: &str = "ANTHROPIC_API_KEY=sk-do-not-persist-me";

        let mut writing = event(HookEventName::PreToolUse);
        writing.tool_name = Some("Write".to_owned());
        writing.tool_input = Some(serde_json::json!({
            "file_path": ".env",
            "content": SECRET,
        }));
        let row = writing.to_row(pane(), at()).unwrap();
        assert_eq!(row.state, AgentState::Working);
        assert_eq!(row.question, None, "a working row must carry no tool input");
        assert!(!serde_json::to_string(&row).unwrap().contains(SECRET));

        // And the waiting case keeps it, so the confinement is not "always absent".
        let mut asking = writing;
        asking.tool_name = Some(ASK_USER_QUESTION.to_owned());
        let row = asking.to_row(pane(), at()).unwrap();
        assert_eq!(row.state, AgentState::Waiting);
        assert_eq!(row.question, asking.tool_input);
    }

    #[test]
    fn the_done_events_map_to_done_and_interrupt_turns_them_into_interrupted() {
        for name in [HookEventName::Stop, HookEventName::StopFailure] {
            assert_eq!(event(name.clone()).state(), Some(AgentState::Done));
            let mut interrupted = event(name.clone());
            interrupted.is_interrupt = true;
            assert_eq!(
                interrupted.state(),
                Some(AgentState::Interrupted),
                "{name} with is_interrupt should be interrupted"
            );
        }
    }

    #[test]
    fn only_a_hand_triggered_compaction_is_done() {
        // §2.1 maps `PostCompact{trigger:"manual"}`, and the qualification is the whole
        // point: an automatic compaction is the agent carrying on, not finishing.
        let mut manual = event(HookEventName::PostCompact);
        manual.trigger = Some(MANUAL_COMPACT.to_owned());
        assert_eq!(manual.state(), Some(AgentState::Done));

        let mut interrupted = manual.clone();
        interrupted.is_interrupt = true;
        assert_eq!(interrupted.state(), Some(AgentState::Interrupted));

        let mut automatic = event(HookEventName::PostCompact);
        automatic.trigger = Some("auto".to_owned());
        assert_eq!(automatic.state(), None);
        assert_eq!(event(HookEventName::PostCompact).state(), None);
    }

    #[test]
    fn pre_compact_is_deliberately_unmapped() {
        // Named for the rule it holds. §2.1: "`PreCompact` is deliberately unmapped —
        // dropping it is the specified behaviour, not an oversight, and it needs a test
        // saying so." This is that test. If it ever goes red because somebody mapped
        // `PreCompact`, the fix is to un-map it, not to change this assertion.
        let mut compacting = event(HookEventName::PreCompact);
        assert_eq!(compacting.state(), None);
        assert_eq!(compacting.to_row(pane(), at()), None);

        // Not even with the fields that make other events map: an unmapped event is
        // unmapped whatever it carries.
        compacting.is_interrupt = true;
        compacting.trigger = Some(MANUAL_COMPACT.to_owned());
        compacting.tool_name = Some(ASK_USER_QUESTION.to_owned());
        assert_eq!(compacting.state(), None);
        assert_eq!(compacting.to_row(pane(), at()), None);

        // And the name still survives a round trip, so a dropped event is still legible in
        // a log line rather than becoming `Other("PreCompact")`.
        assert_eq!(compacting.hook_event_name.as_str(), "PreCompact");
    }

    #[test]
    fn the_events_with_no_row_in_the_table_map_to_nothing() {
        // §5.1 installs twelve events; §2.1's table gives three of them no state. They
        // reach the roster through `agent_id` like everything else, and an event with no
        // state updates no entry. Neither document says otherwise, so neither does this.
        for name in [
            HookEventName::SubagentStart,
            HookEventName::SubagentStop,
            HookEventName::TeammateIdle,
        ] {
            let mut with_agent = event(name.clone());
            with_agent.agent_id = Some("agent_7".to_owned());
            assert_eq!(with_agent.state(), None, "{name} has no row in §2.1");
            assert_eq!(with_agent.to_row(pane(), at()), None);
            // It still names the subagent it is about, so a later wave that does map them
            // does not have to re-derive the routing.
            assert_eq!(
                with_agent.target(),
                StatusTarget::Subagent {
                    agent_id: "agent_7".to_owned()
                }
            );
        }
    }

    #[test]
    fn a_session_start_is_done_and_a_boundary_and_never_notifies() {
        for source in ["startup", "resume", "clear"] {
            let mut start = event(HookEventName::SessionStart);
            start.source = Some(source.to_owned());
            assert_eq!(start.state(), Some(AgentState::Done));

            let row = start.to_row(pane(), at()).unwrap();
            assert!(row.session_boundary, "{source} should be a boundary");
            // The rule §2.1 puts in bold. A `bool` would let a consumer forget; the arm
            // makes it name the reason.
            assert_eq!(
                row.notify(),
                Notify::Suppressed {
                    reason: NotifySuppressed::SessionBoundary
                },
                "a {source} boundary must never notify"
            );
        }

        // A session start is a boundary, never an interruption.
        let mut interrupted = event(HookEventName::SessionStart);
        interrupted.is_interrupt = true;
        let row = interrupted.to_row(pane(), at()).unwrap();
        assert_eq!(row.state, AgentState::Done);
        assert!(!row.is_interrupt);
    }

    #[test]
    fn nothing_but_a_session_start_is_a_boundary() {
        // The other half of the rule above: if every row were a boundary, nothing would
        // ever notify and the suppression would look like it worked.
        let mut stop = event(HookEventName::Stop);
        stop.source = Some("startup".to_owned());
        let row = stop.to_row(pane(), at()).unwrap();
        assert!(!row.session_boundary);
        assert_eq!(row.notify(), Notify::Permitted);
    }

    // ------------------------------------------------------------------ the §2.2 row

    #[test]
    fn a_row_carries_exactly_the_eight_fields_section_two_two_names() {
        // A stated total is a fact about the list beneath it. §2.2's table has eight rows,
        // so this holds the wire to eight keys and names each one — a ninth field added
        // without amending the plan fails here rather than in a reviewer's memory.
        let row = event(HookEventName::Stop).to_row(pane(), at()).unwrap();
        let json = serde_json::to_value(&row).unwrap();
        let object = json.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "agentId",
                "isInterrupt",
                "observedAt",
                "pane",
                "question",
                "restoredUnconfirmed",
                "sessionBoundary",
                "state",
            ]
        );
    }

    #[test]
    fn a_rows_interrupt_flag_agrees_with_its_state() {
        for name in [HookEventName::Stop, HookEventName::StopFailure] {
            for is_interrupt in [false, true] {
                let mut stopped = event(name.clone());
                stopped.is_interrupt = is_interrupt;
                let row = stopped.to_row(pane(), at()).unwrap();
                assert_eq!(
                    row.is_interrupt,
                    row.state == AgentState::Interrupted,
                    "{name} with is_interrupt={is_interrupt} disagrees with itself"
                );
            }
        }
    }

    #[test]
    fn a_row_round_trips_and_observed_at_is_a_bare_number() {
        let mut ask = event(HookEventName::PreToolUse);
        ask.tool_name = Some(ASK_USER_QUESTION.to_owned());
        ask.tool_input = Some(serde_json::json!({ "question": "which?" }));
        ask.agent_id = Some("agent_7".to_owned());
        let row = ask.to_row(pane(), at()).unwrap();

        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["state"], "waiting");
        assert_eq!(json["observedAt"], 1_757_721_600_000_u64);
        assert_eq!(json["agentId"], "agent_7");
        assert_eq!(json["restoredUnconfirmed"], false);
        assert_eq!(serde_json::from_value::<AgentStatusRow>(json).unwrap(), row);
    }

    #[test]
    fn a_restored_row_never_notifies_until_a_live_hook_confirms_it() {
        // §2.3: rows rehydrated from the spool "never count as fresh", and a notification
        // is the freshest possible use of a row. A daemon restart must not replay every
        // toast the person already saw.
        let live = event(HookEventName::Stop).to_row(pane(), at()).unwrap();
        assert_eq!(live.notify(), Notify::Permitted);

        let restored = AgentStatusRow {
            restored_unconfirmed: true,
            ..live.clone()
        };
        assert_eq!(
            restored.notify(),
            Notify::Suppressed {
                reason: NotifySuppressed::RestoredUnconfirmed
            }
        );
    }

    #[test]
    fn staleness_is_derived_from_the_clock_rather_than_being_a_fifth_state() {
        let row = event(HookEventName::UserPromptSubmit)
            .to_row(pane(), at())
            .unwrap();
        assert_eq!(row.state, AgentState::Working);

        assert!(!row.is_stale(at()));
        assert!(!row.is_stale(UnixMillis(at().get() + AGENT_STATUS_STALE_AFTER_MS)));
        assert!(row.is_stale(UnixMillis(at().get() + AGENT_STATUS_STALE_AFTER_MS + 1)));

        // A clock that went backwards is not a row that never ages: it is a row we cannot
        // date, and calling it fresh is the safer of the two wrong answers because the
        // alternative decays every dot the moment a clock is corrected.
        assert!(!row.is_stale(UnixMillis(at().get() - 1)));
        assert_eq!(at().elapsed_to(UnixMillis(at().get() - 1)), None);

        // Thirty minutes, from §2.3, in milliseconds.
        assert_eq!(AGENT_STATUS_STALE_AFTER_MS, 1_800_000);
        assert_eq!(AGENT_STATUS_HISTORY_CAP, 20);
    }

    // ------------------------------------------------------------------- the roster

    #[test]
    fn an_event_with_an_agent_id_targets_the_roster_and_one_without_targets_the_lead() {
        let lead = event(HookEventName::PostToolUse);
        assert_eq!(lead.target(), StatusTarget::Lead);
        assert_eq!(lead.to_row(pane(), at()).unwrap().agent_id, None);

        let mut subagent = event(HookEventName::PostToolUse);
        subagent.agent_id = Some("agent_7".to_owned());
        assert_eq!(
            subagent.target(),
            StatusTarget::Subagent {
                agent_id: "agent_7".to_owned()
            }
        );
        assert_eq!(
            subagent.to_row(pane(), at()).unwrap().agent_id.as_deref(),
            Some("agent_7")
        );
    }

    #[test]
    fn a_blank_agent_id_is_absent_rather_than_a_roster_key() {
        // One blank key would collapse every subagent in a pane into a single row that
        // flickered between their states, and the pane's lead would keep its own row while
        // an empty-string entry shadowed the roster.
        for blank in ["\"\"", "\"   \"", "null"] {
            let json = format!("{{\"hook_event_name\":\"PostToolUse\",\"agent_id\":{blank}}}");
            let parsed: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.agent_id, None, "{blank} should read as absent");
            assert_eq!(parsed.subagent(), None);
            assert_eq!(parsed.target(), StatusTarget::Lead);
        }

        // And one built in process rather than read off the wire, which is the case a
        // deserialise-only guard would have missed: every reader goes through `subagent`,
        // so a blank id is absent there too and cannot become a roster key or a row.
        let mut blank = event(HookEventName::PostToolUse);
        blank.agent_id = Some("   ".to_owned());
        assert_eq!(blank.subagent(), None);
        assert_eq!(blank.target(), StatusTarget::Lead);
        assert_eq!(blank.to_row(pane(), at()).unwrap().agent_id, None);
        assert_eq!(
            serde_json::to_value(&blank).unwrap()["agent_id"],
            serde_json::Value::Null
        );
    }

    // ----------------------------------------------------------- the stdin envelope

    #[test]
    fn a_hook_event_reads_the_json_claude_actually_writes() {
        // Claude's own spelling, and more fields than this type names. The extra ones are
        // ignored rather than refused: a hook that failed because a field appeared would
        // stop reporting status the day the agent updated.
        let payload = serde_json::json!({
            "session_id": "abc123",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/src/nysia",
            "permission_mode": "ask",
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_input": { "question": "Which migration?" },
        });
        let parsed: HookEvent = serde_json::from_value(payload).unwrap();
        assert_eq!(parsed.hook_event_name, HookEventName::PreToolUse);
        assert_eq!(parsed.tool_name.as_deref(), Some(ASK_USER_QUESTION));
        assert_eq!(parsed.state(), Some(AgentState::Waiting));
        // The question arrives under Claude's key. A field named `question` would have read
        // a key Claude never writes, and every waiting row would have carried nothing.
        assert_eq!(
            parsed.tool_input,
            Some(serde_json::json!({ "question": "Which migration?" }))
        );
        assert_eq!(
            parsed.to_row(pane(), at()).unwrap().question,
            parsed.tool_input
        );
        // The dropped fields are dropped: nothing this type carries came from them.
        let back = serde_json::to_value(&parsed).unwrap();
        assert!(back.get("transcript_path").is_none());
        assert!(back.get("session_id").is_none());
        assert!(back.get("cwd").is_none());
    }

    #[test]
    fn a_payload_with_nothing_but_an_event_name_still_reads() {
        // Every event has a different set of fields, so demanding any of them would refuse
        // the payloads of the events that do not have them.
        let parsed: HookEvent = serde_json::from_str(r#"{"hook_event_name":"Stop"}"#).unwrap();
        assert_eq!(parsed, HookEvent::new(HookEventName::Stop));
        assert!(!parsed.is_interrupt);
        assert_eq!(parsed.state(), Some(AgentState::Done));

        // And a name nobody has seen is absorbed rather than refused.
        let unknown: HookEvent =
            serde_json::from_str(r#"{"hook_event_name":"SomethingNew"}"#).unwrap();
        assert_eq!(
            unknown.hook_event_name,
            HookEventName::Other("SomethingNew".to_owned())
        );
        assert_eq!(unknown.state(), None);
        assert_eq!(unknown.to_row(pane(), at()), None);
    }

    #[test]
    fn the_hook_envelope_keeps_claudes_snake_case_and_the_row_keeps_nysias_camel_case() {
        // The one place two vocabularies meet, and the reason the module docs lead with it.
        let mut stop = event(HookEventName::Stop);
        stop.is_interrupt = true;
        let event_json = serde_json::to_value(&stop).unwrap();
        assert!(event_json.get("hook_event_name").is_some());
        assert!(event_json.get("is_interrupt").is_some());
        assert!(event_json.get("hookEventName").is_none());

        let row_json = serde_json::to_value(stop.to_row(pane(), at()).unwrap()).unwrap();
        assert!(row_json.get("isInterrupt").is_some());
        assert!(row_json.get("is_interrupt").is_none());
    }

    #[test]
    fn the_exported_event_union_lists_every_known_name() {
        // The union is a hand-written string in a `#[ts(type = …)]` attribute and the
        // compiler ties it to nothing, so without this a new event name ships a union that
        // has never heard of it. `known()` comes from the same macro list as the variants,
        // which is what makes this test worth running. Same shape as
        // `the_exported_typescript_union_lists_every_known_code` in `error.rs`.
        let exported = <HookEventName as ts_rs::TS>::inline(&ts_rs::Config::default());

        for name in HookEventName::known() {
            assert!(
                exported.contains(&format!("\"{}\"", name.as_str())),
                "{name} is missing from the exported union:
  {exported}"
            );
        }

        // Every known name being present is only half of it: a renamed or deleted variant
        // leaves a literal behind that no `as_str` produces. The union's only quoted
        // strings are the names — the open tail carries none — so counting them is exact,
        // and this is what makes the list and its total one fact rather than two.
        let quotes = exported.matches('"').count();
        assert_eq!(
            quotes % 2,
            0,
            "unbalanced quotes in:
  {exported}"
        );
        assert_eq!(
            quotes / 2,
            HookEventName::known().len(),
            "the exported union has {} literal(s) but this build knows {} name(s):
  {exported}",
            quotes / 2,
            HookEventName::known().len()
        );

        // The tail that keeps the union open, for the reason `ErrorCode` has one: a hook
        // name this build has not met has to stay readable on the TypeScript side too.
        assert!(exported.contains("(string & {})"), "{exported}");
    }

    // ------------------------------------------------------------------- the verbs

    #[test]
    fn the_status_verbs_round_trip() {
        let get = AgentStatusGet { pane: pane() };
        let json = serde_json::to_value(&get).unwrap();
        assert_eq!(json["pane"], "tab_1:leaf_1");
        assert_eq!(serde_json::from_value::<AgentStatusGet>(json).unwrap(), get);

        assert_eq!(
            serde_json::to_value(AgentStatusList {}).unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::to_value(AgentStatusSubscribe {}).unwrap(),
            serde_json::json!({})
        );

        let subscribed = AgentStatusSubscribed {
            stream_id: StreamId(3),
        };
        let json = serde_json::to_value(subscribed).unwrap();
        assert_eq!(json["streamId"], 3);
        assert_eq!(
            serde_json::from_value::<AgentStatusSubscribed>(json).unwrap(),
            subscribed
        );

        let unsubscribe = AgentStatusUnsubscribe {
            stream_id: StreamId(3),
        };
        let json = serde_json::to_value(unsubscribe).unwrap();
        assert_eq!(json["streamId"], 3);
        assert_eq!(
            serde_json::from_value::<AgentStatusUnsubscribe>(json).unwrap(),
            unsubscribe
        );
    }

    #[test]
    fn the_ingest_verb_carries_the_pane_as_a_hint_that_may_be_absent() {
        let hook = AgentHook {
            pane_hint: None,
            event: event(HookEventName::Stop),
        };
        let json = serde_json::to_value(&hook).unwrap();
        assert_eq!(json["paneHint"], serde_json::Value::Null);
        assert_eq!(json["event"]["hook_event_name"], "Stop");
        assert_eq!(serde_json::from_value::<AgentHook>(json).unwrap(), hook);

        let hinted = AgentHook {
            pane_hint: Some(pane()),
            ..hook
        };
        let json = serde_json::to_value(&hinted).unwrap();
        assert_eq!(json["paneHint"], "tab_1:leaf_1");
        assert_eq!(serde_json::from_value::<AgentHook>(json).unwrap(), hinted);
    }

    #[test]
    fn a_status_carries_the_lead_and_its_roster() {
        let lead = event(HookEventName::UserPromptSubmit)
            .to_row(pane(), at())
            .unwrap();
        let status = AgentStatus::new(lead.clone());
        assert_eq!(status.pane(), &pane());
        assert!(status.subagents.is_empty());

        let mut subagent_event = event(HookEventName::PostToolUse);
        subagent_event.agent_id = Some("agent_7".to_owned());
        let with_roster = AgentStatus {
            lead,
            subagents: vec![subagent_event.to_row(pane(), at()).unwrap()],
        };
        let json = serde_json::to_value(&with_roster).unwrap();
        assert_eq!(json["lead"]["agentId"], serde_json::Value::Null);
        assert_eq!(json["subagents"][0]["agentId"], "agent_7");
        assert_eq!(
            serde_json::from_value::<AgentStatus>(json).unwrap(),
            with_roster
        );
    }

    #[test]
    fn a_target_spells_its_subagent_id_the_way_a_row_does() {
        // A container `rename_all` renames variants, not fields, so this spelling is one
        // attribute away from being the only snake_case key on a camelCase wire — in the
        // one place a client joins a target to the row it names.
        let target = StatusTarget::Subagent {
            agent_id: "agent_7".to_owned(),
        };
        let json = serde_json::to_value(&target).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "target": "subagent", "agentId": "agent_7" })
        );
        assert_eq!(
            serde_json::from_value::<StatusTarget>(json).unwrap(),
            target
        );

        let row = serde_json::to_value(
            event(HookEventName::PostToolUse)
                .to_row(pane(), at())
                .unwrap(),
        )
        .unwrap();
        assert!(row.as_object().unwrap().contains_key("agentId"));

        assert_eq!(
            serde_json::to_value(StatusTarget::Lead).unwrap(),
            serde_json::json!({ "target": "lead" })
        );
    }

    #[test]
    fn the_notify_decision_names_itself_on_the_wire() {
        assert_eq!(
            serde_json::to_value(Notify::Permitted).unwrap(),
            serde_json::json!({ "decision": "permitted" })
        );
        assert_eq!(
            serde_json::to_value(Notify::Suppressed {
                reason: NotifySuppressed::SessionBoundary
            })
            .unwrap(),
            serde_json::json!({ "decision": "suppressed", "reason": "session_boundary" })
        );
    }

    #[test]
    fn a_pushed_change_carries_the_decision_rather_than_leaving_it_to_the_window() {
        // The window is where a notification is raised and it reads this crate through
        // TypeScript, which has no `AgentStatusRow::notify` to call — so a `bool` on the row
        // would be the forgettable flag all over again, one language further away. The
        // decision travels with the change, already made.
        let start = event(HookEventName::SessionStart);
        let row = start.to_row(pane(), at()).unwrap();
        let change = AgentStatusChange {
            status: AgentStatus::new(row.clone()),
            changed: start.target(),
            notify: row.notify(),
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["changed"], serde_json::json!({ "target": "lead" }));
        assert_eq!(
            json["notify"],
            serde_json::json!({ "decision": "suppressed", "reason": "session_boundary" })
        );
        assert_eq!(
            serde_json::from_value::<AgentStatusChange>(json).unwrap(),
            change
        );

        // And an ordinary change says so, so the suppression above is not "nothing ever
        // notifies" wearing a disguise.
        let stop = event(HookEventName::Stop);
        let row = stop.to_row(pane(), at()).unwrap();
        let change = AgentStatusChange {
            status: AgentStatus::new(row.clone()),
            changed: stop.target(),
            notify: row.notify(),
        };
        assert_eq!(
            serde_json::to_value(&change).unwrap()["notify"],
            serde_json::json!({ "decision": "permitted" })
        );
    }
}
