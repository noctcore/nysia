//! Worktrees, keyed by branch.
//!
//! D-6: a worktree is identified by its branch and never by a task id. Task-keying caused
//! three shipped bugs in the system Nysia is replacing, and is free to get right now.
//!
//! Will own creation, discovery, adoption of a worktree that already exists on disk,
//! pruning, and the lexical confinement gates that keep an agent's file access inside the
//! worktree it was given.
//!
//! Owned after v0.1; the scaffold reserves the module so the layering is visible from the
//! first commit.
