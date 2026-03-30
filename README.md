# jj-worktree

A git shim that translates `git worktree` operations to `jj workspace` commands.

## Why

Tools like [Claude Code](https://docs.anthropic.com/en/docs/claude-code) use `git worktree` for parallel session isolation. In jj repositories, git operations like `git status` and ref resolution don't work correctly, causing the worktree workflow to fail. jj-worktree bridges this gap by intercepting `git worktree` calls and converting them to equivalent `jj workspace` operations.

## How it works

A single binary acts as a `git` shim via the busybox pattern:

```
argv[0] == "git"         -> shim mode: intercept worktree/branch/status/rev-parse subcommands
argv[0] == "jj-worktree" -> direct mode: run workspace commands directly
```

A symlink `git -> jj-worktree` is placed at the front of `PATH`. Inside a jj repository, worktree operations are translated to jj workspace commands. Outside a jj repository (or for unrelated git subcommands), the real `git` is called transparently.

### Translated commands

| git command | jj equivalent |
|---|---|
| `git worktree add <path> [-b <branch>] [<commit-ish>]` | `jj workspace add` + `jj bookmark set` + `jj new` |
| `git worktree list` | `jj workspace list` + `jj log` |
| `git worktree remove <path>` | `jj workspace forget` + cleanup |
| `git branch -d <name>` | `jj bookmark delete` (if managed) |
| `git status [--porcelain]` | `jj diff --summary` |
| `git rev-parse <ref>` | `jj log -r <ref> -T commit_id` (`HEAD` → `@`) |

## Install

```bash
brew install kawaz/tap/jj-worktree
```

Or build from source:

```bash
cargo build --release
```

## Usage

### With Claude Code

```bash
jj-worktree run claude --worktree
```

This creates a symlink to the shim, prepends it to `PATH`, and `exec`s the command. The shim is active for all child processes.

- **Installed version** (in PATH): symlink at `~/.cache/jj-worktree/bin/git` (shared, persistent)
- **Dev build** (not in PATH): symlink at `$TMPDIR/jj-worktree.{hash}/git` (per-build, cleaned up on reboot)

### Direct commands

```bash
jj-worktree add <path> [-b <branch>] [<commit-ish>]
jj-worktree list
jj-worktree remove [--force] <path>
```

## Environment variables

| Variable | Description |
|---|---|
| `JJ_WORKTREE_DISABLED=1` | Disable the shim; pass all commands to real git |
| `JJ_WORKTREE_DEBUG=1` | Print debug logs to stderr (JSONL) |
| `JJ_WORKTREE_LOG_FILE=<path>` | Append debug logs to file (JSONL) |
| `JJ_WORKTREE_REAL_GIT=<path>` | Explicitly set the real git binary path |

## License

[MIT](LICENSE)
