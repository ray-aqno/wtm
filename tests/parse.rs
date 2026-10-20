//! Unit tests for the pure parsers and mappers.

use std::path::PathBuf;
use wtm::git::{parse_git_version, parse_status_v2, parse_worktree_list};

#[test]
fn git_version_parses() {
    assert_eq!(parse_git_version("git version 2.53.0").unwrap(), (2, 53));
    assert_eq!(
        parse_git_version("git version 2.36.1.windows.2").unwrap(),
        (2, 36)
    );
    assert!(parse_git_version("git version unknown").is_err());
}

#[test]
fn worktree_list_parses_branch_and_detached() {
    // Two records: one on a branch, one detached. NUL-delimited fields.
    let input = "worktree /repo\0HEAD abc123\0branch refs/heads/main\0\0\
                 worktree /repo-worktrees/feat\0HEAD def456\0detached\0\0";
    let (wts, truncated) = parse_worktree_list(input, 1024).unwrap();
    assert!(!truncated);
    assert_eq!(wts.len(), 2);
    assert_eq!(wts[0].branch.as_deref(), Some("main"));
    assert_eq!(wts[0].head, "abc123");
    assert_eq!(wts[1].branch, None);
}

#[test]
fn worktree_list_respects_cap() {
    let mut input = String::new();
    for i in 0..10 {
        input.push_str(&format!(
            "worktree /wt/{i}\0HEAD h{i}\0branch refs/heads/b{i}\0\0"
        ));
    }
    let (wts, truncated) = parse_worktree_list(&input, 3).unwrap();
    assert!(truncated, "cap should truncate");
    assert_eq!(wts.len(), 3);
}

#[test]
fn status_v2_counts_dirty_and_ahead_behind() {
    // branch.ab header + two changed entries (one modified `1 `, one untracked `? `).
    let input = "# branch.oid abc\0# branch.head main\0# branch.ab +2 -1\0\
                 1 .M N... 100644 100644 100644 aa bb file1.rs\0\
                 ? untracked.rs\0";
    let (dirty, ab) = parse_status_v2(input, 100_000).unwrap();
    assert_eq!(dirty, 2);
    let ab = ab.expect("ahead/behind present");
    assert_eq!((ab.ahead, ab.behind), (2, 1));
}

#[test]
fn status_v2_handles_rename_embedded_nul() {
    // A rename record `2 ` carries an extra NUL field (the original path). The
    // parser must consume it so the trailing untracked file is still counted
    // (Safety Car #4). Without rename-awareness, the orig path would be
    // miscounted as its own record.
    let input = "# branch.ab +0 -0\0\
                 2 R. N... 100644 100644 100644 aa bb R100 new_name.rs\0old_name.rs\0\
                 ? extra.rs\0";
    let (dirty, _ab) = parse_status_v2(input, 100_000).unwrap();
    assert_eq!(
        dirty, 2,
        "rename (1) + untracked (1), orig path not counted"
    );
}

#[test]
fn status_v2_detached_has_no_ahead_behind() {
    let input = "# branch.oid abc\0# branch.head (detached)\0";
    let (dirty, ab) = parse_status_v2(input, 100_000).unwrap();
    assert_eq!(dirty, 0);
    assert!(ab.is_none(), "detached HEAD yields no ahead/behind");
}

#[test]
fn status_v2_rejects_over_cap() {
    let mut input = String::new();
    for i in 0..50 {
        input.push_str(&format!("1 .M N... 100644 100644 100644 aa bb f{i}.rs\0"));
    }
    assert!(
        parse_status_v2(&input, 10).is_err(),
        "over-cap parse is an error"
    );
}

#[test]
fn session_name_is_tmux_legal_and_collision_free() {
    let a = wtm::switch::session_name(&PathBuf::from("/wt/feature.v1.2"));
    let b = wtm::switch::session_name(&PathBuf::from("/wt/feature_v1_2"));
    assert!(!a.contains('.'), "no dots in tmux session name");
    assert!(!a.contains(':'), "no colons in tmux session name");
    assert_ne!(a, b, "distinct paths must not collide after sanitization");
}
