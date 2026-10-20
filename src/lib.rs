//! `wtm` — a persistent TUI for git worktree management in tandem with AI agents.
//!
//! Library root: exposes the modules the binary and the test suite share.
//! Execution follows the approved P10 plan (Rust adapter). The P10 rules that
//! shape this code are annotated at their call sites; the static loop caps that
//! satisfy Rule 2 live here so every module shares one source of truth.

pub mod agent;
pub mod cli;
pub mod config;
pub mod error;
pub mod git;
pub mod model;
pub mod recovery;
pub mod switch;

// TUI modules are only meaningful with a terminal; kept behind the binary but
// exposed for the headless render test.
pub mod app;
pub mod refresh;
pub mod ui;

/// P10 Rule 2 — static loop caps. Every loop over an externally-sized quantity
/// breaks and marks the data degraded (never panics) when it reaches its cap.
/// These are compile-time `const`s, never runtime-configurable (arbiter
/// execution condition 7).
pub mod caps {
    /// Maximum number of worktrees rendered / iterated in one pass.
    pub const MAX_WORKTREES: usize = 1024;
    /// Maximum `/proc` entries scanned in one agent-detection pass.
    pub const MAX_PROC_ENTRIES: usize = 65_536;
    /// Maximum porcelain records parsed from a single git invocation.
    pub const MAX_PORCELAIN_RECORDS: usize = 100_000;
    /// Maximum `try_wait()` polls before a git child is killed on timeout.
    /// Derived from `timeout / poll_interval` at the call site; this is the
    /// absolute ceiling regardless of the configured timeout.
    pub const MAX_TRY_WAIT_POLLS: usize = 10_000;
    /// Recovery log rotates to a single `.1` backup past this size.
    pub const MAX_RECOVERY_LOG_BYTES: u64 = 1_048_576;
}
