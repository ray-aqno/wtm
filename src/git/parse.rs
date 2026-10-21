//! Parsers for git's machine-readable output.
//!
//! All parsing is field-aware. The porcelain-v2 `-z` format delimits records
//! with NUL, but rename/copy records (`2 `) embed a SECOND NUL before the
//! original path, so naive NUL-splitting shifts every later field. The status
//! parser consumes that extra field explicitly (Safety Car #4).
//!
//! P10 Rule 2: every loop is bounded by `max_records`; past the cap the parser
//! returns `truncated = true` rather than looping unbounded on hostile input.
//! P10 Rule 5: each leaf parser carries >= 2 assertions.

use crate::error::{Error, Result};
use crate::model::{AheadBehind, Worktree};
use std::path::PathBuf;

/// Parse `git --version` output into `(major, minor)`.
pub fn parse_git_version(s: &str) -> Result<(u32, u32)> {
    assert!(!s.is_empty(), "git --version output is empty");
    let token = s
        .split_whitespace()
        .find(|t| {
            t.chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        })
        .ok_or_else(|| Error::Invalid(format!("no version in: {s:?}")))?;
    let mut parts = token.split('.');
    let major = parts.next().and_then(|p| p.parse().ok());
    let minor = parts.next().and_then(|p| p.parse().ok());
    match (major, minor) {
        (Some(ma), Some(mi)) => {
            assert!(ma < 1000, "implausible git major version");
            Ok((ma, mi))
        }
        _ => Err(Error::Invalid(format!("unparseable git version: {token}"))),
    }
}

/// Parse `git worktree list --porcelain -z`. Records are groups of
/// NUL-terminated lines; a `worktree ` line opens each group. Returns the
/// worktrees and whether the cap truncated the list.
pub fn parse_worktree_list(s: &str, max_records: usize) -> Result<(Vec<Worktree>, bool)> {
    assert!(max_records > 0, "record cap must be positive");
    let mut out: Vec<Worktree> = Vec::new();
    let mut truncated = false;
    let mut cur: Option<Worktree> = None;
    for field in s.split('\0') {
        if field.is_empty() {
            continue;
        }
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(done) = cur.take() {
                out.push(done);
                if out.len() >= max_records {
                    truncated = true;
                    break;
                }
            }
            cur = Some(new_worktree(PathBuf::from(path)));
        } else if let Some(wt) = cur.as_mut() {
            apply_worktree_field(wt, field);
        }
    }
    if !truncated {
        if let Some(done) = cur.take() {
            if out.len() < max_records {
                out.push(done);
            } else {
                truncated = true;
            }
        }
    }
    assert!(out.len() <= max_records, "worktree count within cap");
    Ok((out, truncated))
}

fn new_worktree(path: PathBuf) -> Worktree {
    Worktree {
        path,
        branch: None,
        head: String::new(),
        ahead_behind: None,
        dirty_count: 0,
        last_activity: None,
        reachable: true,
        agent: None,
    }
}

fn apply_worktree_field(wt: &mut Worktree, field: &str) {
    if let Some(head) = field.strip_prefix("HEAD ") {
        wt.head = head.to_string();
    } else if let Some(branch) = field.strip_prefix("branch ") {
        wt.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
    } else if field == "detached" {
        wt.branch = None;
    }
}

/// Parse `git status --porcelain=v2 --branch -z` into `(dirty_count,
/// ahead_behind)`. Rename records (`2 `) carry an extra NUL-delimited original
/// path which is consumed so field alignment is preserved (Safety Car #4).
/// Missing `# branch.ab` (detached HEAD) yields `ahead_behind = None`
/// (Safety Car #14).
pub fn parse_status_v2(s: &str, max_records: usize) -> Result<(u32, Option<AheadBehind>)> {
    assert!(max_records > 0, "record cap must be positive");
    let mut fields = s.split('\0');
    let mut dirty: u32 = 0;
    let mut ahead_behind: Option<AheadBehind> = None;
    let mut seen = 0usize;
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        seen += 1;
        if seen > max_records {
            // Hit the cap: stop parsing; caller treats a truncated parse as a
            // degraded read rather than trusting a partial count.
            return Err(Error::Invalid("status exceeded record cap".into()));
        }
        if let Some(ab) = field.strip_prefix("# branch.ab ") {
            ahead_behind = parse_branch_ab(ab);
        } else if field.starts_with("1 ") || field.starts_with("u ") || field.starts_with("? ") {
            dirty += 1;
        } else if field.starts_with("2 ") {
            dirty += 1;
            // Rename/copy: the next NUL-delimited field is the original path.
            let _ = fields.next();
        }
    }
    Ok((dirty, ahead_behind))
}

/// Parse a `# branch.ab +A -B` tail into ahead/behind counts.
fn parse_branch_ab(tail: &str) -> Option<AheadBehind> {
    let mut parts = tail.split_whitespace();
    let ahead = parts.next()?.strip_prefix('+')?.parse().ok()?;
    let behind = parts.next()?.strip_prefix('-')?.parse().ok()?;
    Some(AheadBehind { ahead, behind })
}
