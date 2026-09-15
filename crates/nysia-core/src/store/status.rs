//! The §2.2 agent-status row on disk: written, capped, rehydrated and read back.
//!
//! The contract is `docs/plans/v0.2-delivery-plan.md` §2, which is the single authority.
//! `nysia-proto` owns [`AgentStatusRow`]; this module owns the table it lands in, and
//! **defines no second [`AgentState`]** — §2.1 fixes it at four, the crate refuses a fifth,
//! and a `TEXT` column holding the wire spelling is how it stays that way rather than an
//! integer discriminant this module would have to keep in step by hand.
//!
//! # Two entry points, because they are two different claims
//!
//! [`Store::record_status`] is a live hook: something just happened. [`Store::restore_status`]
//! is the daemon draining the disk spool on start: something happened before the last
//! restart, and nobody has confirmed it since. §2.3 says the second **never counts as
//! fresh**, so the two cannot be one function with a flag a caller passes — the flag is the
//! whole difference, and a caller that gets it wrong turns a half-hour-old row into a toast.
//! Each entry point sets [`Provenance`] itself and neither takes it as an argument, so
//! `restored_unconfirmed` is not a field the caller can reach.
//!
//! # What the store rewrites, and what it does not
//!
//! Exactly two fields, and both for a reason outside this module:
//!
//! - `restored_unconfirmed`, as above.
//! - `question`, which is dropped unless the row is `waiting`. `nysia-proto` confines
//!   `tool_input` to `waiting` on the wire; this is the same rule at the disk boundary, for
//!   trap 14, and it lives in the one private path both entry points share so that neither
//!   can be the one that forgets.
//!
//! Everything else is persisted verbatim, including `is_interrupt`. Deriving that from
//! `state` here would be a second opinion on a field `nysia-proto` already derives and holds
//! with a test, and a persistence layer that quietly edits what it was handed is one whose
//! round-trip cannot be asserted.
//!
//! # Staleness is read-side, and is not stored
//!
//! §2.3's thirty minutes decays a `working` dot to "active", and "active" is **not** an
//! [`AgentState`]. Nothing here writes it, nothing here computes it, and there is no
//! `is_stale` column: the row carries `observed_at` and the reader calls
//! [`AgentStatusRow::is_stale`], which is how PR #69 models it. A stored staleness would be
//! wrong one millisecond after it was written.
//!
//! The store's obligation is therefore narrow and exact: `observed_at` must survive the
//! round-trip **to the millisecond**, because the threshold is a comparison against it.
//! `a_row_at_the_staleness_boundary_reads_back_exact` is that obligation.

use std::path::Path;

use nysia_proto::{
    AGENT_STATUS_HISTORY_CAP, AgentState, AgentStatus, AgentStatusRow, PaneKey, UnixMillis,
};
use rusqlite::{Transaction, TransactionBehavior};

use super::Store;
use super::error::StoreError;

/// The eight §2.2 columns, in §2.2's order, for every `SELECT` in this module.
///
/// One string rather than one per query, because the column order is what the index
/// positions in [`RawRow::read`] mean. Two queries listing them differently is a row read
/// with its fields shuffled, and every field here is either a string or a boolean, so it
/// would not even fail to parse.
const COLUMNS: &str = "pane, state, question, is_interrupt, session_boundary, agent_id, observed_at, \
     restored_unconfirmed";

/// Where a row came from.
///
/// Never a parameter of a public function: see the module doc. It exists so the one private
/// insert path can be shared without the difference between a live row and a rehydrated one
/// becoming a boolean two call sites could pass the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// A hook that just fired.
    Live,
    /// A row drained from the disk spool, which §2.3 says never counts as fresh.
    Spool,
}

impl Provenance {
    /// What lands in `restored_unconfirmed`.
    const fn restored_unconfirmed(self) -> bool {
        matches!(self, Self::Spool)
    }
}

/// What [`Store::restore_status`] did.
///
/// An enum rather than a `bool` or a silent write, because "the spool had a row and it was
/// dropped" is a thing the drain wants to count and a thing a test has to be able to assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restored {
    /// The row was written, flagged `restored_unconfirmed`.
    Applied,
    /// Nothing was written: a **live** row at least as recent already stood for this agent.
    ///
    /// §2.3 says a rehydrated row never counts as fresh, and a row that displaced a live one
    /// would be counting as the freshest thing there is. The case is real rather than
    /// theoretical: a write that failed at 09:50 is spooled, a later hook at 09:55 succeeds,
    /// the daemon restarts, and the drain arrives with the older row in its hand.
    Superseded,
}

impl Store {
    /// Persist a row from a live hook.
    ///
    /// The row is appended to the agent's history and the history is trimmed to
    /// [`AGENT_STATUS_HISTORY_CAP`], both in one transaction, so no reader ever sees the
    /// moment in between where the history is one over the cap.
    ///
    /// `restored_unconfirmed` is forced `false` whatever the caller's row says: a live hook
    /// is by definition not a rehydrated one.
    ///
    /// # Errors
    ///
    /// - [`StoreError::Timestamp`] if `observed_at` does not fit an `i64`.
    /// - [`StoreError::Sqlite`] if the write fails, in which case nothing was written.
    pub fn record_status(&self, row: &AgentStatusRow) -> Result<(), StoreError> {
        let mut conn = self.conn()?;
        let tx = begin(&mut conn, &self.path)?;
        insert(&tx, row, Provenance::Live, &self.path)?;
        trim(&tx, row, &self.path)?;
        commit(tx, &self.path)
    }

