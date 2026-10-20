# RFC 0001: Persistent TUI for Git Worktree Management in Tandem with AI Agents

- Status: draft
- Date: 2026-10-20
- Run: 20261020-221140-5b47a0

## Summary

A persistent terminal UI (TUI), written in Rust with `ratatui` and shipped as a
single binary, that lets a solo developer run several AI coding agents in
parallel without hand-managing git worktrees. The TUI is a long-running
dashboard: it creates, lists, switches to, and removes worktrees, shows each
worktree's git status (branch, ahead/behind, dirty/clean, last activity), and —
on a best-effort basis — shows which agent process (if any) is working inside
each worktree. The one metric that defines success is that spinning up a fresh,
isolated worktree for an agent drops from a multi-command git ritual to a single
keystroke that completes in seconds.

## Motivation

Running N AI coding agents concurrently means N isolated branches, which in git
means N worktrees. Managing them by hand is `git worktree add`, naming the
branch, `cd`-ing into the right directory, repeatedly running `git status` and
`git worktree list`, and remembering which agent is in which directory doing
what. The plumbing is tedious and error-prone, and the "which agent is where,
doing what" state lives only in the developer's head — so it gets lost the
moment attention moves elsewhere.

The affected party is a single developer working on one machine (WSL/Linux),
primarily with Claude Code (possibly under the Hermes harness). If nothing
changes, parallel-agent workflows stay friction-heavy: each new agent costs a
half-dozen manual steps, and the developer periodically loses track of running
agents, leading to duplicated work, abandoned worktrees, and accidental edits in
the wrong tree.

## Design

### Shape

A single statically-linked Rust binary (`wtm`, working name) built on
`ratatui` + `crossterm`. It runs as a persistent full-screen TUI but also
exposes a few non-interactive subcommands (notably `create`) so the core
"spin up in seconds" path works from a shell keybinding without entering the UI.

### Core model

- **Repo context.** On launch the binary resolves the enclosing git repository
  (`git rev-parse --show-toplevel`). All worktrees of that repo are the unit of
  management.
- **Worktree record.** For each worktree: path, branch (or detached HEAD), HEAD
  SHA, upstream ahead/behind counts, dirty/clean + file-change counts, last
  commit time, a reachability flag (is the path on a live disk?), and a
  best-effort `agent` field (see Agent detection).

### Git integration — shell out, porcelain v2

All git reads go through the `git` CLI with machine-readable output, not a
libgit2 binding. Rationale is in Alternatives; the council noted `gitui`
repeatedly hits libgit2 worktree gaps while `gh-dash` shells out cleanly.

- Enumerate: `git worktree list --porcelain -z`
- Per-worktree status: `git -C <path> status --porcelain=v2 --branch -z`
  (gives branch, upstream, and dirty state in one call)
- Ahead/behind: taken from the `# branch.ab +X -Y` header of the same
  porcelain-v2 status — **computed from local refs only; the TUI never runs
  `git fetch`.** Counts are relative to the last-known remote ref and labeled as
  such.
- Last activity: `git -C <path> log -1 --format=%ct` (committer timestamp).

Every git invocation is wrapped in a timeout. A worktree whose path is
unreachable (removed disk, deleted externally) or whose git call times out is
marked **degraded** and rendered distinctly; it is never auto-removed.

### Operations

- **Create** (the hot path). Prompt for / accept a branch name; run
  `git worktree add <path> -b <branch>` (or attach an existing branch). Worktree
  paths are derived by a configurable template (default:
  `../<repo>-worktrees/<branch>`). Exposed both as an interactive action and as
  `wtm create <branch>` so it can be bound to a single shell key. On success the
  new worktree appears in the dashboard immediately (no wait for the next poll).
- **List.** The persistent dashboard: one row per worktree with branch, status,
  ahead/behind, last activity, and agent.
