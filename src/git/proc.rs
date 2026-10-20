//! Process spawning for the git layer.
//!
//! Two entry points enforce the Safety Car #5 split:
//!   * `run_read`  — read-only git calls; subject to kill-on-timeout.
//!   * `run_mutate` — mutating git calls; NEVER killed on a timeout, so a
//!     half-done `worktree add`/`remove` can't be interrupted into a partial
//!     state. They still run to completion and their exit status is checked.
//!
//! Read calls spawn git in its own process group and signal the whole group on
//! timeout (Greptile #12) so pagers/hooks are not orphaned. Both output pipes
//! are drained by dedicated threads WHILE waiting (Greptile #5) so a large
//! status cannot fill a pipe, block git, and masquerade as a timeout. Output is
//! captured as raw bytes and decoded lossily (Greptile #19) so a non-UTF-8
//! filename never fails the read.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::caps::MAX_TRY_WAIT_POLLS;
use crate::error::{Error, Result};

/// Poll interval while waiting for a timed read child to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Run a read-only git command, capturing stdout, killing it (and its process
/// group) if it exceeds `timeout`.
pub fn run_read(cwd: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut cmd = base_command(cwd, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Own process group so we can signal children git itself spawned.
    cmd.process_group(0);
    let mut child = cmd.spawn()?;

    // Drain both pipes concurrently so git never blocks on a full pipe buffer.
    let out_reader = spawn_drain(child.stdout.take());
    let err_reader = spawn_drain(child.stderr.take());

    let timed_out = wait_until(&mut child, timeout);
    if timed_out {
        kill_group(&mut child);
    }
    // Reap, then join the readers (the pipes close once git exits/dies).
    let status = child.wait()?;
    let stdout = join_drain(out_reader);
    let stderr = join_drain(err_reader);

    let ctx = args.join(" ");
    if timed_out {
        return Err(Error::Timeout(format!("git {ctx}")));
    }
    if !status.success() {
        return Err(Error::Git(format!(
            "git {ctx}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Run a mutating git command to completion (no kill path). Checks exit status.
pub fn run_mutate(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = base_command(cwd, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(Error::Git(format!("git {}: {stderr}", args.join(" "))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn base_command(cwd: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.current_dir(cwd).args(args);
    c
}

/// Spawn a thread that reads a pipe to EOF. `None` pipe yields empty bytes.
fn spawn_drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<JoinHandle<Vec<u8>>> {
    pipe.map(|mut r| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    })
}

fn join_drain(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

/// Poll `try_wait` up to `MAX_TRY_WAIT_POLLS` (P10 Rule 2 bound) or until the
/// derived deadline. Returns true if the deadline/cap was hit before exit.
fn wait_until(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let max_polls = derived_poll_cap(timeout);
    for poll in 0..max_polls {
        match child.try_wait() {
            Ok(Some(_)) => {
                debug_assert!(poll < max_polls, "exited within poll cap");
                return false;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    return true;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => return false,
        }
    }
    true
}

/// Derive the poll cap from `timeout / POLL_INTERVAL`, clamped to the static
/// ceiling (R2xR7: the poll loop has its own fixed bound).
fn derived_poll_cap(timeout: Duration) -> usize {
    let derived = (timeout.as_millis() / POLL_INTERVAL.as_millis().max(1)) as usize + 1;
    derived.min(MAX_TRY_WAIT_POLLS)
}

/// Signal the child's whole process group (negative pid) so git's own children
/// (pagers, fsmonitor hooks) die too, then fall back to the direct child.
fn kill_group(child: &mut Child) {
    let pid = child.id() as i32;
    debug_assert!(pid > 0, "child pid must be positive");
    // The child leads its own group (process_group(0)); the group id equals the
    // child pid. SIGKILL to -pid signals every member.
    // SAFETY: `libc::kill` is a thin syscall wrapper with no memory effects.
    let signalled = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if signalled != 0 {
        let _ = child.kill();
    }
}