    /// Persist a row the daemon drained from the disk spool.
    ///
    /// Flags it `restored_unconfirmed` — always, and whatever the caller's row says — so
    /// that §2.1's second suppression applies and it raises no notification. `observed_at` is
    /// kept as the spool recorded it and is **not** restamped: a row written now with today's
    /// clock is a row that claims to be fresh, which is the one thing §2.3 forbids.
    ///
    /// Returns [`Restored::Superseded`] without writing when a live row for the same agent is
    /// at least as recent.
    ///
    /// **It appends, and it is not idempotent.** The guard consults live rows only, so
    /// replaying one spool entry twice writes it twice — and while a re-drain is in flight the
    /// newest row by `seq` is whichever entry was replayed last, which for a re-run is the
    /// oldest one. The alternative, refusing any row not strictly newer than everything
    /// stored, would silently drop the second of two genuine events sharing a millisecond,
    /// which a status history has no way to recover. So the drain owns not replaying what it
    /// has already handed over (W4), and the cap bounds the damage if it does.
    ///
    /// # Errors
    ///
    /// As [`Store::record_status`].
    pub fn restore_status(&self, row: &AgentStatusRow) -> Result<Restored, StoreError> {
        let mut conn = self.conn()?;
        // `IMMEDIATE`, because this reads and then writes. A deferred transaction would start
        // as a reader and, if another writer committed in between, fail the write with
        // `SQLITE_BUSY_SNAPSHOT` — which the busy timeout does not retry, because there is no
        // waiting that can fix a snapshot that is already stale.
        let tx = begin(&mut conn, &self.path)?;
        if live_row_at_least_as_recent(&tx, row, &self.path)? {
            return Ok(Restored::Superseded);
        }
        insert(&tx, row, Provenance::Spool, &self.path)?;
        trim(&tx, row, &self.path)?;
        commit(tx, &self.path)?;
        Ok(Restored::Applied)
    }

    /// The newest row for an agent in `pane`, or `None` when it has no history.
    ///
    /// `agent_id` is `None` for the lead and `Some` for a subagent, matching §2.2's one
    /// shape for both.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::State`] or [`StoreError::Pane`] if the stored row is not one
    /// this build can read.
    pub fn current_status(
        &self,
        pane: &PaneKey,
        agent_id: Option<&str>,
    ) -> Result<Option<AgentStatusRow>, StoreError> {
        let conn = self.conn()?;
        let mut statement = conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM agent_status
                  WHERE pane = ?1 AND agent_id IS ?2
                  ORDER BY seq DESC LIMIT 1"
            ))
            .map_err(self.sqlite("prepare the current-status query"))?;
        let mut rows = statement
            .query(rusqlite::params![pane.as_str(), agent_id])
            .map_err(self.sqlite("read the current status"))?;
        let Some(row) = rows
            .next()
            .map_err(self.sqlite("read the current status"))?
        else {
            return Ok(None);
        };
        RawRow::read(row)
            .map_err(self.sqlite("read the current status"))?
            .decode(&self.path)
            .map(Some)
    }

    /// One pane's whole status: the lead's row and the subagent roster.
    ///
    /// `None` when the lead has no row. A pane whose subagents have reported but whose lead
    /// has not is not a status — [`AgentStatus`] has a lead, not an optional one — and
    /// inventing a `done` for the lead so the shape fits would be a dot nothing observed.
    ///
    /// The roster is ordered by `agent_id`, so two reads of an unchanged table agree.
    ///
    /// # Errors
    ///
    /// As [`Store::current_status`].
    pub fn status(&self, pane: &PaneKey) -> Result<Option<AgentStatus>, StoreError> {
        let Some(lead) = self.current_status(pane, None)? else {
            return Ok(None);
        };
        let conn = self.conn()?;
        let mut statement = conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM agent_status
                  WHERE pane = ?1
                    AND agent_id IS NOT NULL
                    AND seq IN (
                        SELECT MAX(seq) FROM agent_status
                         WHERE pane = ?1 AND agent_id IS NOT NULL
                         GROUP BY agent_id
                    )
                  ORDER BY agent_id"
            ))
            .map_err(self.sqlite("prepare the roster query"))?;
        let raws = statement
            .query_map([pane.as_str()], RawRow::read)
            .map_err(self.sqlite("read the roster"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlite("read the roster"))?;
        let subagents = raws
            .into_iter()
            .map(|raw| raw.decode(&self.path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(AgentStatus { lead, subagents }))
    }

    /// An agent's history, newest first.
    ///
    /// **No `LIMIT`.** The cap is enforced on the way in, and a read that silently returned
    /// the newest twenty of a table holding thirty would hide exactly the defect
    /// `the_history_stops_at_the_cap` exists to catch.
    ///
    /// # Errors
    ///
    /// As [`Store::current_status`].
    pub fn status_history(
        &self,
        pane: &PaneKey,
        agent_id: Option<&str>,
    ) -> Result<Vec<AgentStatusRow>, StoreError> {
        let conn = self.conn()?;
        let mut statement = conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM agent_status
                  WHERE pane = ?1 AND agent_id IS ?2
                  ORDER BY seq DESC"
            ))
            .map_err(self.sqlite("prepare the history query"))?;
        let raws = statement
            .query_map(rusqlite::params![pane.as_str(), agent_id], RawRow::read)
            .map_err(self.sqlite("read the history"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlite("read the history"))?;
        raws.into_iter().map(|raw| raw.decode(&self.path)).collect()
    }

    /// A [`StoreError::Sqlite`] builder carrying this store's path.
    fn sqlite(&self, action: &'static str) -> impl Fn(rusqlite::Error) -> StoreError + '_ {
        move |source| StoreError::Sqlite {
            action,
            path: self.path.clone(),
            source,
        }
    }
}

