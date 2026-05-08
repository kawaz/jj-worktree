# jj-worktree Design Document

> English | [日本語](./DESIGN-ja.md)

## Background

Claude Code provides parallel session isolation via `git worktree`, but in jj repositories:
- The presence of `.git` causes the directory to be detected as a git repository, and git-based operations are attempted
- However, git operations such as `git status` and ref resolution do not work correctly in a jj environment, breaking the entire worktree flow
- WorktreeCreate/WorktreeRemove hooks are ignored inside git repositories (Issue #36205)

## Solution

Build a shim for the `git` command that translates only the `worktree` subcommand into jj workspace operations.

## Architecture

### busybox pattern

A single binary switches its operating mode based on `argv[0]`:

```
argv[0] == "git"          → shim mode (shim.rs)
argv[0] == "jj-worktree"  → direct mode (main.rs → worktree.rs)
```

By creating a `git → jj-worktree` symlink and placing it at the front of PATH, it operates transparently.

### Module layout

```
src/
├── main.rs       # entry point, argv[0] dispatch, run command
├── shim.rs       # git shim: global option parsing, real git detection, command dispatch
├── worktree.rs   # add/list/remove: implementation of jj workspace operations
├── jj.rs         # jj command execution helpers, repository detection
├── meta.rs       # metadata CRUD (.jj-worktree-meta/*.json)
├── issue_log.rs  # self-reporting (DR-0002): JSONL log for unknown_option etc. + AI-directed stderr
└── test_util.rs  # test-only helpers (crate-wide env_lock for tests that mutate env)
```

### Command flow

```
git worktree add -B branch path origin/main
  ↓ shim.rs: argv[0]=="git" → parse global opts → subcmd=="worktree" → .jj present
  ↓ worktree.rs: cmd_add()
  ├── jj workspace add <path>
  ├── jj bookmark set <branch> -r <wsname>@
  ├── translate_git_ref("origin/main") → "main@origin"
  ├── jj -R <ws_path> new main@origin
  └── meta::save() → .jj-worktree-meta/<wsname>.json

git worktree list
  ↓ worktree.rs: cmd_list()
  ├── jj workspace list → list of workspace names
  ├── for each workspace: get path from metadata or repo_root/ws_name
  ├── jj log -r '<ws>@' to obtain commit hash + bookmarks
  └── output in git worktree list compatible format

git worktree remove <path>
  ↓ worktree.rs: cmd_remove()
  ├── normalize path → canonical path
  ├── main/default workspace → reject
  ├── without --force: check for uncommitted changes
  ├── bookmark from metadata → jj bookmark delete
  ├── jj workspace forget
  ├── remove directory
  └── remove metadata

git branch -d <name>
  ↓ shim.rs: subcmd=="branch" + -d/-D/--delete
  ├── meta::find_by_bookmark() to check whether it is managed
  ├── managed → jj bookmark delete
  └── not managed → fall back to real git

git status [--porcelain] [-C <path>]
  ↓ shim.rs: subcmd=="status" + inside jj repo
  ├── run jj diff --summary
  ├── convert output to git porcelain v1 format (M→" M", A→"??", D→" D")
  └── on jj diff failure, fall back to real git

git rev-parse [--verify] <ref>
  ↓ shim.rs: subcmd=="rev-parse" + inside jj repo
  ├── flag parsing: only --verify is allowed; other flags → fall back to real git
  ├── jj git export to sync repository state
  ├── if <ref> is "HEAD", convert to "@"
  ├── jj log -r <ref> --no-graph -T commit_id to obtain hash
  └── print the hash (on resolution failure, fall back to real git)

git <other>
  ↓ shim.rs → exec_real_git() (Unix: process replace via exec)
```

### git ref → jj ref translation (translate_git_ref)

Claude Code passes git-style remote references. Since jj uses different notation, translation is required:

| git ref | jj ref | Notes |
|---------|--------|------|
| `origin/main` | `main@origin` | remote/branch → branch@remote |
| `origin/HEAD` | `trunk()` | remote HEAD is substituted with trunk() |
| `HEAD` | `@` | the current revision of the working copy |
| `abc1234` | `abc1234` | commit hashes are passed through unchanged |

During translation, `jj git remote list` is consulted to enumerate known remote names, and translation is applied only when the name matches a known remote.

### git global option parsing (parse_git_global_opts)

Before the shim intercepts, it skips git's global options to identify the subcommand:

```
git [-C path] [-c key=val] [--git-dir=path] [--work-tree=path]
    [--no-pager] [--bare] [--literal-pathspecs] [--glob-pathspecs]
    [--noglob-pathspecs] [--icase-pathspecs] [--no-replace-objects]
    <subcommand> [args...]
```

`-C <path>` is used as the starting point for jj repository detection.

### real git detection (find_real_git)

1. `JJ_WORKTREE_REAL_GIT` environment variable → if set, use that path
2. PATH traversal → compare with the canonical path of `env::current_exe()` to exclude the binary itself
3. If not found, return an error

### jj repository detection (find_repo_root)

Walk upward from the current directory (or the path specified by `-C`), looking for a `.jj` directory. The `jj root` command is not used; only filesystem checks are performed for fast detection. If no `.jj` is found, all commands fall back to real git.

## Metadata

Storage location: `<repo_root>/.jj-worktree-meta/<wsname>.json`

```json
{
  "workspace": "feature-auth",
  "bookmark": "worktree-feature-auth",
  "created_at": "2026-03-27T12:00:00Z",
  "path": "/absolute/path/to/feature-auth"
}
```

Uses:
- `list`: obtain absolute paths of workspaces
- `remove`: reverse-lookup path → workspace name, and obtain bookmark name
- `branch -d`: bookmark name → determine whether it is managed (`find_by_bookmark`)

Bookmark deletion targets only names recorded in the metadata; broad deletion via pattern matching is not performed.

## Setup

### `jj-worktree run`

```bash
jj-worktree run claude --worktree
```

Creates a symlink to the shim, prepends it to PATH, and then `exec`s the command. The shim becomes effective in all child processes.

The symlink location is determined by how the binary was launched:
- **Installed version** (on PATH): `${XDG_CACHE_HOME:-$HOME/.cache}/jj-worktree/bin/git` — shared across multiple sessions
- **Development version** (local build, etc.): `${TMPDIR}/jj-worktree.{hash}/git` — isolated by a key derived from a hash of the binary's canonical path. Avoids overwriting the production shim. Automatically cleaned up on OS reboot

If a `jj-worktree` entry on PATH points to the same binary, it is treated as the installed version. Even if `brew upgrade` removes a versioned path, using a stable path (e.g. `/opt/homebrew/bin/jj-worktree`) as the symlink target ensures resilience.

## Environment variables

| Variable | Purpose |
|------|------|
| `JJ_WORKTREE_DISABLED=1` | Disable the shim; fall back all commands to real git |
| `JJ_WORKTREE_DEBUG=1` | Emit debug logs as JSONL on stderr |
| `JJ_WORKTREE_LOG_FILE=<path>` | Append debug logs to a file as JSONL (can be combined with DEBUG) |
| `JJ_WORKTREE_REAL_GIT=<path>` | Explicitly specify the path to the real git binary |
| `JJ_WORKTREE_ISSUE_LOG=<path>` | Override the self-reporting issues log path (default: `${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log`) |
| `JJ_WORKTREE_ISSUE_QUIET=1` | Suppress the self-report stderr block (the JSONL log is still written) |

Log format (JSONL):
```json
{"ts":"2026-03-30T09:00:00.123Z","pid":1234,"msg":"exec real git: /usr/bin/git --version"}
```

## Lenient pass-through of unknown options (DR-0001 / DR-0002)

The set of options that callers (e.g. Claude Code) pass to `git worktree add` may grow as those callers evolve. `jj-worktree` handles this leniently:

1. **Explicit no-op list**: `--track` / `--no-track` / `--lock` / `--reason <val>` / `--guess-remote` / `--no-guess-remote` / `--detach` are silently ignored because there is no equivalent concept on the `jj workspace` side.
2. **Unknown `--xxx` / `-x` flags**: instead of failing, the shim records a warning entry via `issue_log` and emits a `[For AI agents]` directed block on stderr, then accepts the flag as a no-op. Because the shim cannot tell whether a value follows, `--xxx=val` is consumed as a single argument, while a bare `--xxx` does not consume the next argument as a value.

The self-report log (`issues.log`) records a `kind` of `unknown_option` / `parse_error` / `unsupported_combination`, plus `ts` / `cmd` / `option` / `argv` / `caller_pid` / `caller_cmdline`. This surfaces gaps to the user before AI agents silently fall back.

## Safety mechanisms

1. **main/default workspace protection**: `remove` rejects deletion
2. **uncommitted change check**: `remove` checks `jj diff --stat` (override with `--force`)
3. **path containment**: `remove` verifies that the target path is under repo_root
4. **scoped bookmark deletion**: only names recorded in the metadata are targeted
5. **shim bypass**: `JJ_WORKTREE_DISABLED=1` falls back to real git immediately

## Build and release

### Targets

| Target | OS | Arch |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Linux | x86_64 |
| `x86_64-unknown-linux-musl` | Linux (static) | x86_64 |
| `aarch64-unknown-linux-gnu` | Linux | ARM64 |
| `aarch64-unknown-linux-musl` | Linux (static) | ARM64 |
| `x86_64-apple-darwin` | macOS | Intel |
| `aarch64-apple-darwin` | macOS | Apple Silicon |

Windows is not supported (symlink + exec are inherently Unix-dependent).

### Release flow

```
just bump-version [patch|minor|major]
  ↓
ensure-clean → test → build → bump Cargo.toml → jj describe + new → just push
  ↓
GitHub Actions (release.yml) detects Cargo.toml change on main
  ↓
build for 6 targets → gh release create --target <sha> --generate-notes
  ↓
auto-create tag v<X.Y.Z>, GitHub Release notes, and homebrew-tap Formula update
```

See `docs/decisions/DR-0003-release-flow.md` for the rationale (why local-only tag operations are avoided, why CHANGELOG.md is delegated to `--generate-notes`).

### Installation

```bash
brew install kawaz/tap/jj-worktree
```

## Tests

70 tests (48 unit + 22 integration)

### Unit tests

- `jj.rs`: find_repo_root, build_command (4)
- `meta.rs`: save/load/remove/list/serialization (9)
- `shim.rs`: parse_git_global_opts (18), parse_branch_delete (6), parse_rev_parse_refs (9)
- `main.rs`: invocation_mode, help (2)

### Integration tests (tests/integration.rs)

Each test initializes a jj repository in a temporary directory before running:

1. shim pass-through (no .jj → real git)
2. shim `-C` support
3. jj detection + worktree redirection
4. add: workspace + bookmark + metadata
5. add: without bookmark / with commit-ish
6. list: format / empty repository / via shim
7. remove: safety checks / main protection / relative path / via shim
8. branch -d: managed → jj bookmark delete / not managed → real git

## Repository

- Path: `~/.local/share/repos/github.com/kawaz/jj-worktree/main/`
- GitHub: https://github.com/kawaz/jj-worktree
- Homebrew: `brew install kawaz/tap/jj-worktree`
