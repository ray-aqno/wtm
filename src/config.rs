//! Configuration: worktree path template, timings, and agent match patterns.
//!
//! Kept deliberately small (Karpathy Rule 2 — nothing speculative). Only the
//! fields the approved stages actually consume exist. A real TOML loader is a
//! future stage; today the defaults are the contract and the worktrees root is
//! derived from the repo so Stage 1's success metric needs no config file.

use std::path::{Path, PathBuf};

/// Resolved configuration for one `wtm` process.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory under which new worktrees are created. Derived default:
    /// `<repo-parent>/<repo-name>-worktrees`.
    pub worktrees_root: PathBuf,
    /// Backstop poll interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Per git-call timeout in milliseconds.
    pub git_timeout_ms: u64,
    /// cmdline substrings that classify an agent process, longest-first.
    pub agent_patterns: Vec<(String, String)>,
}

impl Config {
    /// Build the default config for a repository at `repo_root`.
    pub fn for_repo(repo_root: &Path) -> Self {
        assert!(repo_root.is_absolute(), "repo_root must be absolute");
        let name = repo_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string());
        let parent = repo_root.parent().unwrap_or(repo_root);
        let worktrees_root = parent.join(format!("{name}-worktrees"));
        assert!(!name.is_empty(), "repo name must be non-empty");
        Config {
            worktrees_root,
            poll_interval_ms: 5_000,
            git_timeout_ms: 10_000,
            agent_patterns: vec![
                ("claude".into(), "claude".into()),
                ("hermes".into(), "hermes".into()),
                ("codex".into(), "codex".into()),
                ("aider".into(), "aider".into()),
            ],
        }
    }

    /// Derive the worktree path for a branch under the worktrees root.
    pub fn worktree_path_for(&self, segment: &str) -> PathBuf {
        self.worktrees_root.join(segment)
    }
}
