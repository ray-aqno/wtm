//! The git layer: process spawning with timeouts, worktree operations, and the
//! porcelain parsers. Shells out to the `git` CLI (never libgit2) per the
//! approved plan.
//!
//! P10 Rule 7 governs every function here: each spawned process and each parse
//! returns a `Result` the caller must handle. Mutating operations
//! (`add`/`remove`) never run through the kill-on-timeout path (Safety Car #5);
//! only read-only status/list calls can be killed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::caps::{MAX_PORCELAIN_RECORDS, MAX_WORKTREES};
use crate::error::{Error, Result};
use crate::model::{AheadBehind, Worktree};

mod parse;
mod proc;

pub use parse::{parse_git_version, parse_status_v2, parse_worktree_list};

/// Minimum git version that supports `worktree list --porcelain -z`
/// (Safety Car #9). `-z` for `worktree list` is available from git 2.36.
const MIN_GIT_MAJOR: u32 = 2;
const MIN_GIT_MINOR: u32 = 36;

/// Assert at startup that the installed git is new enough. Fails fast with a
/// clear message rather than misparsing output later.
pub fn check_git_version() -> Result<(u32, u32)> {
    let out = proc::run_read(Path::new("."), &["--version"], Duration::from_secs(5))?;
    let (major, minor) = parse::parse_git_version(&out)?;
    assert!(major > 0, "git major version must be positive");
    if (major, minor) < (MIN_GIT_MAJOR, MIN_GIT_MINOR) {
        return Err(Error::Invalid(format!(
            "git {major}.{minor} is too old; need >= {MIN_GIT_MAJOR}.{MIN_GIT_MINOR}"
        )));
    }
    Ok((major, minor))
}

/// Resolve the enclosing repository's top-level directory.
pub fn repo_root(start: &Path) -> Result<PathBuf> {
    let out = proc::run_read(
        start,
        &["rev-parse", "--show-toplevel"],
        Duration::from_secs(5),
    )?;
    let line = out.trim();
    if line.is_empty() {
        return Err(Error::NotFound("not inside a git repository".into()));
    }
    let root = PathBuf::from(line);
    assert!(root.is_absolute(), "git toplevel must be absolute");
    Ok(root)
}

/// Reject a branch name that could be read as a git flag or escape the path
/// template (Safety Car #1 and #2). Returns the validated name.
pub fn validate_branch_name(name: &str) -> Result<&str> {
    if name.is_empty() {
        return Err(Error::Invalid("branch name is empty".into()));
    }
    if name.starts_with('-') {
        return Err(Error::Invalid(format!(
            "branch name may not start with '-': {name}"
        )));
    }
    let bad = ['\0', '/', '\\'];
    if name.contains("..") || name.chars().any(|c| bad.contains(&c)) {
        return Err(Error::Invalid(format!(
            "branch name may not contain '..', NUL, or path separators: {name}"
        )));
    }
    Ok(name)
}

/// Create a worktree for `branch` under `root`, returning its path.
///
/// Mutating op: runs WITHOUT the kill-on-timeout path. On failure it prunes the
/// admin stub and deletes the just-created branch so a retry with the same name
/// is not blocked (Safety Car #11).
pub fn add_worktree(repo: &Path, root: &Path, branch: &str) -> Result<PathBuf> {
    let branch = validate_branch_name(branch)?;
    let path = safe_worktree_path(root, branch)?;
    assert!(path.is_absolute(), "derived worktree path must be absolute");
    assert!(
        !path.exists(),
        "derived worktree path must not already exist"
    );
    let path_str = path.to_string_lossy().into_owned();
    // `--` separates options from the path operand (Safety Car #1) so the path
    // can never be read as a flag; `-b <branch>` precedes it and `branch` is
    // already rejected above if it looks like a flag.
    let add = ["worktree", "add", "-b", branch, "--", &path_str];
    match proc::run_mutate(repo, &add) {
        Ok(_) => Ok(path),
        Err(e) => {
            cleanup_failed_add(repo, branch);
            Err(e)
        }
    }
}

