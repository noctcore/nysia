//! The daemon's half of agent status: ingest, persistence, read-back and the subscription.
//!
//! `docs/plans/v0.2-delivery-plan.md` §2 is the contract and is **cited rather than
//! re-derived**. `nysia-proto` owns the types and §2.1's mapping — [`HookEvent::state`] and
//! [`HookEvent::to_row`] are that table, so nothing here restates it — and
//! [`crate::store`] owns the table they land in. This module is the part in between: which
//! pane an event belongs to, what to do when the store refuses it, and who hears about it.
//!
//! # The pane is proved, never accepted
//!
//! §3.2. An [`AgentHook`](nysia_proto::AgentHook)'s `pane_hint` is a hint and the ancestry
//! walk is the proof, so the pane is resolved from the session the caller descends from and
//! the hint is only ever compared against it. A daemon that took the hint when it had no
//! ancestry would let any process on the machine write status into any pane — which is the
//! exact sentence `crates/nysia/tests/agent_status.rs` gives as its reason for running the
//! hook *inside* the session rather than spawning it from the test harness.
//!
//! So a hook that descends from nothing this daemon spawned is refused. That is not a
//! degraded mode: the window and a person at a shell are nobody's descendant too, and they
//! have no business reporting an agent's status either.
//!
//! **The spool is the one place a hint is honoured**, and it is honoured by a different rule.
//! A row drained from disk carries a pane nothing can prove after the fact, because the
//! process that wrote it is long gone — so it lands as `restored_unconfirmed`, which §2.3
//! says never counts as fresh and which suppresses its notification. A claim nobody can check
//! arriving in the bucket for claims nobody has confirmed is the same answer §2.3 already
//! gives, not a second one.
//!
//! # Staleness is nothing this module does
//!
//! §2.3's thirty minutes decays a `working` dot to "active", and
//! [`AgentStatusRow::is_stale`] derives it from `observed_at` at read time. There is no
//! column, no fifth [`AgentState`](nysia_proto::AgentState) and no server-side rewrite: a
//! stored staleness would be wrong a millisecond after it was written, and a daemon that
//! recomputed `state` on the way out would be a second opinion on a field the row already
//! carries. The daemon's whole obligation is therefore negative — stamp `observed_at` with
//! the moment the event **arrived** (§2.2) and then leave the row alone — and
//! `a_row_that_has_gone_stale_is_still_working_when_it_is_read_back` is that obligation.
//!
//! # Trap 13, and what it means for everything added here
//!
//! A `question` is a tool's input verbatim and can be a secret. `nysia-proto` confines
//! `tool_input` to `waiting` events in both directions and the store applies the same rule at
//! the disk boundary. Nothing here widens it, and **no error variant or trace in this module
//! carries a payload** — a row is named by its pane and its state, never by what it holds.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use nysia_proto::{
    AgentStatus, AgentStatusChange, AgentStatusRow, Frame, FrameKind, HookEvent, PaneKey,
    StatusTarget, StreamId, UnixMillis,
};

use crate::rpc::stream::{ConnectionKey, StreamSink};
use crate::store::{Restored, Store, StoreError};

/// One pane's status service: the store it persists through and the clients watching it.
#[derive(Debug)]
pub struct AgentStatusService {
    store: Arc<Store>,
    subscribers: Mutex<Vec<Subscription>>,
}

/// One client watching status changes.
#[derive(Debug)]
struct Subscription {
    /// The stream connection the frames go to. Named so a client that reconnected does not
    /// have its new connection torn down by its old one's unsubscribe.
    connection: ConnectionKey,
    /// The id its frames carry, and the sink they go through.
    sink: Arc<StreamSink>,
}

