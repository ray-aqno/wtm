# wtm — worktree manager

A persistent terminal UI (and quick CLI) for managing git worktrees in tandem
with parallel AI coding agents. Spin up an isolated worktree for an agent in one
keystroke, then watch every worktree's branch, status, and attached agent from a
live dashboard — no manual `git worktree` plumbing, no losing track of what each
agent is doing.

Built in Rust with [ratatui](https://ratatui.rs). Single binary. Linux/WSL.

## Why

Running several AI agents at once means several isolated branches, which in git
means several worktrees. Doing that by hand is `git worktree add`, naming the
branch, `cd`-ing around, and repeatedly checking status. `wtm` collapses the hot
path — creating an agent's worktree — to one command, and keeps a dashboard of
everything running.

## Install

```sh
cargo build --release
# drop target/release/wtm on your PATH
```

Requires git >= 2.36 (for `worktree list --porcelain -z`); `tmux` is optional
(used for `switch`).

## Use

Run with no arguments inside any git repo to launch the dashboard:

```sh
wtm
```

Dashboard keys: `j`/`k` move, `r` refresh, `q` quit. The header shows the
worktree count, whether refresh is live (filesystem events) or `poll-only`, and
a truncation banner if the worktree count exceeds the safety cap.

Non-interactive subcommands (the hot path — bind `create` to a shell key):

```sh
wtm create <branch>   # new worktree + branch under <repo>-worktrees/, prints path
wtm path   <branch>   # print the worktree path (for shell `cd "$(wtm path x)"`)
wtm rm     <branch>   # remove a worktree, guarding against data loss
```

`rm` refuses to delete a dirty worktree unless `--force`, and `--force`
snapshots uncommitted work to a recovery ref first; every removal's branch tip
SHA and path are appended to `$XDG_STATE_HOME/wtm/removed.log` so nothing is
truly lost.

## Design

See [docs/rfc/0001](docs/rfc/0001-persistent-tui-for-git-worktree-management-in-tand.md)
for the full proposal. In short: shell out to the `git` CLI (never libgit2),
compute ahead/behind from local refs only (no network), detect attached agents
best-effort via `/proc` (never trusted to gate a destructive action), and refresh
on filesystem events with a debounced poll backstop.

## License

MIT
