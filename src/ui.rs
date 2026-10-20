//! Rendering: turn app state into a ratatui frame.
//!
//! Render reads a `&mut App` only to drive the stateful `TableState`, which
//! carries selection and scroll offset. It makes no other mutation, so the
//! headless `TestBackend` test can still render a fixture and assert the buffer.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::model::Worktree;
use crate::refresh::WatchMode;

/// Render the whole dashboard into `frame`. Takes `&mut App` so the table's
/// selection/scroll state (Greptile #10) is applied.
pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(frame.area());
    frame.render_widget(header_line(app), chunks[0]);
    let table = worktree_table(app);
    app.table_state.select(if app.worktrees.is_empty() {
        None
    } else {
        Some(app.selected.min(app.worktrees.len() - 1))
    });
    frame.render_stateful_widget(table, chunks[1], &mut app.table_state);
}

/// The one-line header: title, watch mode, and a truncation banner if the cap
/// tripped (Safety Car #10).
fn header_line(app: &App) -> Line<'static> {
    let mode = match app.watch_mode {
        WatchMode::Events => "live",
        WatchMode::PollOnly => "poll-only",
    };
    let mut text = format!("wtm — {} worktrees [{mode}]", app.worktrees.len());
    if app.truncated {
        text.push_str("  ⚠ list truncated — results incomplete");
    }
    Line::from(text)
}

/// The worktree table widget.
fn worktree_table(app: &App) -> Table<'static> {
    let header = Row::new(["branch", "status", "±", "activity", "agent"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app.worktrees.iter().map(worktree_row).collect();
    let widths = [
        Constraint::Percentage(32),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Percentage(26),
    ];
    Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("worktrees"))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
}

/// Format one worktree into a table row.
fn worktree_row(wt: &Worktree) -> Row<'static> {
    let branch = wt.branch.clone().unwrap_or_else(|| "(detached)".into());
    let status = if !wt.reachable {
        "degraded".to_string()
    } else if wt.is_clean() {
        "clean".to_string()
    } else {
        format!("dirty:{}", wt.dirty_count)
    };
    let ab = match &wt.ahead_behind {
        Some(ab) => format!("+{} -{}", ab.ahead, ab.behind),
        None => "n/a".to_string(),
    };
    let activity = match wt.last_activity {
        Some(ts) => format_age(ts),
        None => "—".to_string(),
    };
    let agent = match &wt.agent {
        Some(a) => format!("{} ({})", a.label, a.pid),
        None => "—".to_string(),
    };
    Row::new(vec![
        Cell::from(branch),
        Cell::from(status),
        Cell::from(ab),
        Cell::from(activity),
        Cell::from(agent),
    ])
}

/// A compact relative age ("3m", "2h", "5d") from a unix timestamp
/// (Greptile #14). Falls back to "—" for a future/invalid timestamp.
fn format_age(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let secs = now - ts;
    if secs < 0 {
        return "—".to_string();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}
