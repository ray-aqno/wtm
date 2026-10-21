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
use crate::model::Worktree;

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
/// admin stub and deletes the branch ONLY if this call created it, so a
/// pre-existing branch is never destroyed (Greptile #1).
pub fn add_worktree(repo: &Path, root: &Path, branch: &str) -> Result<PathBuf> {
    let branch = validate_branch_name(branch)?;
    let path = safe_worktree_path(root, branch)?;
    assert!(path.is_absolute(), "derived worktree path must be absolute");
    if path.exists() {
        // Routine user conflict, not an internal invariant: report it (Greptile
        // #17) rather than panicking on an assertion.
        return Err(Error::Invalid(format!(
            "destination already exists: {}",
            path.display()
        )));
    }
    // Did the branch already exist before we tried? If so, a failed add must not
    // delete it during cleanup.
    let pre_existing = branch_exists(repo, branch);
    let path_str = path.to_string_lossy().into_owned();
    // `--` separates options from the path operand (Safety Car #1) so the path
    // can never be read as a flag; `-b <branch>` precedes it and `branch` is
    // already rejected above if it looks like a flag.
    let add = ["worktree", "add", "-b", branch, "--", &path_str];
    match proc::run_mutate(repo, &add) {
        Ok(_) => Ok(path),
        Err(e) => {
            cleanup_failed_add(repo, branch, pre_existing);
            Err(e)
        }
    }
}

/// True if a local branch already exists.
fn branch_exists(repo: &Path, branch: &str) -> bool {
    let refname = format!("refs/heads/{branch}");
    proc::run_mutate(repo, &["show-ref", "--verify", "--quiet", "--", &refname]).is_ok()
}

/// Best-effort cleanup after a failed `worktree add` (Safety Car #11). Errors
/// here are intentionally swallowed: this is recovery, not a primary op. The
/// branch is deleted only when this call is the one that could have created it
/// (`pre_existing == false`), and with `-d` (safe) so unmerged commits block
/// the delete (Greptile #1).
fn cleanup_failed_add(repo: &Path, branch: &str, pre_existing: bool) {
    let _ = proc::run_mutate(repo, &["worktree", "prune"]);
    if !pre_existing {
        let _ = proc::run_mutate(repo, &["branch", "-d", "--", branch]);
    }
}

