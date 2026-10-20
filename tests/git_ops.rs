//! Integration tests that exercise the git layer against a real temp repo.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use wtm::git;

/// Create an initialized git repo with one commit in a fresh temp dir.
fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    run(root, &["init", "-q", "-b", "main"]);
    run(root, &["config", "user.name", "t"]);
    run(root, &["config", "user.email", "t@t"]);
    std::fs::write(root.join("README.md"), "hi").unwrap();
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "init"]);
    dir
}

fn run(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fresh worktree-root TempDir, returned so the caller keeps it alive for the
/// test's duration. Being its own temp dir, it is auto-cleaned and never
/// collides with another run under /tmp.
fn wt_root_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn rejects_flaglike_and_traversal_branches() {
    assert!(git::validate_branch_name("--force").is_err());
    assert!(git::validate_branch_name("-x").is_err());
    assert!(git::validate_branch_name("a/../b").is_err());
    assert!(git::validate_branch_name("a/b").is_err());
    assert!(git::validate_branch_name("ok-branch").is_ok());
}

#[test]
fn create_makes_worktree_and_branch() {
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    let wtr = wt_root_dir();
    let wt_root = wtr.path().canonicalize().unwrap();
    let path = git::add_worktree(&root, &wt_root, "feature-x").unwrap();
    assert!(path.exists(), "worktree dir created");
    assert!(path.join(".git").exists(), "worktree is a git checkout");

    // The new branch is listed.
    let (wts, _) = git::list_worktrees(&root, Duration::from_secs(10)).unwrap();
    assert!(
        wts.iter().any(|w| w.branch.as_deref() == Some("feature-x")),
        "feature-x worktree present in list"
    );
}

#[test]
fn list_reports_clean_and_dirty() {
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    let (wts, _) = git::list_worktrees(&root, Duration::from_secs(10)).unwrap();
    assert_eq!(wts.len(), 1);
    assert!(wts[0].is_clean(), "fresh repo is clean");

    std::fs::write(root.join("dirty.txt"), "x").unwrap();
    let (wts, _) = git::list_worktrees(&root, Duration::from_secs(10)).unwrap();
    assert!(!wts[0].is_clean(), "untracked file makes it dirty");
}

#[test]
fn guarded_remove_refuses_dirty_without_force() {
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    let wtr = wt_root_dir();
    let wt_root = wtr.path().canonicalize().unwrap();
    let path = git::add_worktree(&root, &wt_root, "to-remove").unwrap();
    std::fs::write(path.join("uncommitted.txt"), "data").unwrap();

    let (wts, _) = git::list_worktrees(&root, Duration::from_secs(10)).unwrap();
    let wt = wts.into_iter().find(|w| w.path == path).unwrap();
    let log = root.join("removed.log");

    // Dirty + no force => refused.
    let refused =
        wtm::switch::guarded_remove(&root, &wt, &log, false, false, Duration::from_secs(10));
    assert!(refused.is_err(), "dirty remove refused without --force");
    assert!(path.exists(), "worktree still present after refusal");
}

#[test]
fn failed_create_preserves_existing_branch() {
    // Greptile #1: if the branch already exists, a failed `add` must NOT delete
    // it. Create a branch with a real commit, then try to add a worktree with
    // `-b` for that same name (which errors because the branch exists).
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    run(&root, &["branch", "keep-me"]);
    let wtr = wt_root_dir();
    let wt_root = wtr.path().canonicalize().unwrap();

    let err = git::add_worktree(&root, &wt_root, "keep-me");
    assert!(err.is_err(), "add -b on an existing branch fails");

    let still = Command::new("git")
        .current_dir(&root)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/keep-me"])
        .status()
        .unwrap();
    assert!(
        still.success(),
        "pre-existing branch must survive a failed add"
    );
}

#[test]
fn create_rejects_existing_destination() {
    // Greptile #17: an occupied destination is a user error, not a panic.
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    let wtr = wt_root_dir();
    let wt_root = wtr.path().canonicalize().unwrap();
    std::fs::create_dir_all(wt_root.join("already")).unwrap();
    let err = git::add_worktree(&root, &wt_root, "already");
    assert!(err.is_err(), "occupied destination returns Err, not panic");
}

#[test]
fn find_by_branch_matches_branch_not_dirname() {
    // Greptile #9: resolve by branch identity. Create a worktree, rename its
    // branch; lookup by the OLD name must fail, by the new must succeed.
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    let wtr = wt_root_dir();
    let wt_root = wtr.path().canonicalize().unwrap();
    let path = git::add_worktree(&root, &wt_root, "orig").unwrap();
    run(&path, &["branch", "-m", "orig", "renamed"]);

    assert!(
        git::find_by_branch(&root, "orig", Duration::from_secs(10)).is_err(),
        "old branch name no longer resolves"
    );
    let found = git::find_by_branch(&root, "renamed", Duration::from_secs(10)).unwrap();
    assert_eq!(found.path, path);
}

#[test]
fn guarded_remove_snapshots_untracked_and_removes() {
    // Greptile #2, #3: untracked-only dirty worktree is snapshotted to a durable
    // ref and then removed under --force.
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    let wtr = wt_root_dir();
    let wt_root = wtr.path().canonicalize().unwrap();
    let path = git::add_worktree(&root, &wt_root, "snap").unwrap();
    std::fs::write(path.join("only_untracked.txt"), "precious").unwrap();

    let wt = git::find_by_branch(&root, "snap", Duration::from_secs(10)).unwrap();
    let log = root.join("removed.log");
    let res = wtm::switch::guarded_remove(&root, &wt, &log, true, false, Duration::from_secs(10));
    assert!(res.is_ok(), "forced remove with snapshot succeeds: {res:?}");
    assert!(!path.exists(), "worktree removed");

    let refs = Command::new("git")
        .current_dir(&root)
        .args(["for-each-ref", "--format=%(refname)", "refs/wtm-recovery/"])
        .output()
        .unwrap();
    let refs = String::from_utf8_lossy(&refs.stdout);
    assert!(
        refs.contains("refs/wtm-recovery/"),
        "recovery ref created: {refs:?}"
    );
    assert!(log.exists(), "recovery log written");
}

#[test]
fn large_status_output_does_not_false_timeout() {
    // Greptile #5: a status large enough to fill a pipe buffer must still be
    // read (drained concurrently), not killed as a bogus timeout.
    let repo = init_repo();
    let root = repo.path().canonicalize().unwrap();
    for i in 0..4000 {
        std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
    }
    let (wts, _) = git::list_worktrees(&root, Duration::from_secs(20)).unwrap();
    assert!(
        wts[0].reachable,
        "large status read succeeded, not degraded"
    );
    assert!(wts[0].dirty_count >= 4000, "all untracked files counted");
}
