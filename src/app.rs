//! Application state and the persistent event loop.
//!
//! P10 Rule 2: the event loop in `run` is the SOLE documented unbounded loop in
//! the crate — it runs "until quit". Every other loop in the crate is capped.
//! Rule 6: all state lives in `App` and is threaded through; no globals.
//!
//! Refresh work (two git calls per worktree) runs on a background worker thread
//! (Greptile #11) so a slow git call never freezes input or rendering; the loop
//! requests a refresh and applies the snapshot when it arrives.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::TableState;
use ratatui::Terminal;

use crate::config::Config;
use crate::error::Result;
use crate::model::Worktree;
use crate::refresh::{Refresher, WatchMode};
use crate::{agent, git, ui};

/// A computed view of the repository's worktrees.
pub struct Snapshot {
    pub worktrees: Vec<Worktree>,
    pub truncated: bool,
}

/// Everything the dashboard renders.
pub struct App {
    pub worktrees: Vec<Worktree>,
    pub truncated: bool,
    pub watch_mode: WatchMode,
    pub selected: usize,
    pub table_state: TableState,
}

impl App {
    /// Build an app directly from a fixture model, for the headless render
    /// test. Does not touch git. Mirrors the fields `ui::render` reads.
    pub fn for_render(worktrees: Vec<Worktree>, truncated: bool, watch_mode: WatchMode) -> App {
        App {
            worktrees,
            truncated,
            watch_mode,
            selected: 0,
            table_state: TableState::default(),
        }
    }

    /// Replace the rendered state with a freshly computed snapshot.
    fn apply(&mut self, snap: Snapshot) {
        self.worktrees = snap.worktrees;
        self.truncated = snap.truncated;
        if !self.worktrees.is_empty() && self.selected >= self.worktrees.len() {
            self.selected = self.worktrees.len() - 1;
        }
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

/// Compute a snapshot: list worktrees + status, then overlay best-effort agent
/// detection. Pure w.r.t. app state so it can run on a worker thread.
fn load_state(repo: &Path, config: &Config) -> Result<Snapshot> {
    let timeout = Duration::from_millis(config.git_timeout_ms);
    let (mut worktrees, truncated) = git::list_worktrees(repo, timeout)?;
    let paths: Vec<PathBuf> = worktrees.iter().map(|w| w.path.clone()).collect();
    for (path, found) in agent::detect(&paths, &config.agent_patterns) {
        if let Some(wt) = worktrees.iter_mut().find(|w| w.path == path) {
            wt.agent = Some(found);
        }
    }
    Ok(Snapshot {
        worktrees,
        truncated,
    })
}

/// Background refresh worker: on each request it recomputes a snapshot and sends
/// it back. Lives for the duration of the dashboard; exits when the request
/// channel disconnects (the app quit) — a clean shutdown, not an error.
struct Loader {
    request: Sender<()>,
    result: Receiver<Snapshot>,
    _handle: std::thread::JoinHandle<()>,
}

impl Loader {
    fn spawn(repo: PathBuf, config: Config) -> Loader {
        let (req_tx, req_rx) = mpsc::channel::<()>();
        let (res_tx, res_rx) = mpsc::channel::<Snapshot>();
        let handle = std::thread::spawn(move || {
            // Bounded only by the lifetime of the request channel: each recv
            // blocks until a request or disconnect. Not a busy loop.
            while req_rx.recv().is_ok() {
                if let Ok(snap) = load_state(&repo, &config) {
                    // Receiver gone => app quit; stop.
                    if res_tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        });
        Loader {
            request: req_tx,
            result: res_rx,
            _handle: handle,
        }
    }

    /// Ask for a refresh; ignore a full/broken channel (a refresh is already
    /// in flight or the worker is gone).
    fn request(&self) {
        let _ = self.request.send(());
    }

    /// Take the most recent completed snapshot, if any (drains the channel).
    fn take_latest(&self) -> Option<Snapshot> {
        let mut latest = None;
        while let Ok(snap) = self.result.try_recv() {
            latest = Some(snap);
        }
        latest
    }
}

/// Run the persistent dashboard until the user quits.
pub fn run(repo: &Path, config: Config) -> Result<()> {
    let interval = Duration::from_millis(config.poll_interval_ms);
    let refresher = Refresher::new(repo, &config.worktrees_root, interval);
    let watch_mode = refresher.mode;

    let loader = Loader::spawn(repo.to_path_buf(), config.clone());
    // Kick off the initial load and wait briefly for the first snapshot so the
    // first frame is populated.
    loader.request();
    let initial = wait_initial(&loader, Duration::from_millis(config.git_timeout_ms));

    let mut app = App::for_render(
        initial
            .as_ref()
            .map(|s| s.worktrees.clone())
            .unwrap_or_default(),
        initial.as_ref().map(|s| s.truncated).unwrap_or(false),
        watch_mode,
    );

    let mut terminal = setup_terminal()?;
    let outcome = event_loop(&mut terminal, &mut app, refresher, &loader);
    // Restore runs via the guard's Drop even if the loop errored.
    drop(terminal);
    outcome
}

/// Block up to `timeout` for the first snapshot so startup isn't blank.
fn wait_initial(loader: &Loader, timeout: Duration) -> Option<Snapshot> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(snap) = loader.take_latest() {
            return Some(snap);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// The sole unbounded loop (P10 Rule 2 exception): poll input, apply any ready
/// snapshot, request a refresh on a tick or filesystem event, render, repeat
/// until Quit. No git call runs on this thread.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<TerminalGuard>>,
    app: &mut App,
    mut refresher: Refresher,
    loader: &Loader,
) -> Result<()> {
    loop {
        if let Some(snap) = loader.take_latest() {
            app.apply(snap);
        }
        terminal.draw(|f| ui::render(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                        KeyCode::Char('r') => loader.request(),
                        _ => {}
                    }
                }
            }
        }
        if refresher.should_refresh(Instant::now()) {
            loader.request();
        }
    }
}

/// RAII guard over the terminal's raw mode + alternate screen. Entering happens
/// in `new`; leaving happens in `Drop` so a failure partway through setup or a
/// panic in the loop still restores the terminal (Greptile #18).
struct TerminalGuard {
    stdout: io::Stdout,
}

impl TerminalGuard {
    fn new() -> Result<TerminalGuard> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen) {
            // Undo raw mode before surfacing the error.
            let _ = crossterm::terminal::disable_raw_mode();
            return Err(e.into());
        }
        Ok(TerminalGuard { stdout })
    }
}

impl io::Write for TerminalGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdout.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(self.stdout, crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<TerminalGuard>>> {
    let guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(guard);
    Ok(Terminal::new(backend)?)
}
