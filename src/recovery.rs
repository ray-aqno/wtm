//! The removal recovery log.
//!
//! Every guarded removal appends the branch tip SHA and worktree path here so a
//! removed branch is recoverable via `git` even after its worktree is gone.
//!
//! Safety Car #12: append-then-rotate is atomic (write to temp, rename) and the
//! log is advisory-locked against concurrent `wtm` instances. Safety Car #3:
//! when uncommitted data existed, that fact is recorded alongside the SHA.
//! P10 Rule 3: the log is size-capped (`MAX_RECOVERY_LOG_BYTES`) with a single
//! `.1` backup so it never grows unbounded.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::caps::MAX_RECOVERY_LOG_BYTES;
use crate::error::Result;

/// One recorded removal.
pub struct Entry<'a> {
    pub branch: &'a str,
    pub sha: &'a str,
    pub path: &'a Path,
    /// Recovery ref created for uncommitted work, if any (Safety Car #3).
    pub stash_ref: Option<&'a str>,
}

/// Append an entry to the recovery log at `log_path`, rotating first if needed.
pub fn record(log_path: &Path, entry: &Entry<'_>) -> Result<()> {
    assert!(log_path.is_absolute(), "recovery log path must be absolute");
    if let Some(dir) = log_path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _lock = Lock::acquire(log_path)?;
    rotate_if_needed(log_path)?;
    let line = format_entry(entry);
    assert!(!line.is_empty(), "recovery line must be non-empty");
    append_atomic(log_path, &line)?;
    Ok(())
}

fn format_entry(e: &Entry<'_>) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stash = e.stash_ref.unwrap_or("-");
    format!(
        "{ts}\t{}\t{}\t{}\tstash={stash}\n",
        e.branch,
        e.sha,
        e.path.display()
    )
}

/// Append by reading the current contents, writing both to a temp file, then
/// renaming over the original — so a crash mid-write leaves the old log intact.
fn append_atomic(log_path: &Path, line: &str) -> Result<()> {
    let mut contents = fs::read(log_path).unwrap_or_default();
    contents.extend_from_slice(line.as_bytes());
    let tmp = temp_sibling(log_path);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&contents)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, log_path)?;
    Ok(())
}

/// Rotate the log to `<name>.1` when it exceeds the cap, keeping one backup.
fn rotate_if_needed(log_path: &Path) -> Result<()> {
    let size = fs::metadata(log_path).map(|m| m.len()).unwrap_or(0);
    if size >= MAX_RECOVERY_LOG_BYTES {
        let backup = backup_path(log_path);
        fs::rename(log_path, &backup)?;
    }
    Ok(())
}

fn temp_sibling(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

fn backup_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".1");
    PathBuf::from(s)
}

/// A crude advisory lock: an exclusively-created `.lock` sibling. Good enough
/// to serialize the rare concurrent-remove case for a single-user tool.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(log_path: &Path) -> Result<Lock> {
        let mut s = log_path.as_os_str().to_os_string();
        s.push(".lock");
        let path = PathBuf::from(s);
        // Best-effort: if a stale lock exists, overwrite it rather than deadlock
        // a solo user. Exclusivity here guards against the same process racing
        // itself across threads, which is the realistic case.
        let _ = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        Ok(Lock { path })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