/// Derive and canonicalize the worktree path, asserting it stays a
/// path-component child of `root` with symlinks resolved (Safety Car #2). Both
/// sides are resolved to the same form so a symlinked worktrees root still
/// accepts its own children (Greptile #20).
fn safe_worktree_path(root: &Path, segment: &str) -> Result<PathBuf> {
    assert!(root.is_absolute(), "worktrees root must be absolute");
    let anchor = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = anchor.join(segment);
    if !candidate.starts_with(&anchor) || candidate == anchor {
        return Err(Error::Invalid(format!(
            "worktree path escapes its root: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
}

/// Find a registered worktree by its branch short-name (Greptile #8, #9). The
/// branch is the identity the CLI argument names, so callers resolve through
/// this instead of guessing a path.
pub fn find_by_branch(repo: &Path, branch: &str, timeout: Duration) -> Result<Worktree> {
    let (worktrees, _) = list_worktrees(repo, timeout)?;
    worktrees
        .into_iter()
        .find(|w| w.branch.as_deref() == Some(branch))
        .ok_or_else(|| Error::NotFound(format!("no worktree on branch {branch}")))
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

/// The current dirty-entry count for a worktree path, read fresh. Used by the
/// guarded-remove path so the decision is never based on stale list-time state.
pub fn dirty_count(repo: &Path, path: &Path, timeout: Duration) -> Result<u32> {
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
    let (dirty, _ab) = parse_status_v2(&out, MAX_PORCELAIN_RECORDS)?;
    Ok(dirty)
}

/// Build a recovery commit capturing a worktree's FULL uncommitted state —
/// working-tree changes, untracked files, AND the real index — without touching
/// the working tree, the real index, or any ref. Returns the dangling commit
/// SHA, or `None` if there is nothing uncommitted to save. The caller anchors it
/// at a unique ref (see `create_recovery_ref`).
///
/// `git stash create` is deliberately NOT used: it drops an untracked-only tree.
/// Two trees are captured so nothing is lost even when staged content differs
/// from the working copy (Greptile #1, follow-up): the working tree becomes the
/// recovery commit's tree, and the real index is committed as an extra parent so
/// its tree stays reachable from the ref.
pub fn snapshot_commit(repo: &Path, wt: &Path) -> Result<Option<String>> {
    assert!(wt.is_absolute(), "worktree path must be absolute");
    let wt_str = wt.to_string_lossy().into_owned();
    let head_tree = proc::run_mutate(repo, &["-C", &wt_str, "rev-parse", "HEAD^{tree}"])?
        .trim()
        .to_string();

    // Tree 1: the real index (staged content), read with the worktree's own
    // index (no GIT_INDEX_FILE override).
    let index_tree = proc::run_mutate(repo, &["-C", &wt_str, "write-tree"])?
        .trim()
        .to_string();
    assert!(
        !index_tree.is_empty(),
        "index write-tree produced empty oid"
    );

    // Tree 2: the working tree incl. untracked, via a scratch index.
    let idx = scratch_index_path();
    let idx_str = idx.to_string_lossy().into_owned();
    let _cleanup = ScratchIndex(idx.clone());
    git_env(repo, &["-C", &wt_str, "read-tree", "HEAD"], &idx_str)?;
    git_env(repo, &["-C", &wt_str, "add", "-A"], &idx_str)?;
    let work_tree = git_env(repo, &["-C", &wt_str, "write-tree"], &idx_str)?
        .trim()
        .to_string();
    assert!(!work_tree.is_empty(), "work write-tree produced empty oid");

    // Nothing staged and nothing in the working tree differs from HEAD.
    if work_tree == head_tree && index_tree == head_tree {
        return Ok(None);
    }

    // Commit the index tree (extra parent) so staged content stays reachable.
    let index_commit = proc::run_mutate(
        repo,
        &[
            "-C",
            &wt_str,
            "commit-tree",
            &index_tree,
            "-p",
            "HEAD",
            "-m",
            "wtm recovery (index)",
        ],
    )?
    .trim()
    .to_string();

    // Recovery commit: tree = working tree; parents = HEAD + index commit.
    let commit = git_env(
        repo,
        &[
            "-C",
            &wt_str,
            "commit-tree",
            &work_tree,
            "-p",
            "HEAD",
            "-p",
            &index_commit,
            "-m",
            "wtm recovery",
        ],
        &idx_str,
    )?
    .trim()
    .to_string();
    if commit.is_empty() {
        return Err(Error::Git("commit-tree produced no oid".into()));
    }
    Ok(Some(commit))
}

/// Create a durable ref pointing at `sha` under `base`, choosing a suffix that
/// does not already exist and creating it atomically (create-only), so a second
/// snapshot can never overwrite an earlier recovery copy (Greptile #2,
/// follow-up). Returns the ref name actually created.
pub fn create_recovery_ref(repo: &Path, base: &str, sha: &str) -> Result<String> {
    assert!(
        base.starts_with("refs/"),
        "recovery ref base must be qualified"
    );
    assert!(!sha.is_empty(), "recovery sha must be non-empty");
    // Bounded search for a free suffix (P10 Rule 2).
    const MAX_SUFFIX: u32 = 1000;
    for n in 0..MAX_SUFFIX {
        let refname = format!("{base}-{n}");
        // `update-ref <ref> <new> ""` requires the ref to NOT already exist;
        // it fails otherwise, so we never clobber an existing recovery copy.
        if proc::run_mutate(repo, &["update-ref", &refname, sha, ""]).is_ok() {
            return Ok(refname);
        }
    }
    Err(Error::Git("no free recovery ref suffix".into()))
}

/// Run a mutating git command with a scratch `GIT_INDEX_FILE` so the real index
/// is untouched. Not on the kill path (these writes must complete).
fn git_env(repo: &Path, args: &[&str], index_file: &str) -> Result<String> {
    use std::process::Command;
    let out = Command::new("git")
        .current_dir(repo)
        .env("GIT_INDEX_FILE", index_file)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn scratch_index_path() -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("wtm-index-{}-{ts}", std::process::id()))
}

/// Deletes the scratch index file on drop.
struct ScratchIndex(PathBuf);
impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Run a read-only git command with timeout, returning captured stdout.
pub fn capture_read(repo: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    proc::run_read(repo, args, timeout)
}
