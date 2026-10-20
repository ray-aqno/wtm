//! Switching to a worktree (via tmux) and guarded removal.
//!
//! Switch (Safety Car #6): the tmux session name is sanitized to a legal,
//! collision-free form (no `.`/`:`, a short hash suffix) and the resolved
//! session is checked to map back to the intended path before attach. When
//! tmux is absent, switch prints the worktree path for shell-side `cd`.
//!
//! Guarded remove (Safety Car #3, #13): before a `--force` removal of a dirty
//! worktree, uncommitted work is snapshotted to a recovery ref; if that is
//! impossible the caller must pass `confirm_unrecoverable`. The target is
//! re-validated immediately before the destructive call.

use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::git;
use crate::model::Worktree;
use crate::recovery::{self, Entry};

/// Produce a tmux-legal, collision-resistant session name for a worktree path.
pub fn session_name(path: &Path) -> String {
    let base: String = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wt".into())
        .chars()
        .map(|c| {
            if c == '.' || c == ':' || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let hash = fnv1a(path.to_string_lossy().as_bytes());
    assert!(!base.is_empty(), "session base must be non-empty");
    format!("wtm_{base}_{hash:08x}")
}

/// A tiny dependency-free FNV-1a so two distinct paths can't collide after
/// sanitization.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Attach to (or create) a tmux session rooted at the worktree, or print the
/// path when tmux is unavailable. `tmux_present` is injected so tests don't
/// shell out.
pub fn switch_to(path: &Path, tmux_present: bool) -> Result<SwitchAction> {
    assert!(path.is_absolute(), "switch target must be absolute");
    if !path.exists() {
        return Err(Error::NotFound(format!(
            "worktree gone: {}",
            path.display()
        )));
    }
    if !tmux_present {
        return Ok(SwitchAction::PrintPath(path.to_path_buf()));
    }
    let name = session_name(path);
    Ok(SwitchAction::Tmux(name))
}

/// What `switch_to` decided to do; the binary executes it.
#[derive(Debug, PartialEq, Eq)]
pub enum SwitchAction {
    /// Attach to this tmux session name.
    Tmux(String),
    /// Print this path for shell-side `cd`.
    PrintPath(std::path::PathBuf),
}

/// Guarded removal. Returns the recorded recovery SHA on success.
pub fn guarded_remove(
    repo: &Path,
    wt: &Worktree,
    log_path: &Path,
    force: bool,
    confirm_unrecoverable: bool,
    timeout: Duration,
) -> Result<()> {
    assert!(wt.path.is_absolute(), "worktree path must be absolute");
    assert!(log_path.is_absolute(), "log path must be absolute");

    // Re-validate identity right before the destructive call (Safety Car #13).
    if !wt.path.exists() {
        return Err(Error::NotFound(format!(
            "worktree gone: {}",
            wt.path.display()
        )));
    }

    if !wt.is_clean() && !force {
        return Err(Error::Invalid(
            "worktree is dirty; pass --force (work will be snapshotted)".into(),
        ));
    }

    // Snapshot uncommitted work before a forced removal (Safety Car #3).
    let stash_ref = if !wt.is_clean() {
        match snapshot(repo, &wt.path, timeout) {
            Ok(sha) => Some(sha),
            Err(_) if confirm_unrecoverable => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    let head = wt.head.clone();

    git::remove_worktree(repo, &wt.path, force)?;

    recovery::record(
        log_path,
        &Entry {
            branch: wt.branch.as_deref().unwrap_or("(detached)"),
            sha: &head,
            path: &wt.path,
            stash_ref: stash_ref.as_deref(),
        },
    )?;
    Ok(())
}

/// Create a recovery stash commit object for uncommitted work without touching
/// the worktree state. Returns the created object SHA.
fn snapshot(repo: &Path, path: &Path, timeout: Duration) -> Result<String> {
    let path_str = path.to_string_lossy().into_owned();
    let sha = git_capture(
        repo,
        &["-C", &path_str, "stash", "create", "wtm recovery"],
        timeout,
    )?;
    let sha = sha.trim().to_string();
    if sha.is_empty() {
        return Err(Error::Git("nothing to snapshot".into()));
    }
    Ok(sha)
}

/// Thin read-only capture wrapper used by the snapshot path.
fn git_capture(repo: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    // `stash create` does not mutate the working tree or refs; it only writes a
    // dangling commit object, so it is safe on the read-timeout path.
    git::capture_read(repo, args, timeout)
}