- **Switch.** The TUI process does **not** `cd` itself (a child process cannot
  change the parent shell's directory, and the TUI is long-running). Instead,
  switch creates or attaches a per-worktree `tmux` session rooted at the
  worktree path (the `sesh` pattern). When `tmux` is absent, switch falls back
  to printing the worktree path for shell-side `cd "$(wtm path <sel>)"`
  evaluation.
- **Remove** (destructive — guarded). Blocks when the worktree is dirty or the
  branch has unmerged commits, unless `--force`. Before removal it shows exactly
  what would be lost (uncommitted files; unmerged commit SHAs). It prefers
  `git branch -d` (refuses to drop unmerged) over `-D`. Every removed branch's
  tip SHA and worktree path are appended to a local recovery log
  (`$XDG_STATE_HOME/wtm/removed.log`) so work is recoverable via `git` reflog /
  the logged SHA even after the worktree is gone.

### Agent detection (best-effort enrichment, never load-bearing)

There is no cross-agent "I am attached" protocol, so detection is heuristic and
explicitly best-effort. It must never gate a destructive action and must never
assert "attached" when uncertain.

- Scan `/proc/<pid>/cwd` (readlink) and `/proc/<pid>/cmdline` on
  Linux/WSL. Map a pid to a worktree when its cwd is at or below the worktree
  path. Classify the agent from cmdline (e.g. `claude`, hermes harness markers),
  falling back to a generic "agent" label.
- **Liveness is validated before every display**: a pid that has exited is
  dropped immediately; a worktree with no matching live pid shows **no** agent
  (never a stale "attached").
- Detection is Linux/WSL-only in v1. On platforms without `/proc`, the agent
  column simply shows "—" and everything else works.

### Refresh model

A persistent dashboard must not hammer git. Refresh is event-driven with a
polling backstop:

- Watch the repo and worktree directories for filesystem changes (notify-style)
  and debounce bursts.
- A low-frequency backstop poll (default ~5s, with backoff when idle) catches
  changes the watcher misses (e.g. ahead/behind after a commit).
- All refresh work runs off the UI thread; git-call timeouts keep one slow
  worktree from freezing the dashboard.

### Config

A small TOML config (`$XDG_CONFIG_HOME/wtm/config.toml`): worktree path
template, poll interval, tmux on/off, and agent-cmdline match patterns so new
agents can be recognized without a rebuild.

## Alternatives

- **libgit2 via `git2`/`gix` instead of shelling out.** Rejected for v1:
  worktree + porcelain-v2 status + ahead/behind are trivial via the CLI and
  fiddly via bindings (`gitui` hits exactly these gaps), and bindings add build
  weight. `gix` remains a profiling-driven option if fork cost ever matters (it
  won't at solo-dev scale).
- **Pid files / an agent-cooperation protocol for attachment.** Rejected as the
  primary mechanism: it requires write-side buy-in from tools we don't control
  (Claude Code, Hermes). Left open as optional future enrichment.
- **Wrap/launch agents inside TUI-owned tmux sessions (claude-squad / uzi
  style) so attachment is known by construction.** Rejected for v1 because
  launching agents is explicitly out of scope; v1 observes agents the developer
  starts however they like.
- **TUI changes the shell's directory on "switch".** Impossible for a child
  process; the tmux-session approach is the standard workaround.
- **Drop agent-attachment entirely for v1** (the Minimalist dissent). Rejected:
  agent visibility is the feature that distinguishes this from plain `lazygit` /
  `gitui`. Kept, but demoted to best-effort per above.
- **Auto-fetch for accurate ahead/behind.** Rejected: network I/O on a refresh
  loop is slow and surprising; local-ref counts are good enough and clearly
  labeled.

## Risks

- **Silent data loss on remove** (primary). Contained by dirty/unmerged blocking
  unless `--force`, an explicit "what will be lost" preview, `-d` over `-D`, and
  the SHA recovery log. Detected via the recovery log if it still happens.
- **Stale/wrong agent status misleads the developer.** The Skeptic's core
  concern: a wrong "attached" is worse than none because it's trusted. Contained
  by per-display liveness validation and a strict "show nothing when unsure"
  rule; agent state never gates an action.
- **Refresh loop contention / battery drain** and lock races with active
  agents' own git ops. Contained by event-driven refresh + debounce + backoff,
  read-only porcelain calls, and per-call timeouts.
- **Detection heuristics rot** as Claude Code / Hermes change how they spawn.
  Contained by config-driven cmdline patterns (fixable without a rebuild) and by
  the fact that detection is non-critical.
- **Worktree disk bloat** (duplicated `node_modules` / build caches) becomes the
  real spin-up bottleneck. Acknowledged, out of scope for v1; noted so the
  success metric isn't misread.
- **`/proc`-only detection** means no agent column on non-Linux and none for
  agents in containers / remote boxes the TUI can't see. Accepted for v1
  (target host is WSL/Linux); degrades gracefully to "—".

## Rollout

1. **Milestone 1 — the metric.** `wtm create <branch>` subcommand + worktree
   path template + recovery log skeleton. This alone collapses spin-up to one
   keystroke and satisfies the primary success metric; shippable independently.
2. **Milestone 2 — dashboard.** Persistent `ratatui` list view with git status
   (branch, dirty/clean, ahead/behind local, last activity), event-driven
   refresh, degraded-worktree handling.
3. **Milestone 3 — switch + remove.** tmux switch (with path-print fallback) and
   guarded remove with the loss preview and recovery log.
4. **Milestone 4 — agent column.** `/proc` scan + liveness + config-driven
   classification.

Distribution: a single `cargo build --release` binary; install by dropping it on
`PATH`. No daemon, no persistent server state beyond the config and recovery log
— "turning it off" is deleting the binary; nothing it does is irreversible
beyond what guarded `remove` already protects.

## Open questions

- **Worktree path template default** — sibling `../<repo>-worktrees/<branch>` vs
  a dot-dir inside the repo. Decider: user. Affects M1.
- **tmux as a hard dependency vs optional** — is the path-print fallback
  acceptable as the only non-tmux switch, or is a built-in pane needed? Decider:
  user, during M3.
- **Agent classification taxonomy** — how granular (just "agent" vs named
  Claude Code / Hermes / other) and whether "recent activity" means git activity
  or process activity. Decider: user, during M4.
- **Windows/macOS support** — is `/proc`-less agent detection (or no agent
  column) acceptable there, or is v1 explicitly WSL/Linux-only? Decider: user.
- **Multiple agents in one worktree** — show all, or the most recent? Decider:
  user, during M4.