/// Open a write transaction that takes the write lock immediately.
fn begin<'a>(
    conn: &'a mut rusqlite::Connection,
    path: &Path,
) -> Result<Transaction<'a>, StoreError> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| StoreError::Sqlite {
            action: "begin a status write",
            path: path.to_path_buf(),
            source,
        })
}

/// Commit, reporting the path if it fails.
fn commit(tx: Transaction<'_>, path: &Path) -> Result<(), StoreError> {
    tx.commit().map_err(|source| StoreError::Sqlite {
        action: "commit a status write",
        path: path.to_path_buf(),
        source,
    })
}

/// The one path to disk, shared by both entry points.
///
/// The two confinements live here rather than in the callers: `question` is dropped unless
/// the row is `waiting`, and `restored_unconfirmed` comes from [`Provenance`] rather than
/// from the row. A rule applied in two places is a rule that is applied in one of them.
fn insert(
    tx: &Transaction<'_>,
    row: &AgentStatusRow,
    provenance: Provenance,
    path: &Path,
) -> Result<(), StoreError> {
    // Trap 14, at the disk boundary. A `working` row's `tool_input` is the whole file a
    // `Write` was about to create; §2.2 asks for the `waiting` payload and nothing else.
    let question = match row.question.as_ref().filter(|_| is_waiting(row)) {
        Some(value) => {
            Some(
                serde_json::to_string(value).map_err(|source| StoreError::Question {
                    action: "encode",
                    path: path.to_path_buf(),
                    source,
                })?,
            )
        }
        None => None,
    };
    let observed_at = i64::try_from(row.observed_at.get()).map_err(|_| StoreError::Timestamp {
        millis: row.observed_at.get(),
    })?;

    tx.execute(
        "INSERT INTO agent_status
             (pane, state, question, is_interrupt, session_boundary, agent_id, observed_at,
              restored_unconfirmed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            row.pane.as_str(),
            row.state.as_str(),
            question,
            row.is_interrupt,
            row.session_boundary,
            row.agent_id.as_deref(),
            observed_at,
            provenance.restored_unconfirmed(),
        ],
    )
    .map_err(|source| StoreError::Sqlite {
        action: "insert a status row",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Drop everything past the cap for the agent `row` belongs to.
///
/// Absolute rather than "delete one per insert": the `NOT IN (… ORDER BY seq DESC LIMIT n)`
/// says what the history should be rather than how much to remove, so a history that is over
/// the cap for any reason — two writers, an interrupted process, a database restored from a
/// backup — converges on the next write instead of staying over it forever.
fn trim(tx: &Transaction<'_>, row: &AgentStatusRow, path: &Path) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM agent_status
          WHERE pane = ?1
            AND agent_id IS ?2
            AND seq NOT IN (
                SELECT seq FROM agent_status
                 WHERE pane = ?1 AND agent_id IS ?2
                 ORDER BY seq DESC
                 LIMIT ?3
            )",
        rusqlite::params![
            row.pane.as_str(),
            row.agent_id.as_deref(),
            i64::from(AGENT_STATUS_HISTORY_CAP),
        ],
    )
    .map_err(|source| StoreError::Sqlite {
        action: "trim the status history",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Whether a live row for this agent is at least as recent as `row`.
fn live_row_at_least_as_recent(
    tx: &Transaction<'_>,
    row: &AgentStatusRow,
    path: &Path,
) -> Result<bool, StoreError> {
    let observed_at = i64::try_from(row.observed_at.get()).map_err(|_| StoreError::Timestamp {
        millis: row.observed_at.get(),
    })?;
    let found: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM agent_status
              WHERE pane = ?1
                AND agent_id IS ?2
                AND restored_unconfirmed = 0
                AND observed_at >= ?3",
            rusqlite::params![row.pane.as_str(), row.agent_id.as_deref(), observed_at],
            |found| found.get(0),
        )
        .map_err(|source| StoreError::Sqlite {
            action: "look for a live status row",
            path: path.to_path_buf(),
            source,
        })?;
    Ok(found > 0)
}

/// Whether a row is the one state §2.2 keeps a payload for.
const fn is_waiting(row: &AgentStatusRow) -> bool {
    matches!(row.state, AgentState::Waiting)
}

/// A row exactly as SQLite holds it, before it is anything typed.
///
/// A separate step because `rusqlite`'s row closure may only fail with a
/// [`rusqlite::Error`], and "the stored state is not one of the four" is a
/// [`StoreError::State`] — an error that names the value and the database. Reading first and
/// decoding after is what lets it.
struct RawRow {
    pane: String,
    state: String,
    question: Option<String>,
    is_interrupt: bool,
    session_boundary: bool,
    agent_id: Option<String>,
    observed_at: i64,
    restored_unconfirmed: bool,
}

impl RawRow {
    /// Read the [`COLUMNS`], in their order.
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            pane: row.get(0)?,
            state: row.get(1)?,
            question: row.get(2)?,
            is_interrupt: row.get(3)?,
            session_boundary: row.get(4)?,
            agent_id: row.get(5)?,
            observed_at: row.get(6)?,
            restored_unconfirmed: row.get(7)?,
        })
    }

    /// Turn it into the proto row, refusing anything this build cannot read.
    fn decode(self, path: &Path) -> Result<AgentStatusRow, StoreError> {
        let pane = self
            .pane
            .parse::<PaneKey>()
            .map_err(|source| StoreError::Pane {
                path: path.to_path_buf(),
                value: self.pane.clone(),
                source,
            })?;
        let state = self
            .state
            .parse::<AgentState>()
            .map_err(|source| StoreError::State {
                path: path.to_path_buf(),
                value: self.state.clone(),
                source,
            })?;
        let question = match self.question {
            Some(text) => {
                Some(
                    serde_json::from_str(&text).map_err(|source| StoreError::Question {
                        action: "decode",
                        path: path.to_path_buf(),
                        source,
                    })?,
                )
            }
            None => None,
        };
        let observed_at =
            u64::try_from(self.observed_at).map_err(|_| StoreError::NegativeTimestamp {
                path: path.to_path_buf(),
                millis: self.observed_at,
            })?;
        Ok(AgentStatusRow {
            pane,
            state,
            question,
            is_interrupt: self.is_interrupt,
            session_boundary: self.session_boundary,
            agent_id: self.agent_id,
            observed_at: UnixMillis(observed_at),
            restored_unconfirmed: self.restored_unconfirmed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests_support::{other_pane, pane, row, subagent, temp_db, waiting};
    use nysia_proto::{AGENT_STATUS_STALE_AFTER_MS, Notify, NotifySuppressed};
    use serde_json::json;

    /// A store, and the directory to remove when the test is done.
    fn store(tag: &str) -> (std::path::PathBuf, Store) {
        let (dir, path) = temp_db(tag);
        let store = Store::open(&path).expect("open");
        (dir, store)
    }

    /// A second connection onto the same database, for the things a test has to assert about
    /// the table rather than about the API: what is physically stored, and what happens when
    /// a row this code would never write is present anyway.
    fn raw(store: &Store) -> rusqlite::Connection {
        rusqlite::Connection::open(store.path()).expect("open raw")
    }

    fn count(store: &Store) -> i64 {
        raw(store)
            .query_row("SELECT COUNT(*) FROM agent_status", [], |row| row.get(0))
            .expect("count")
    }

    // ---- The numbers §2.3 fixes --------------------------------------------------------

    #[test]
    fn the_cap_is_the_plans_twenty() {
        // §2.3 says "~20 states per agent". A store cannot enforce an approximation, so the
        // number it enforces is **exactly twenty**, and it is `nysia-proto`'s constant rather
        // than a second copy — the window renders what survives the cap and the store applies
        // it, and two numbers that have to agree should only be written once.
        //
        // The literal is the point: a test that wrote `AGENT_STATUS_HISTORY_CAP` on both
        // sides would agree with the constant whatever it became.
        assert_eq!(AGENT_STATUS_HISTORY_CAP, 20);
    }

    #[test]
    fn the_staleness_threshold_is_the_plans_thirty_minutes() {
        // §2.3's thirty minutes, in milliseconds, as a literal for the same reason.
        assert_eq!(AGENT_STATUS_STALE_AFTER_MS, 1_800_000);
    }

    #[test]
    fn the_table_has_the_eight_fields_of_the_contract_and_one_key() {
        // A stated total is a fact about the list beneath it. §2.2 names eight fields; the
        // table adds `seq` and nothing else, and this is what stops a ninth arriving quietly.
        let (dir, store) = store("columns");
        let conn = raw(&store);
        let mut statement = conn
            .prepare("PRAGMA table_info(agent_status)")
            .expect("prepare");
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("names");

        let columns: Vec<&str> = columns.iter().map(String::as_str).collect();
        let (key, fields) = columns.split_first().expect("a non-empty table");
        assert_eq!(*key, "seq", "the ordering key comes first");
        assert_eq!(
            fields,
            &[
                "pane",
                "state",
                "question",
                "is_interrupt",
                "session_boundary",
                "agent_id",
                "observed_at",
                "restored_unconfirmed",
            ],
            "the eight fields, spelled and ordered as the contract spells them"
        );
        assert_eq!(
            fields.len(),
            8,
            "the contract names eight, and eight is the total"
        );
        drop(statement);
        drop(conn);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- The round trip ----------------------------------------------------------------

    #[test]
    fn a_row_round_trips_through_the_database() {
        let (dir, store) = store("round-trip");
        let written = waiting(pane(), 1_757_721_600_000, json!({"header": "Auth method"}));
        store.record_status(&written).expect("record");
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(read, written, "every field survives the round trip");
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_boundary_and_an_interrupt_survive_verbatim() {
        // The two flags §2.2 keeps beside `state`. The store persists them rather than
        // re-deriving them: `nysia-proto` already derives `is_interrupt` from the state it
        // built the row with, and a persistence layer holding a second opinion is a row two
        // readers disagree about.
        let (dir, store) = store("flags");
        let interrupted = AgentStatusRow {
            state: AgentState::Interrupted,
            is_interrupt: true,
            session_boundary: false,
            ..row(pane(), 1_000)
        };
        let boundary = AgentStatusRow {
            state: AgentState::Done,
            is_interrupt: false,
            session_boundary: true,
            ..row(pane(), 2_000)
        };
        store.record_status(&interrupted).expect("interrupted");
        store.record_status(&boundary).expect("boundary");

        let history = store.status_history(&pane(), None).expect("history");
        assert_eq!(history[0], boundary);
        assert_eq!(history[1], interrupted);
        // And the notification rule that rides on the flag still reads off the stored row.
        assert_eq!(
            history[0].notify(),
            Notify::Suppressed {
                reason: NotifySuppressed::SessionBoundary
            }
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_in_the_same_millisecond_keep_their_order() {
        // Why the table has `seq` at all. A `PreToolUse` and its `PostToolUse` around a fast
        // tool land in one millisecond, and "latest by observed_at" would tie.
        let (dir, store) = store("same-ms");
        store.record_status(&row(pane(), 5_000)).expect("first");
        let second = AgentStatusRow {
            state: AgentState::Done,
            ..row(pane(), 5_000)
        };
        store.record_status(&second).expect("second");

        let current = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(
            current.state,
            AgentState::Done,
            "insertion order breaks the tie"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- The cap -----------------------------------------------------------------------

    #[test]
    fn the_history_stops_at_the_cap() {
        let (dir, store) = store("cap");
        for millis in 0..50 {
            store.record_status(&row(pane(), millis)).expect("record");
        }
        let history = store.status_history(&pane(), None).expect("history");
        // Twenty as a literal, and the row count in the table beside it: the read carries no
        // `LIMIT`, so this would not pass with thirty rows on disk.
        assert_eq!(history.len(), 20);
        assert_eq!(count(&store), 20);
        // And it is the newest twenty that survived, not the first twenty.
        assert_eq!(history[0].observed_at, UnixMillis(49));
        assert_eq!(history[19].observed_at, UnixMillis(30));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_agent_and_each_pane_has_its_own_cap() {
        // "~20 states per **agent**". `agent_id IS ?` rather than `= ?` is what makes the
        // lead's history its own: `NULL = NULL` is `NULL` in SQL, so `=` would match no lead
        // row at all and the trim would delete every one of them.
        let (dir, store) = store("cap-per-agent");
        for millis in 0..25 {
            store.record_status(&row(pane(), millis)).expect("lead");
            store
                .record_status(&subagent(pane(), millis, "sub-a"))
                .expect("subagent");
            store
                .record_status(&row(other_pane(), millis))
                .expect("other pane");
        }
        assert_eq!(store.status_history(&pane(), None).expect("lead").len(), 20);
        assert_eq!(
            store
                .status_history(&pane(), Some("sub-a"))
                .expect("sub")
                .len(),
            20
        );
        assert_eq!(
            store
                .status_history(&other_pane(), None)
                .expect("other")
                .len(),
            20
        );
        assert_eq!(
            count(&store),
            60,
            "three independent histories, capped apart"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_writers_racing_the_cap_leave_exactly_the_cap() {
        // The question a reading cannot answer. Two `Store`s on one file is two connections,
        // which is what WAL is for; both write the same agent's history at once.
        //
        // Two things are under test. The cap holds under interleaving, because the insert and
        // the trim are one transaction and the trim says what the history should be rather
        // than how much to remove. And **no write is lost**: without `busy_timeout`, SQLite
        // fails a contended write immediately with `SQLITE_BUSY` rather than waiting, and
        // half of these would be `expect`ed into a panic.
        let (dir, path) = temp_db("cap-race");
        Store::open(&path).expect("create the schema first");

        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2u64)
                .map(|writer| {
                    let path = &path;
                    scope.spawn(move || {
                        let store = Store::open(path).expect("open");
                        for step in 0..50u64 {
                            store
                                .record_status(&row(pane(), writer * 1_000 + step))
                                .expect("a contended write must not be lost");
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("thread");
            }
        });

        let store = Store::open(&path).expect("reopen");
        assert_eq!(
            store.status_history(&pane(), None).expect("history").len(),
            20
        );
        assert_eq!(count(&store), 20);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Staleness ---------------------------------------------------------------------

    #[test]
    fn a_row_at_the_staleness_boundary_reads_back_exact() {
        // The store's whole obligation to the thirty minutes: `observed_at` survives to the
        // millisecond, because the threshold is a comparison against it. The offsets are
        // literals — `1_800_000` is thirty minutes — so a store that wrote seconds, or
        // rounded, or lost the low bits turns this red rather than moving the goalposts with
        // the value it is checking.
        let (dir, store) = store("staleness");
        let now = UnixMillis(1_757_721_600_000);

        store
            .record_status(&row(pane(), now.get() - 1_800_000))
            .expect("at the boundary");
        let at_boundary = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(at_boundary.observed_at, UnixMillis(now.get() - 1_800_000));
        assert!(
            !at_boundary.is_stale(now),
            "exactly thirty minutes old is not yet stale"
        );

        store
            .record_status(&row(pane(), now.get() - 1_800_001))
            .expect("one past it");
        let past_boundary = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert!(
            past_boundary.is_stale(now),
            "one millisecond past thirty minutes is stale"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_working_row_is_still_working_on_disk() {
        // "A `working` dot decays to `active`" is a read-side decay, not a fifth state. The
        // stored state does not change, and `AgentState` still has four variants.
        let (dir, store) = store("stale-state");
        let now = UnixMillis(1_757_721_600_000);
        store
            .record_status(&row(pane(), now.get() - 3_600_000))
            .expect("an hour old");
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert!(read.is_stale(now));
        assert_eq!(
            read.state,
            AgentState::Working,
            "staleness is derived, never stored"
        );
        assert_eq!(AgentState::ALL.len(), 4, "and it did not add a state");
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Rehydration -------------------------------------------------------------------

    #[test]
    fn a_restored_row_is_unconfirmed_whatever_the_caller_said() {
        // The flag is not the caller's to set. A drain handing over a row it built with the
        // field already `false` still gets a row the contract calls unconfirmed.
        let (dir, store) = store("restore-flag");
        let lying = AgentStatusRow {
            restored_unconfirmed: false,
            ..row(pane(), 1_000)
        };
        assert_eq!(
            store.restore_status(&lying).expect("restore"),
            Restored::Applied
        );

        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert!(
            read.restored_unconfirmed,
            "the store sets the flag, not the caller"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_live_row_is_never_flagged_restored() {
        // And the other direction, which matters just as much: a live hook whose row happens
        // to carry the flag must not be persisted as a rehydrated one, or a real event would
        // silently raise no notification.
        let (dir, store) = store("live-flag");
        let lying = AgentStatusRow {
            restored_unconfirmed: true,
            ..row(pane(), 1_000)
        };
        store.record_status(&lying).expect("record");
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert!(!read.restored_unconfirmed);
        assert_eq!(read.notify(), Notify::Permitted);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_row_raises_no_notification() {
        // §2.1's second suppression, tied to the thing that persists it. A daemon restart
        // must not replay every toast the person already dismissed.
        let (dir, store) = store("restore-notify");
        store.restore_status(&row(pane(), 1_000)).expect("restore");
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(
            read.notify(),
            Notify::Suppressed {
                reason: NotifySuppressed::RestoredUnconfirmed
            }
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_row_never_displaces_a_newer_live_one() {
        // The drain race "never counts as fresh" implies: a write that failed at 09:50 is
        // spooled, a hook at 09:55 succeeds, the daemon restarts, and the drain arrives
        // holding the older row. Writing it would put a stale `working` dot over a `done` one.
        let (dir, store) = store("restore-superseded");
        let live = AgentStatusRow {
            state: AgentState::Done,
            ..row(pane(), 9_550)
        };
        store.record_status(&live).expect("live");
        assert_eq!(
            store.restore_status(&row(pane(), 9_500)).expect("restore"),
            Restored::Superseded
        );
        assert_eq!(count(&store), 1, "nothing was written");
        assert_eq!(
            store
                .current_status(&pane(), None)
                .expect("current")
                .expect("a row"),
            live
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_row_lands_when_every_live_row_is_older() {
        // The other half: a genuinely newer spooled row is not thrown away because some older
        // live row exists. "Never counts as fresh" bounds what the row *claims*, not whether
        // it is written at all.
        let (dir, store) = store("restore-applied");
        store
            .record_status(&row(pane(), 9_000))
            .expect("older live");
        assert_eq!(
            store.restore_status(&row(pane(), 9_500)).expect("restore"),
            Restored::Applied
        );
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(read.observed_at, UnixMillis(9_500));
        assert!(read.restored_unconfirmed);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_row_keeps_the_time_the_spool_recorded() {
        // Not restamped with the clock at drain time. A row stamped `now` is a row that claims
        // to be fresh, and it would never decay.
        let (dir, store) = store("restore-time");
        store.restore_status(&row(pane(), 1_000)).expect("restore");
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(read.observed_at, UnixMillis(1_000));
        assert!(read.is_stale(UnixMillis(1_000 + 1_800_001)));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_row_does_not_block_the_next_one() {
        // Only a *live* row supersedes. Two spool entries for the same agent both land, or a
        // drain would stop after its first row.
        let (dir, store) = store("restore-twice");
        store.restore_status(&row(pane(), 1_000)).expect("first");
        assert_eq!(
            store.restore_status(&row(pane(), 2_000)).expect("second"),
            Restored::Applied
        );
        assert_eq!(count(&store), 2);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_out_of_order_drain_still_lands() {
        // The `restored_unconfirmed = 0` half of rule 3, which is the half that makes the
        // guard "consults **live** rows only" rather than "consults whatever is newest".
        //
        // A spool is not sorted: the drain hands over whatever order the entries were written
        // in, and a re-drain after a crash can hand them over oldest-last. So the second entry
        // here is *older* than the first, and both must land — a restored row is not evidence
        // that anything was confirmed, so it cannot be the thing that supersedes.
        //
        // `a_restored_row_does_not_block_the_next_one` only covers the ascending order, which
        // passes with the clause dropped. This one goes red without it, with
        // `left: Superseded / right: Applied`.
        let (dir, store) = store("restore-out-of-order");
        assert_eq!(
            store.restore_status(&row(pane(), 2_000)).expect("newer"),
            Restored::Applied
        );
        assert_eq!(
            store.restore_status(&row(pane(), 1_000)).expect("older"),
            Restored::Applied,
            "a restored row is not live, so it cannot supersede the next one"
        );
        assert_eq!(count(&store), 2, "an out-of-order drain loses nothing");
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Trap 14: the question payload ---------------------------------------------------

    #[test]
    fn only_a_waiting_row_keeps_its_question() {
        // `nysia-proto` confines `tool_input` to `waiting` on the wire; this is the same rule
        // at the disk boundary, for the row that never crossed a socket. A `working`
        // `PreToolUse` carries the whole file a `Write` was about to create.
        let (dir, store) = store("question-confined");
        let secret = json!({"file_path": "/etc/shadow", "content": "a password"});
        let working = AgentStatusRow {
            question: Some(secret.clone()),
            ..row(pane(), 1_000)
        };
        store.record_status(&working).expect("record");

        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(
            read.question, None,
            "a working row's payload never reaches disk"
        );

        // And it is absent from the column, rather than merely filtered on the way out.
        let stored: Option<String> = raw(&store)
            .query_row("SELECT question FROM agent_status", [], |row| row.get(0))
            .expect("column");
        assert_eq!(stored, None);

        // The same payload on a `waiting` row is kept, which is what the contract asks for.
        store
            .record_status(&waiting(pane(), 2_000, secret.clone()))
            .expect("record waiting");
        assert_eq!(
            store
                .current_status(&pane(), None)
                .expect("current")
                .expect("a row")
                .question,
            Some(secret)
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_confinement_applies_to_a_restored_row_too() {
        // A spool file written before a restart can hold a `working` row with a payload. The
        // rule lives on the shared insert path precisely so this is not a second case to
        // remember.
        let (dir, store) = store("question-confined-restore");
        let working = AgentStatusRow {
            question: Some(json!({"content": "a password"})),
            ..row(pane(), 1_000)
        };
        store.restore_status(&working).expect("restore");
        assert_eq!(
            store
                .current_status(&pane(), None)
                .expect("current")
                .expect("a row")
                .question,
            None
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_null_question_is_not_an_absent_one() {
        // The distinction that storing text rather than a value keeps: a tool whose input is
        // the JSON literal `null` said something, and a tool with no input said nothing.
        let (dir, store) = store("question-null");
        store
            .record_status(&waiting(pane(), 1_000, json!(null)))
            .expect("record");
        let read = store
            .current_status(&pane(), None)
            .expect("current")
            .expect("a row");
        assert_eq!(read.question, Some(json!(null)));
        assert_ne!(read.question, None);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_question_survives_the_shapes_sqlite_is_fussy_about() {
        // A tool's input is arbitrary JSON and SQLite has five storage classes. Storing the
        // serialised text rather than mapping onto those classes is what makes every one of
        // these exact; each entry here is a shape a value-mapped column would lose.
        let (dir, store) = store("question-shapes");
        let payloads = [
            ("an empty object", json!({})),
            ("an empty array", json!([])),
            ("an empty string", json!("")),
            // No `INTEGER` to land in: SQLite's is signed.
            ("a u64 past i64::MAX", json!(u64::MAX)),
            ("i64::MIN", json!(i64::MIN)),
            // `1.0` and `1` are one `REAL`, and are two different JSON values.
            ("a float that is a whole number", json!(1.0)),
            ("the same number as an integer", json!(1)),
            ("a float at the top of the range", json!(1.0e308)),
            ("the smallest subnormal", json!(5.0e-324)),
            // Bound as a parameter, so this is text rather than syntax.
            (
                "text shaped like SQL",
                json!("'; DROP TABLE agent_status; --"),
            ),
            ("a backslash and a quote", json!("c:\\path\\\"quoted\"")),
            ("astral and combining unicode", json!("\u{1F600} e\u{301}")),
            (
                "the AskUserQuestion shape",
                json!({
                    "questions": [
                        {"header": "Auth", "options": [{"label": "a"}, {"label": "b"}]}
                    ]
                }),
            ),
        ];

        for (index, (name, payload)) in payloads.iter().enumerate() {
            let observed_at = 1_000 + index as u64;
            store
                .record_status(&waiting(pane(), observed_at, payload.clone()))
                .expect("record");
            let read = store
                .current_status(&pane(), None)
                .expect("current")
                .expect("a row");
            assert_eq!(
                read.question.as_ref(),
                Some(payload),
                "{name} did not survive the round trip"
            );
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_nul_in_a_question_is_escaped_rather_than_stored_raw() {
        // The shape SQLite is fussiest about. A literal NUL in a TEXT value truncates it for
        // every C-string reader that touches the file, the `sqlite3` shell included.
        // `serde_json` escapes `U+0000` on the way out, so the stored text is NUL-free and
        // the value still round-trips — which is the second reason the column holds text.
        let (dir, store) = store("question-nul");
        let payload = json!({"text": "before\u{0}after\u{1}\u{1f}"});
        store
            .record_status(&waiting(pane(), 1_000, payload.clone()))
            .expect("record");

        let stored: String = raw(&store)
            .query_row("SELECT question FROM agent_status", [], |row| row.get(0))
            .expect("column");
        assert!(!stored.contains('\0'), "the stored text carries no raw NUL");
        assert!(stored.contains("\\u0000"), "it carries the escape instead");
        assert_eq!(
            store
                .current_status(&pane(), None)
                .expect("current")
                .expect("a row")
                .question,
            Some(payload),
            "and the value is unchanged"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- The roster ----------------------------------------------------------------------

    #[test]
    fn the_roster_is_the_newest_row_per_subagent() {
        let (dir, store) = store("roster");
        store.record_status(&row(pane(), 1_000)).expect("lead");
        store
            .record_status(&subagent(pane(), 1_100, "sub-b"))
            .expect("b");
        store
            .record_status(&subagent(pane(), 1_200, "sub-a"))
            .expect("a first");
        store
            .record_status(&subagent(pane(), 1_300, "sub-a"))
            .expect("a second");

        let status = store.status(&pane()).expect("status").expect("a status");
        assert_eq!(status.lead.observed_at, UnixMillis(1_000));
        assert_eq!(status.lead.agent_id, None, "the lead keeps its own row");
        let roster: Vec<_> = status
            .subagents
            .iter()
            .map(|entry| (entry.agent_id.as_deref(), entry.observed_at))
            .collect();
        assert_eq!(
            roster,
            [
                (Some("sub-a"), UnixMillis(1_300)),
                (Some("sub-b"), UnixMillis(1_100))
            ],
            "one entry per subagent, newest, ordered by id"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pane_with_only_subagent_rows_has_no_status() {
        // `AgentStatus` has a lead, not an optional one. Inventing a `done` so the shape fits
        // would be a dot nothing observed.
        let (dir, store) = store("roster-no-lead");
        store
            .record_status(&subagent(pane(), 1_000, "sub-a"))
            .expect("subagent");
        assert!(store.status(&pane()).expect("status").is_none());
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_pane_has_no_status() {
        let (dir, store) = store("empty");
        assert!(
            store
                .current_status(&pane(), None)
                .expect("current")
                .is_none()
        );
        assert!(store.status(&pane()).expect("status").is_none());
        assert!(
            store
                .status_history(&pane(), None)
                .expect("history")
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Rows this code would not write, which are on disk anyway -------------------------

    const RAW_INSERT: &str = "INSERT INTO agent_status
             (pane, state, question, is_interrupt, session_boundary, agent_id,
              observed_at, restored_unconfirmed)
         VALUES (?1, ?2, ?3, 0, 0, NULL, ?4, 0)";

    #[test]
    fn a_state_this_build_cannot_read_is_an_error() {
        // A fifth state is refused at `FromStr`, so a database carrying one — written by a
        // newer build, or by hand — surfaces as an error naming the value rather than as a
        // panic on the daemon's read path.
        let (dir, store) = store("corrupt-state");
        raw(&store)
            .execute(
                RAW_INSERT,
                rusqlite::params!["tab1:leaf1", "sleeping", None::<String>, 1_000],
            )
            .expect("raw insert");

        let error = store
            .current_status(&pane(), None)
            .expect_err("an unknown state is refused");
        assert!(
            matches!(&error, StoreError::State { value, .. } if value == "sleeping"),
            "unexpected error: {error}"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pane_key_this_build_cannot_read_is_an_error() {
        // Driven against the decoder rather than through a query. Every query here is keyed
        // by a `PaneKey` that already parsed, so a malformed one on disk can never come back
        // from one — but `decode` still has to answer for it, because the sweep queries a
        // later wave adds will not be keyed that way.
        let malformed = RawRow {
            pane: "no-colon-here".to_owned(),
            state: "working".to_owned(),
            question: None,
            is_interrupt: false,
            session_boundary: false,
            agent_id: None,
            observed_at: 1_000,
            restored_unconfirmed: false,
        };
        let error = malformed
            .decode(Path::new("<test>"))
            .expect_err("a malformed pane key is refused");
        assert!(
            matches!(&error, StoreError::Pane { value, .. } if value == "no-colon-here"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_negative_timestamp_is_an_error_rather_than_a_row_from_1969() {
        let (dir, store) = store("corrupt-time");
        raw(&store)
            .execute(
                RAW_INSERT,
                rusqlite::params!["tab1:leaf1", "working", None::<String>, -1],
            )
            .expect("raw insert");

        let error = store
            .current_status(&pane(), None)
            .expect_err("a negative timestamp is refused");
        assert!(
            matches!(error, StoreError::NegativeTimestamp { millis: -1, .. }),
            "unexpected error: {error}"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_question_that_is_not_json_is_an_error() {
        let (dir, store) = store("corrupt-question");
        raw(&store)
            .execute(
                RAW_INSERT,
                rusqlite::params!["tab1:leaf1", "waiting", Some("not json"), 1_000],
            )
            .expect("raw insert");

        let error = store
            .current_status(&pane(), None)
            .expect_err("a non-JSON payload is refused");
        assert!(
            matches!(
                error,
                StoreError::Question {
                    action: "decode",
                    ..
                }
            ),
            "unexpected error: {error}"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
