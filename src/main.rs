//! `wtm` binary entry point: parse args, resolve the repo, dispatch.

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use wtm::cli::{self, Cli, Command};
use wtm::config::Config;
use wtm::error::Result;
use wtm::{app, git, switch};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wtm: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    // Fail fast if git is too old to parse (Safety Car #9).
    git::check_git_version()?;

    let cli = Cli::parse();
    let start = cli::start_dir();
    let repo = git::repo_root(&start)?;
    let config = Config::for_repo(&repo);
    let timeout = Duration::from_millis(config.git_timeout_ms);

    match cli.command {
        None => app::run(&repo, config),
        Some(Command::Create { branch }) => {
            let path = git::add_worktree(&repo, &config.worktrees_root, &branch)?;
            println!("{}", path.display());
            Ok(())
        }
        Some(Command::Path { branch }) => {
            let seg = git::validate_branch_name(&branch)?;
            let wt = git::find_by_branch(&repo, seg, timeout)?;
            println!("{}", wt.path.display());
            Ok(())
        }
        Some(Command::Rm {
            branch,
            force,
            confirm_unrecoverable,
        }) => {
            let seg = git::validate_branch_name(&branch)?;
            let wt = git::find_by_branch(&repo, seg, timeout)?;
            let log = recovery_log_path();
            switch::guarded_remove(&repo, &wt, &log, force, confirm_unrecoverable, timeout)?;
            println!("removed {}", wt.path.display());
            Ok(())
        }
    }
}

/// Path to the removal recovery log under the user's state dir.
fn recovery_log_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("wtm").join("removed.log")
}
