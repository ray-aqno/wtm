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

/// Guarded removal. Snapshots uncommitted work to a durable recovery ref and
/// records it BEFORE the worktree is removed, so nothing is lost if the removal
/// or the log write fails.
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

    // Re-validate identity and read FRESH status right before acting, rather
    // than trusting the possibly-stale list-time snapshot (Greptile #4, #13).
    if !wt.path.exists() {
        return Err(Error::NotFound(format!(
            "worktree gone: {}",
            wt.path.display()
        )));
    }
    let dirty = fresh_dirty(repo, &wt.path, timeout)?;

    if dirty && !force {
        return Err(Error::Invalid(
            "worktree is dirty; pass --force (work will be snapshotted)".into(),
        ));
    }

    // Snapshot uncommitted work (tracked AND untracked) to a durable ref BEFORE
    // removal (Greptile #2, #3, #7). If a snapshot can't be made and the user
    // did not accept the loss, refuse.
    let stash_ref = if dirty {
        match snapshot(repo, wt, timeout) {
            Ok(r) => Some(r),
            Err(_) if confirm_unrecoverable => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    // Record the recovery entry first; the ref already protects the data.
    recovery::record(
        log_path,
        &Entry {
            branch: wt.branch.as_deref().unwrap_or("(detached)"),
            sha: &wt.head,
            path: &wt.path,
            stash_ref: stash_ref.as_deref(),
        },
    )?;

    git::remove_worktree(repo, &wt.path, force)?;
    Ok(())
}

/// Read the worktree's current dirty state, failing closed: an unreadable
/// status is treated as "assume dirty" so `--force` can't skip the snapshot on
/// a status that merely failed to read (Greptile #4).
fn fresh_dirty(repo: &Path, path: &Path, timeout: Duration) -> Result<bool> {
    match git::dirty_count(repo, path, timeout) {
        Ok(n) => Ok(n > 0),
        Err(_) => Ok(true),
    }
}

/// Snapshot uncommitted work (tracked AND untracked) to a durable ref so git
/// cannot GC it (Greptile #2, #3). Delegates to `git::snapshot_worktree`, which
/// uses a scratch index + `commit-tree` so untracked-only state is captured too
/// (plain `stash create` would silently drop it). Returns the ref name, or an
/// error if there was genuinely nothing to snapshot. `timeout` is unused here
/// because snapshot steps are mutating and run to completion off the kill path.
fn snapshot(repo: &Path, wt: &Worktree, _timeout: Duration) -> Result<String> {
    let refname = recovery_ref_name(wt);
    match git::snapshot_worktree(repo, &wt.path, &refname)? {
        Some(_sha) => Ok(refname),
        None => Err(Error::Git("nothing to snapshot".into())),
    }
}

/// A unique, legal recovery ref name for a worktree.
fn recovery_ref_name(wt: &Worktree) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hash = fnv1a(wt.path.to_string_lossy().as_bytes());
    format!("refs/wtm-recovery/{ts}-{hash:08x}")
}
