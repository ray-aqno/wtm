//! CLI surface. `clap` derive. The non-interactive subcommands (`create`,
//! `path`, `rm`) exist so the hot path — spinning up a worktree — is one shell
//! keystroke without entering the TUI. No args launches the dashboard.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Persistent TUI + CLI for git worktree management in tandem with AI agents.
#[derive(Parser, Debug)]
#[command(name = "wtm", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The subcommands; `None` means launch the dashboard.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a worktree for a new branch and print its path.
    Create {
        /// Branch name (also the worktree directory segment).
        branch: String,
    },
    /// Print the path of the worktree for a branch (for shell `cd`).
    Path {
        /// Branch name.
        branch: String,
    },
    /// Remove a worktree, guarding against data loss.
    Rm {
        /// Branch whose worktree should be removed.
        branch: String,
        /// Remove even if the worktree is dirty (work is snapshotted first).
        #[arg(long)]
        force: bool,
        /// Proceed even if uncommitted work cannot be snapshotted.
        #[arg(long)]
        confirm_unrecoverable: bool,
    },
}

/// Where to start searching for the enclosing repo (the current directory).
pub fn start_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
