# jj-worktree

> English | [日本語](./README-ja.md)

A git shim that translates `git worktree` operations to `jj workspace` commands.

**Who is this for**: jj users who run [Claude Code](https://docs.anthropic.com/en/docs/claude-code) with `isolation: "worktree"` enabled (or any other tool that issues `git worktree` calls inside a jj repository). If you don't use such tools, you probably don't need jj-worktree.

## Quick start

```bash
brew install kawaz/tap/jj-worktree
jj-worktree run claude       # launch Claude Code with the shim active for all child processes
```

`jj-worktree run <cmd>` symlinks `git → jj-worktree` into a directory it prepends to `PATH`, then `exec`s `<cmd>`. Inside that subprocess, `git worktree` (and a few related `git` calls) are translated to `jj workspace`. Without `jj-worktree run`, the shim is **not active** — Claude Code launched directly will keep using the real `git`.

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

Anything else reaches real `git` unchanged. A colocated workspace *is* a real git worktree, so real git is the authority wherever a faithful translation does not exist: `git worktree prune`, `move`, `repair`, `lock`, optioned forms such as `list --porcelain`, and `add --detach` / `-d` / `--orphan` all pass through. So does every `git worktree` call issued by `jj` itself — jj manages the backing git worktree of its own workspaces, and translating those calls would recurse. `git worktree remove` only translates targets registered with `jj`; git-native worktrees (created via the detach/orphan passthrough) are passed to real git, whose own safety checks (uncommitted changes, locked worktrees, main-worktree protection) then apply.

A caller-initiated `git worktree prune` additionally reaps jj workspaces whose working directory no longer exists: it runs `jj workspace forget` for each one (paths resolved via `jj workspace root --name`, so workspaces created without this shim are covered) and drops the stale `.jj-worktree-meta` entry. Callers that delete worktrees by renaming the directory aside and pruning (Kilo Code's Agent Manager, for example) thus leave no jj workspace records behind. The reap mirrors real git's own prune decision — it parses the `--dry-run` report git produces with the caller's forwarded options, so `--expire` and locked records are honored by git itself — and a `--dry-run` / `-n` prune never reaps anything. Protected workspaces (`default`, `main`) are never reaped, and prunes issued by `jj` itself stay pure pass-through.

## Install

```bash
brew install kawaz/tap/jj-worktree
```

`kawaz/tap` refers to the [`kawaz/homebrew-tap`](https://github.com/kawaz/homebrew-tap) repository. Equivalent two-step form: `brew tap kawaz/tap && brew install jj-worktree`.

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
| `JJ_WORKTREE_ISSUE_LOG=<path>` | Override the self-report log path (default: `${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log`) |
| `JJ_WORKTREE_ISSUE_QUIET=1` | Suppress the `[For AI agents]` stderr block (JSONL log still written) |

## Unknown option handling

On a translated `git worktree add`, flags that have no `jj workspace` equivalent (`--track`, `--no-track`, `--lock`, `--reason`, `--guess-remote`, `--no-guess-remote`) are silently accepted as no-ops. `--detach`, `-d`, and `--orphan` are not translated at all — the whole call goes to real git. Other unknown flags are also accepted, but each occurrence is recorded in the JSONL self-report log and an `[For AI agents]` block is emitted to stderr so AI-driven callers do not silently fall back. See [docs/decisions/DR-0001](./docs/decisions/DR-0001-lenient-unknown-options.md), [DR-0002](./docs/decisions/DR-0002-self-reporting-mechanism.md) and [DR-0004](./docs/decisions/DR-0004-passthrough-untranslatable-worktree.md).

## License

[MIT](LICENSE)
