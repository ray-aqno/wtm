//! Application state and the persistent event loop.
//!
//! P10 Rule 2: the event loop in `run` is the SOLE documented unbounded loop in
//! the crate — it runs "until quit". Every other loop in the crate is capped.
//! Rule 6: all state lives in `App` and is threaded through; no globals.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::Config;
use crate::error::Result;
use crate::model::Worktree;
use crate::refresh::{Refresher, WatchMode};
use crate::{agent, git, ui};

/// Everything the dashboard renders.
pub struct App {
    pub worktrees: Vec<Worktree>,
    pub truncated: bool,
    pub watch_mode: WatchMode,
    pub selected: usize,
    repo: PathBuf,
    config: Config,
}

impl App {
    /// Build an app for a repository, loading the initial worktree list.
    pub fn new(repo: PathBuf, config: Config) -> Result<App> {
        let mut app = App {
            worktrees: Vec::new(),
            truncated: false,
            watch_mode: WatchMode::Events,
            selected: 0,
            repo,
            config,
        };
        app.reload()?;
        Ok(app)
    }

    /// Build an app directly from a fixture model, for the headless render
    /// test. Does not touch git. Mirrors the fields `ui::render` reads.
    pub fn for_render(
        worktrees: Vec<Worktree>,
        truncated: bool,
        watch_mode: WatchMode,
        config: Config,
    ) -> App {
        App {
            worktrees,
            truncated,
            watch_mode,
            selected: 0,
            repo: PathBuf::from("/repo"),
            config,
        }
    }

    /// Re-read worktrees + status, then overlay best-effort agent detection.
    pub fn reload(&mut self) -> Result<()> {
        let timeout = Duration::from_millis(self.config.git_timeout_ms);
        let (mut worktrees, truncated) = git::list_worktrees(&self.repo, timeout)?;
        let paths: Vec<PathBuf> = worktrees.iter().map(|w| w.path.clone()).collect();
        for (path, found) in agent::detect(&paths, &self.config.agent_patterns) {
            if let Some(wt) = worktrees.iter_mut().find(|w| w.path == path) {
                wt.agent = Some(found);
            }
        }
        self.worktrees = worktrees;
        self.truncated = truncated;
        if self.selected >= self.worktrees.len() {
            self.selected = self.worktrees.len().saturating_sub(1);
        }
        Ok(())
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.worktrees.len() {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Run the persistent dashboard until the user quits.
pub fn run(repo: &Path, config: Config) -> Result<()> {
    let mut app = App::new(repo.to_path_buf(), config)?;
    let interval = Duration::from_millis(app.config.poll_interval_ms);
    let mut refresher = Refresher::new(&app.config.worktrees_root, interval);
    app.watch_mode = refresher.mode;

    let mut terminal = setup_terminal()?;
    let outcome = event_loop(&mut terminal, &mut app, &mut refresher);
    restore_terminal(&mut terminal)?;
    outcome
}

/// The sole unbounded loop (P10 Rule 2 exception): poll input, refresh on a
/// tick or filesystem event, render, repeat until Quit.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    refresher: &mut Refresher,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                        KeyCode::Char('r') => app.reload()?,
                        _ => {}
                    }
                }
            }
        }
        if refresher.should_refresh(Instant::now()) {
            // A failed refresh degrades the view but never crashes the loop.
            let _ = app.reload();
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}
