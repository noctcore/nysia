//! Persistent state: SQLite for anything queried, JSON for anything hand-edited.
//!
//! D-12. Sessions, worktrees, orchestration, agent status and the scrollback index go into
//! a WAL-mode SQLite database, because orchestration needs indexes and transactions.
//! Settings and the project list stay JSON, written atomically, because a human is
//! expected to open them in an editor.
//!
//! There is no local task domain model: tasks are GitHub Issues, queried live (D-5).
//!
//! Owned by wave 2 (W4).
