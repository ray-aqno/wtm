//! Rendering: turn app state into a ratatui frame.
//!
//! Kept pure w.r.t. the terminal so the headless `TestBackend` test can render
//! a fixture model and assert on the buffer (P10 Rule 6 — no hidden state).

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::model::Worktree;
use crate::refresh::WatchMode;

/// Render the whole dashboard into `frame`.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(frame.area());
    frame.render_widget(header_line(app), chunks[0]);
    frame.render_widget(worktree_table(app), chunks[1]);
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
    let header = Row::new(["branch", "status", "±", "agent"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app.worktrees.iter().map(worktree_row).collect();
    let widths = [
        Constraint::Percentage(40),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Percentage(30),
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
    let agent = match &wt.agent {
        Some(a) => format!("{} ({})", a.label, a.pid),
        None => "—".to_string(),
    };
    Row::new(vec![
        Cell::from(branch),
        Cell::from(status),
        Cell::from(ab),
        Cell::from(agent),
    ])
}
