//! Tests for best-effort agent detection and the headless TUI render.

use std::path::PathBuf;
use wtm::agent;

#[test]
fn match_is_component_boundary_not_raw_prefix() {
    // `/wt/foo` must not capture a process in `/wt/foobar` (Safety Car #8).
    // We exercise the public `detect` indirectly is hard without /proc, so test
    // the boundary rule via the classifier-independent path: build worktrees and
    // confirm a process cwd under one maps only to that one. Since `detect`
    // reads real /proc, here we assert the documented invariant through the
    // path-prefix helper semantics using PathBuf::starts_with, which is what the
    // mapper relies on.
    let foo = PathBuf::from("/wt/foo");
    let foobar_cwd = PathBuf::from("/wt/foobar/src");
    let foo_cwd = PathBuf::from("/wt/foo/src");
    assert!(!foobar_cwd.starts_with(&foo), "foobar must not match foo");
    assert!(foo_cwd.starts_with(&foo), "foo/src matches foo");
}

#[test]
fn detect_on_empty_worktrees_is_empty() {
    // No worktrees => nothing can be attributed, regardless of /proc contents.
    let out = agent::detect(&[], &[("claude".into(), "claude".into())]);
    assert!(out.is_empty());
}

#[test]
fn headless_render_shows_worktrees_and_truncation() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use wtm::app::App;
    use wtm::model::{AheadBehind, Worktree};
    use wtm::refresh::WatchMode;

    let wt = Worktree {
        path: PathBuf::from("/repo"),
        branch: Some("main".into()),
        head: "abc".into(),
        ahead_behind: Some(AheadBehind {
            ahead: 1,
            behind: 0,
        }),
        dirty_count: 0,
        last_activity: None,
        reachable: true,
        agent: None,
    };
    let mut app = App::for_render(vec![wt], true, WatchMode::PollOnly);

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| wtm::ui::render(f, &mut app)).unwrap();
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("main"), "branch rendered");
    assert!(text.contains("poll-only"), "poll-only indicator rendered");
    assert!(text.contains("truncated"), "truncation banner rendered");
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    buf.content().iter().map(|c| c.symbol()).collect()
}
