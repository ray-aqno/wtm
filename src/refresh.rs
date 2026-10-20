//! Refresh orchestration: a filesystem watcher with a debounced backstop poll.
//!
//! Safety Car #7: if the watcher fails to initialize (e.g. inotify is dead on a
//! `/mnt` drvfs/9p mount, or `max_user_watches` is exhausted), the refresher
//! falls back to poll-only and reports it via `watch_mode`, so the UI can show
//! a visible "poll-only" indicator instead of silently going stale.
//!
//! P10 Rule 2: this module owns no unbounded loop of its own; the periodic tick
//! is driven by the app's event loop (the sole until-quit loop).

use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// Whether live filesystem events are available, or we are polling only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// inotify events are live; the poll is only a backstop.
    Events,
    /// Watcher unavailable; refresh is poll-only (shown in the UI).
    PollOnly,
}

/// Owns the watcher (if any) and tracks when the next backstop poll is due.
pub struct Refresher {
    _watcher: Option<RecommendedWatcher>,
    events: Option<Receiver<()>>,
    pub mode: WatchMode,
    interval: Duration,
    next_poll: Instant,
}

impl Refresher {
    /// Build a refresher watching `root`. Never fails: a watcher error degrades
    /// to poll-only.
    pub fn new(root: &Path, interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if res.is_ok() {
                    // A disconnected receiver means the app quit; ignore.
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        );
        let watched = watcher.and_then(|mut w| {
            w.watch(root, RecursiveMode::NonRecursive)?;
            Ok(w)
        });
        match watched {
            Ok(w) => Refresher {
                _watcher: Some(w),
                events: Some(rx),
                mode: WatchMode::Events,
                interval,
                next_poll: Instant::now() + interval,
            },
            Err(_) => Refresher {
                _watcher: None,
                events: None,
                mode: WatchMode::PollOnly,
                interval,
                next_poll: Instant::now() + interval,
            },
        }
    }

    /// True when a refresh is due: either a filesystem event arrived (debounced
    /// by draining the channel) or the backstop interval elapsed.
    pub fn should_refresh(&mut self, now: Instant) -> bool {
        let mut event = false;
        if let Some(rx) = &self.events {
            // Drain all pending events in one go (debounce).
            while rx.try_recv().is_ok() {
                event = true;
            }
        }
        if now >= self.next_poll {
            self.next_poll = now + self.interval;
            return true;
        }
        if event {
            self.next_poll = now + self.interval;
        }
        event
    }
}