/// Best-effort cleanup after a failed `worktree add` (Safety Car #11). Errors
/// here are intentionally swallowed: this is recovery, not a primary op.
fn cleanup_failed_add(repo: &Path, branch: &str) {
    let _ = proc::run_mutate(repo, &["worktree", "prune"]);
    let _ = proc::run_mutate(repo, &["branch", "-D", branch]);
}

/// Derive and canonicalize the worktree path, asserting it stays a
/// path-component child of `root` with symlinks resolved (Safety Car #2).
fn safe_worktree_path(root: &Path, segment: &str) -> Result<PathBuf> {
    assert!(root.is_absolute(), "worktrees root must be absolute");
    let candidate = root.join(segment);
    // The leaf must not exist yet, so canonicalize the existing ancestor (root)
    // and require the candidate to start with it on a component boundary.
    let anchor = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !candidate.starts_with(&anchor) || candidate == anchor {
        return Err(Error::Invalid(format!(
            "worktree path escapes its root: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
}

/// List all worktrees of `repo` with their status, bounded by `MAX_WORKTREES`.
/// `truncated` is true when the cap was hit (Safety Car #10).
pub fn list_worktrees(repo: &Path, timeout: Duration) -> Result<(Vec<Worktree>, bool)> {
    let out = proc::run_read(repo, &["worktree", "list", "--porcelain", "-z"], timeout)?;
    let (mut worktrees, truncated) = parse_worktree_list(&out, MAX_WORKTREES)?;
    assert!(
        worktrees.len() <= MAX_WORKTREES,
        "worktree list exceeded cap"
    );
    for wt in worktrees.iter_mut() {
        enrich_status(repo, wt, timeout);
    }
    Ok((worktrees, truncated))
}

/// Fill a worktree's dirty/ahead-behind/activity fields. A timed-out or failing
/// read marks the worktree degraded rather than failing the whole list.
fn enrich_status(repo: &Path, wt: &mut Worktree, timeout: Duration) {
    let path = wt.path.to_string_lossy().into_owned();
    let status = proc::run_read(
        repo,
        &["-C", &path, "status", "--porcelain=v2", "--branch", "-z"],
        timeout,
    );
    match status.and_then(|s| parse_status_v2(&s, MAX_PORCELAIN_RECORDS)) {
        Ok((dirty, ab)) => {
            wt.dirty_count = dirty;
            wt.ahead_behind = ab;
        }
        Err(_) => wt.reachable = false,
    }
    wt.last_activity = last_activity(repo, &path, timeout).ok();
}

/// Committer timestamp of the worktree's HEAD, if resolvable.
fn last_activity(repo: &Path, path: &str, timeout: Duration) -> Result<i64> {
    let out = proc::run_read(repo, &["-C", path, "log", "-1", "--format=%ct"], timeout)?;
    let ts: i64 = out
        .trim()
        .parse()
        .map_err(|_| Error::Git("bad %ct".into()))?;
    assert!(ts >= 0, "commit timestamp must be non-negative");
    Ok(ts)
}

/// Remove a worktree. Mutating op: no kill-on-timeout path. Guards (dirty check,
/// recovery snapshot) live in `switch::guarded_remove`; this is the raw op.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    assert!(path.is_absolute(), "remove target must be absolute");
    let path_str = path.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push("--");
    args.push(&path_str);
    proc::run_mutate(repo, &args)?;
    Ok(())
}

/// The current ahead/behind for a worktree path, or `None` if detached / no
/// upstream. Thin wrapper used by callers that only need the counts.
pub fn ahead_behind(repo: &Path, path: &Path, timeout: Duration) -> Result<Option<AheadBehind>> {
    let path_str = path.to_string_lossy().into_owned();
    let out = proc::run_read(
        repo,
        &[
            "-C",
            &path_str,
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
        ],
        timeout,
    )?;
    let (_dirty, ab) = parse_status_v2(&out, MAX_PORCELAIN_RECORDS)?;
    Ok(ab)
}

/// Run a read-only git command with timeout, returning captured stdout. Public
/// so the switch module's `stash create` snapshot (which writes only a dangling
/// object, not working-tree or ref state) can share the read path.
pub fn capture_read(repo: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    proc::run_read(repo, args, timeout)
}