impl AgentStatusService {
    /// Open the store at `path` and serve status out of it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the database cannot be opened or migrated.
    pub fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self::with_store(Arc::new(Store::open(path)?)))
    }

    /// Serve status out of an already-open store.
    #[must_use]
    pub fn with_store(store: Arc<Store>) -> Self {
        Self {
            store,
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// The store behind this service.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Hand every row a drain produced to the store, counting what became of them.
    ///
    /// Each row lands `restored_unconfirmed` whatever it says — the store forces that, and
    /// [`Store::restore_status`] is a different entry point from [`Store::record_status`]
    /// precisely so a caller cannot pass the difference as a flag. A row a live one already
    /// supersedes is dropped by the store rather than by this loop (§2.3).
    pub fn restore(&self, rows: &[AgentStatusRow]) -> RestoreSummary {
        let mut summary = RestoreSummary::default();
        for row in rows {
            match self.store.restore_status(row) {
                Ok(Restored::Applied) => summary.applied += 1,
                Ok(Restored::Superseded) => summary.superseded += 1,
                Err(err) => {
                    // The pane and the failure, never the row: a `waiting` row carries a
                    // question, and trap 13 keeps one out of a log line.
                    tracing::warn!(
                        pane = row.pane.as_str(),
                        %err,
                        "could not restore a spooled agent-status row"
                    );
                    summary.failed += 1;
                }
            }
        }
        summary
    }

    /// Record a live hook against `pane`, answering with the change to broadcast.
    ///
    /// `Ok(None)` is a **success**: §2.1 maps `PreCompact`, the three subagent-lifecycle
    /// events and an automatic `PostCompact` to no state at all, and dropping one is the
    /// specified behaviour rather than a failure to report. The hook is told it was received,
    /// because it was.
    ///
    /// `observed_at` is this daemon's clock at the moment the event arrived (§2.2), never
    /// anything out of the payload: a hook that spooled to disk and drained ten minutes later
    /// is ten minutes old, and a timestamp the hook chose would say otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the row cannot be persisted. The caller reports that to
    /// the hook, which spools it — so a failure here is a status delayed rather than lost.
    pub fn record(
        &self,
        pane: &PaneKey,
        event: &HookEvent,
    ) -> Result<Option<AgentStatusChange>, StoreError> {
        let Some(row) = event.to_row(pane.clone(), now()) else {
            tracing::debug!(
                pane = pane.as_str(),
                event = %event.hook_event_name,
                "§2.1 maps this event to no state; dropping it"
            );
            return Ok(None);
        };
        self.store.record_status(&row)?;
        self.change(pane, event.target(), &row)
    }

    /// One pane's whole status.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the row cannot be read back.
    pub fn status(&self, pane: &PaneKey) -> Result<Option<AgentStatus>, StoreError> {
        self.store.status(pane)
    }

    /// Every pane's status, from the table rather than from this process's memory.
    ///
    /// This asked the store for the panes **this daemon had itself seen** until
    /// [`Store::statuses`] existed, and the difference is not academic: spool a row with no
    /// daemon, start one and let it drain, kill it, start another — and the second daemon
    /// answered `[]` here while `status(pane)` returned the row and SQLite held exactly one.
    /// §2 has the sidebar render this list, so every restart blanked it until each pane's
    /// next hook, including the `restored_unconfirmed` rows the drain had just recovered.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a row cannot be read back.
    pub fn statuses(&self) -> Result<Vec<AgentStatus>, StoreError> {
        self.store.statuses()
    }

    /// Start sending changes to `sink` on `connection`.
    ///
    /// **Called after the answer carrying the id has been written**, never before. A frame
    /// that overtook that answer would name an id the client has not recorded, and proto's
    /// own routing rule classifies one of those as a desync that costs the whole connection —
    /// so the daemon is the side that has to supply the ordering, exactly as it does for a
    /// session attach.
    pub fn subscribe(&self, connection: ConnectionKey, sink: Arc<StreamSink>) {
        lock(&self.subscribers).push(Subscription { connection, sink });
    }

    /// Stop sending changes to one id on one connection, reporting whether it was there.
    pub fn unsubscribe(&self, connection: ConnectionKey, stream_id: StreamId) -> bool {
        let mut subscribers = lock(&self.subscribers);
        let before = subscribers.len();
        subscribers.retain(|subscription| {
            let matched =
                subscription.connection == connection && subscription.sink.stream_id() == stream_id;
            if matched {
                subscription.sink.close();
            }
            !matched
        });
        subscribers.len() != before
    }

    /// How many subscriptions are live.
    #[must_use]
    pub fn subscribers(&self) -> usize {
        lock(&self.subscribers).len()
    }

    /// Send `change` to every subscriber, forgetting the ones that have gone.
    ///
    /// A change that will not fit is **dropped rather than queued**. Every frame carries the
    /// pane's whole status rather than a diff, so a subscriber that missed one is correct
    /// again at the next one — and a status frame spends no credit, so a full outbox is a
    /// client that is not reading its socket rather than one that is behind on its renderer.
    pub fn broadcast(&self, change: &AgentStatusChange) {
        let Ok(payload) = serde_json::to_vec(change) else {
            // A change this daemon composed that will not serialise is a bug here, and there
            // is nothing a subscriber could do about it. The pane and nothing else: the
            // change carries a `waiting` question (trap 13).
            tracing::error!(
                pane = change.status.pane().as_str(),
                "could not render an agent-status change; no subscriber will see it"
            );
            return;
        };
        let mut subscribers = lock(&self.subscribers);
        subscribers.retain(|subscription| {
            let frame = Frame::new(
                FrameKind::AgentStatus,
                subscription.sink.stream_id(),
                payload.clone(),
            );
            match subscription.sink.send(&frame) {
                crate::rpc::stream::SendOutcome::Sent => true,
                crate::rpc::stream::SendOutcome::WouldBlock => {
                    tracing::debug!(
                        stream = subscription.sink.stream_id().get(),
                        "an agent-status subscriber is not reading; dropping this change"
                    );
                    true
                }
                crate::rpc::stream::SendOutcome::Closed => false,
            }
        });
    }

    /// Forget every subscription riding one connection.
    pub fn forget_connection(&self, connection: ConnectionKey) {
        lock(&self.subscribers).retain(|subscription| subscription.connection != connection);
    }

    /// The change a freshly written `row` produced, or `None` when the pane has no lead yet.
    ///
    /// A pane whose subagent reported before its lead did has no [`AgentStatus`] to send —
    /// the type has a lead and not an optional one, and inventing a `done` for the lead so
    /// the shape fits would be a dot nothing observed. The row is persisted either way; it is
    /// only the broadcast that waits for the lead.
    fn change(
        &self,
        pane: &PaneKey,
        changed: StatusTarget,
        row: &AgentStatusRow,
    ) -> Result<Option<AgentStatusChange>, StoreError> {
        let Some(status) = self.store.status(pane)? else {
            tracing::debug!(
                pane = pane.as_str(),
                "a subagent reported before its lead did; nothing to broadcast yet"
            );
            return Ok(None);
        };
        Ok(Some(AgentStatusChange {
            status,
            changed,
            // From the row that was just written, not from the one read back: `notify` is a
            // fact about the change, and the read-back is the pane's whole status after it.
            notify: row.notify(),
        }))
    }
}

/// What a spool drain did once the store had seen it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoreSummary {
    /// Rows written, flagged `restored_unconfirmed`.
    pub applied: usize,
    /// Rows a live row already superseded (§2.3).
    pub superseded: usize,
    /// Rows the store refused.
    pub failed: usize,
}

