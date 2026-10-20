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
    let wt_root = root.parent().unwrap().join("wt-root");
    let path = git::add_worktree(&root, &wt_root, "feature-x").unwrap();
    assert!(path.exists(), "worktree dir created");
    assert!(path.join(".git").exists(), "worktree is a git checkout");

    // The new branch is listed.
    let (wts, _) = git::list_worktrees(&root, Duration::from_secs(10)).unwrap();
    assert!(
        wts.iter().any(|w| w.branch.as_deref() == Some("feature-x")),
        "feature-x worktree present in list"
    );
    let _ = std::fs::remove_dir_all(&wt_root);
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
    let wt_root = root.parent().unwrap().join("wt-rm-root");
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
    let _ = std::fs::remove_dir_all(&wt_root);
}
