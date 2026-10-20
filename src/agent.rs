//! Best-effort agent detection via `/proc`.
//!
//! Scans `/proc/<pid>/cwd` and `/proc/<pid>/cmdline`, maps each live process to
//! a worktree by path-component boundary (Safety Car #8 — never a raw string
//! prefix, so `/wt/foo` does not capture `/wt/foobar`), and classifies the
//! process via config patterns. Liveness is validated at scan time; a process
//! that has exited is simply not reported, so the UI shows "unknown", never a
//! stale "attached".
//!
//! P10 Rule 2: the scan is bounded by `MAX_PROC_ENTRIES`. Rule 7: unreadable
//! `/proc` entries (permission, race with exit) are expected and skipped, not
//! fatal. Non-Linux: `/proc` is absent, the scan yields nothing, and the UI
//! column shows "—".

use std::fs;
use std::path::{Path, PathBuf};

use crate::caps::MAX_PROC_ENTRIES;
use crate::model::Agent;

/// A live process that resolved to a worktree.
pub struct Detected {
    pub pid: u32,
    pub cwd: PathBuf,
    pub label: String,
}

/// Scan `/proc` for processes whose cwd is inside one of `worktrees`, returning
/// at most one agent per worktree (the first match). `patterns` is
/// `(needle, label)` pairs.
pub fn detect(worktrees: &[PathBuf], patterns: &[(String, String)]) -> Vec<(PathBuf, Agent)> {
    let proc = Path::new("/proc");
    if !proc.exists() {
        return Vec::new();
    }
    let mut out: Vec<(PathBuf, Agent)> = Vec::new();
    let entries = match fs::read_dir(proc) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for (scanned, entry) in entries.enumerate() {
        if scanned >= MAX_PROC_ENTRIES {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Some(pid) = pid_of(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let Some(d) = inspect(pid, patterns) else {
            continue;
        };
        if let Some(wt) = match_worktree(&d.cwd, worktrees) {
            if !out.iter().any(|(p, _)| p == &wt) {
                out.push((
                    wt,
                    Agent {
                        pid: d.pid,
                        label: d.label,
                    },
                ));
            }
        }
    }
    debug_assert!(
        out.len() <= worktrees.len(),
        "at most one agent per worktree"
    );
    out
}

/// Parse a `/proc` directory name as a pid.
fn pid_of(name: &str) -> Option<u32> {
    let pid: u32 = name.parse().ok()?;
    assert!(pid > 0, "pid must be positive");
    Some(pid)
}

/// Read a live process's cwd and classify it. `None` if the process is gone or
/// its cwd is unreadable (both expected and non-fatal).
fn inspect(pid: u32, patterns: &[(String, String)]) -> Option<Detected> {
    let base = format!("/proc/{pid}");
    let cwd = fs::read_link(format!("{base}/cwd")).ok()?;
    assert!(cwd.is_absolute(), "/proc cwd link must be absolute");
    let cmdline = fs::read(format!("{base}/cmdline")).unwrap_or_default();
    let label = classify(&cmdline, patterns);
    Some(Detected { pid, cwd, label })
}

/// Classify a NUL-delimited cmdline against the configured patterns.
fn classify(cmdline: &[u8], patterns: &[(String, String)]) -> String {
    let text = String::from_utf8_lossy(cmdline).replace('\0', " ");
    for (needle, label) in patterns {
        if text.contains(needle.as_str()) {
            return label.clone();
        }
    }
    "agent".to_string()
}

/// Return the worktree that contains `cwd`, matching on a path-component
/// boundary (Safety Car #8).
fn match_worktree(cwd: &Path, worktrees: &[PathBuf]) -> Option<PathBuf> {
    worktrees
        .iter()
        .find(|wt| cwd == wt.as_path() || cwd.starts_with(wt))
        .cloned()
}