/// This daemon's clock, in Unix milliseconds.
fn now() -> UnixMillis {
    UnixMillis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
    )
}

/// Take a lock, treating poisoning as the data still being usable.
///
/// The same rule the rest of `rpc` uses: a panic while holding one of these leaves the `Vec`
/// behind it intact, and refusing to serve status for the life of the process afterwards would
/// turn a recoverable fault into a permanent one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nysia_proto::{AgentState, HookEventName, Notify, NotifySuppressed};

    fn store(tag: &str) -> (std::path::PathBuf, AgentStatusService) {
        let dir = std::env::temp_dir().join(format!("nysia-status-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let service = AgentStatusService::open(&dir.join("nysia.db")).expect("a store opens");
        (dir, service)
    }

    fn pane() -> PaneKey {
        PaneKey::new("tab_1", "leaf_1").expect("a well-formed pane key")
    }

    #[test]
    fn a_stop_hook_becomes_a_done_row_for_its_pane() {
        // §2.1's first row, through the path the acceptance test drives.
        let (dir, service) = store("stop");
        let change = service
            .record(&pane(), &HookEvent::new(HookEventName::Stop))
            .expect("the row persists")
            .expect("a Stop maps to a state");
        assert_eq!(change.status.lead.state, AgentState::Done);
        assert_eq!(change.changed, StatusTarget::Lead);
        assert_eq!(change.notify, Notify::Permitted);
        assert_eq!(
            service.statuses().expect("the list reads").len(),
            1,
            "the pane the hook named is in the list"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_event_mapped_to_nothing_is_received_and_not_recorded() {
        // §2.1 calls `PreCompact` deliberately unmapped. The hook still succeeded, so this is
        // `Ok(None)` and not an error — a failure here would have the hook spool an event
        // that is specified to be dropped, and hand it to the next daemon to drop again.
        let (dir, service) = store("unmapped");
        assert!(
            service
                .record(&pane(), &HookEvent::new(HookEventName::PreCompact))
                .expect("an unmapped event is not a failure")
                .is_none()
        );
        assert!(service.status(&pane()).expect("the read runs").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_start_is_done_and_may_not_notify() {
        let (dir, service) = store("boundary");
        let change = service
            .record(&pane(), &HookEvent::new(HookEventName::SessionStart))
            .expect("the row persists")
            .expect("a SessionStart maps to done");
        assert_eq!(change.status.lead.state, AgentState::Done);
        assert_eq!(
            change.notify,
            Notify::Suppressed {
                reason: NotifySuppressed::SessionBoundary
            },
            "§2.1: a session boundary must never notify"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_row_lands_unconfirmed_and_raises_no_notification() {
        let (dir, service) = store("restored");
        let spooled = AgentStatusRow {
            pane: pane(),
            state: AgentState::Working,
            question: None,
            is_interrupt: false,
            session_boundary: false,
            agent_id: None,
            observed_at: UnixMillis(1_000),
            // Even a row claiming to be live: the store forces the flag, and §2.3 is the
            // reason the two entry points are separate rather than one with a parameter.
            restored_unconfirmed: false,
        };
        let summary = service.restore(&[spooled]);
        assert_eq!(
            summary,
            RestoreSummary {
                applied: 1,
                superseded: 0,
                failed: 0
            }
        );

        let status = service
            .status(&pane())
            .expect("the read runs")
            .expect("the restored row is there");
        assert!(status.lead.restored_unconfirmed);
        assert_eq!(
            status.lead.notify(),
            Notify::Suppressed {
                reason: NotifySuppressed::RestoredUnconfirmed
            }
        );
        assert_eq!(
            service.statuses().expect("the list reads").len(),
            1,
            "a drained pane is a pane the list knows"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_daemon_lists_what_the_first_one_drained() {
        // The defect the in-memory pane index had, as the sequence that exposes it: a row is
        // spooled with no daemon, daemon #1 drains it, daemon #1 dies, daemon #2 starts on the
        // database it inherited — and has itself seen no hook for that pane. It answered `[]`
        // while `status(pane)` returned the row, so §2's sidebar went blank on every restart.
        let (dir, first) = store("second-daemon");
        assert_eq!(
            first.restore(&[AgentStatusRow {
                pane: pane(),
                state: AgentState::Working,
                question: None,
                is_interrupt: false,
                session_boundary: false,
                agent_id: None,
                observed_at: UnixMillis(1_000),
                restored_unconfirmed: false,
            }]),
            RestoreSummary {
                applied: 1,
                superseded: 0,
                failed: 0
            }
        );
        let path = first.store().path().to_path_buf();
        drop(first);

        // A service that has never been told anything, on the database the first one left.
        let second = AgentStatusService::open(&path).expect("the second daemon opens it");
        let listed = second.statuses().expect("the list reads");
        assert_eq!(listed.len(), 1, "a restart must not blank the sidebar");
        assert_eq!(listed[0].lead.pane, pane());
        assert!(
            listed[0].lead.restored_unconfirmed,
            "and it is still the unconfirmed row §2.3 says never counts as fresh"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spooled_row_a_live_one_already_beat_is_dropped() {
        // §2.3's sentence as a case: a write failed at 09:50 and spooled, a later hook at
        // 09:55 succeeded, the daemon restarted, and the drain arrives holding the older row.
        let (dir, service) = store("superseded");
        service
            .record(&pane(), &HookEvent::new(HookEventName::Stop))
            .expect("the live row persists");
        let older = AgentStatusRow {
            pane: pane(),
            state: AgentState::Working,
            question: None,
            is_interrupt: false,
            session_boundary: false,
            agent_id: None,
            observed_at: UnixMillis(1),
            restored_unconfirmed: false,
        };
        assert_eq!(
            service.restore(&[older]),
            RestoreSummary {
                applied: 0,
                superseded: 1,
                failed: 0
            }
        );
        assert_eq!(
            service
                .status(&pane())
                .expect("the read runs")
                .expect("the live row stands")
                .lead
                .state,
            AgentState::Done,
            "a rehydrated row never displaces a live one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_row_that_has_gone_stale_is_still_working_when_it_is_read_back() {
        // §2.3's decay is derived at read time and is not a fifth state. The daemon's whole
        // obligation is to leave the row alone, so this asserts what it did *not* do.
        let (dir, service) = store("stale");
        service
            .record(&pane(), &HookEvent::new(HookEventName::UserPromptSubmit))
            .expect("the row persists");
        let lead = service
            .status(&pane())
            .expect("the read runs")
            .expect("the row is there")
            .lead;
        assert_eq!(lead.state, AgentState::Working);
        assert!(
            !lead.is_stale(lead.observed_at),
            "a row observed now is fresh"
        );
        assert!(
            lead.is_stale(UnixMillis(
                lead.observed_at.get() + nysia_proto::AGENT_STATUS_STALE_AFTER_MS + 1
            )),
            "and the same row, read half an hour later, is stale without having changed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subagent_event_updates_the_roster_and_leaves_the_lead_alone() {
        let (dir, service) = store("roster");
        service
            .record(&pane(), &HookEvent::new(HookEventName::Stop))
            .expect("the lead reports");

        let mut working = HookEvent::new(HookEventName::UserPromptSubmit);
        working.agent_id = Some("sub_1".to_owned());
        let change = service
            .record(&pane(), &working)
            .expect("the roster row persists")
            .expect("a mapped event is a change");
        assert_eq!(
            change.changed,
            StatusTarget::Subagent {
                agent_id: "sub_1".to_owned()
            }
        );
        assert_eq!(
            change.status.lead.state,
            AgentState::Done,
            "§2.1: the lead keeps its own state"
        );
        assert_eq!(change.status.subagents.len(), 1);
        assert_eq!(change.status.subagents[0].state, AgentState::Working);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_working_tool_input_never_reaches_the_row() {
        // Trap 13/14 at this layer: `nysia-proto` confines `tool_input` to `waiting` and the
        // store applies the same rule on the way to disk. This holds that the ingest path
        // does not widen either.
        let (dir, service) = store("question");
        let mut writing = HookEvent::new(HookEventName::PreToolUse);
        writing.tool_name = Some("Write".to_owned());
        writing.tool_input = Some(serde_json::json!({ "content": "a secret" }));
        let change = service
            .record(&pane(), &writing)
            .expect("the row persists")
            .expect("a PreToolUse maps to working");
        assert_eq!(change.status.lead.state, AgentState::Working);
        assert!(change.status.lead.question.is_none());

        let mut asking = HookEvent::new(HookEventName::PreToolUse);
        asking.tool_name = Some("AskUserQuestion".to_owned());
        asking.tool_input = Some(serde_json::json!({ "question": "which?" }));
        let change = service
            .record(&pane(), &asking)
            .expect("the row persists")
            .expect("an AskUserQuestion maps to waiting");
        assert_eq!(change.status.lead.state, AgentState::Waiting);
        assert!(
            change.status.lead.question.is_some(),
            "a waiting row is the one that keeps it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
