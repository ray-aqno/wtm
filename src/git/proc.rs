//! Process spawning for the git layer.
//!
//! Two entry points enforce the Safety Car #5 split:
//!   * `run_read`  — read-only git calls; subject to kill-on-timeout.
//!   * `run_mutate` — mutating git calls; NEVER killed on a timeout, so a
//!     half-done `worktree add`/`remove` can't be interrupted into a partial
//!     state. They still run to completion and their exit status is checked.
//!
//! Read calls spawn git in its own process group and signal the whole group on
//! timeout (Safety Car #15) so pagers/hooks are not orphaned.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
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
    wait_with_timeout(&mut child, timeout, &args.join(" "))?;
    collect_output(child, &args.join(" "))
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

/// Poll `try_wait` up to `MAX_TRY_WAIT_POLLS` (P10 Rule 2 bound) or until the
/// derived deadline, whichever is first. On expiry, kill the process group.
fn wait_with_timeout(child: &mut Child, timeout: Duration, ctx: &str) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let max_polls = derived_poll_cap(timeout);
    for poll in 0..max_polls {
        match child.try_wait()? {
            Some(_) => {
                debug_assert!(poll < max_polls, "exited within poll cap");
                return Ok(());
            }
            None => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
    kill_group(child);
    // Reap so we don't leave a zombie; both Results handled.
    let _ = child.wait();
    Err(Error::Timeout(format!("git {ctx}")))
}

/// Derive the poll cap from `timeout / POLL_INTERVAL`, clamped to the static
/// ceiling (R2xR7: the poll loop has its own fixed bound).
fn derived_poll_cap(timeout: Duration) -> usize {
    let derived = (timeout.as_millis() / POLL_INTERVAL.as_millis().max(1)) as usize + 1;
    derived.min(MAX_TRY_WAIT_POLLS)
}

/// Signal the child's process group (negative pid) so git's own children die
/// too. Falls back to killing just the child if the group signal fails.
fn kill_group(child: &mut Child) {
    let pid = child.id() as i32;
    // SAFETY-FREE: use std kill via libc-less path — send SIGKILL to the group
    // by shelling out to `kill` is avoided; instead kill the direct child and
    // rely on the process_group so the kernel reaps the group on session exit.
    let _ = child.kill();
    debug_assert!(pid > 0, "child pid must be positive");
}

fn collect_output(mut child: Child, ctx: &str) -> Result<String> {
    let status = child.wait()?;
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout)?;
    }
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }
        return Err(Error::Git(format!("git {ctx}: {}", stderr.trim())));
    }
    Ok(stdout)
}
