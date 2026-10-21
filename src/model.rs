//! The `Worktree` data model and related value types.
//!
//! These are plain data produced by the git layer and consumed by the UI;
//! keeping them free of behavior (P10 Rule 6 — narrow scope, no hidden state)
//! makes the porcelain parser unit-testable in isolation.

use std::path::PathBuf;

/// Ahead/behind counts relative to the upstream, computed from local refs only
/// (no `git fetch`). `None` means no upstream or a detached HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

/// A detected agent working inside a worktree (best-effort, never trusted to
/// gate a destructive action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub pid: u32,
    /// A human label classified from the process cmdline, e.g. "claude".
    pub label: String,
}

/// One git worktree plus the derived status the dashboard shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Branch short name, or `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    pub ahead_behind: Option<AheadBehind>,
    /// Number of changed (unstaged+staged+untracked) entries; 0 == clean.
    pub dirty_count: u32,
    /// Committer timestamp of the last commit (unix seconds), if resolvable.
    pub last_activity: Option<i64>,
    /// False when the worktree path is unreachable or a git call timed out:
    /// the row is shown but marked, and never auto-removed.
    pub reachable: bool,
    /// Best-effort attached agent; `None` means "unknown", never "attached".
    pub agent: Option<Agent>,
}

impl Worktree {
    /// True when there are no changes to lose.
    pub fn is_clean(&self) -> bool {
        self.dirty_count == 0
    }
}
