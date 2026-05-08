# jj-worktree

> [English](./README.md) | 日本語

`git worktree` の操作を `jj workspace` コマンドに変換する git shim。

## なぜ必要か

[Claude Code](https://docs.anthropic.com/en/docs/claude-code) のようなツールは、並列セッションの分離に `git worktree` を使います。しかし jj リポジトリでは `git status` や ref 解決といった git 操作が正しく動作せず、worktree のワークフローが破綻してしまいます。jj-worktree は `git worktree` の呼び出しを横取りして、等価な `jj workspace` 操作に変換することで、このギャップを埋めます。

## 仕組み

1 つのバイナリが busybox パターンで `git` shim として振る舞います:

```
argv[0] == "git"         -> shim モード: worktree/branch/status/rev-parse サブコマンドを横取り
argv[0] == "jj-worktree" -> direct モード: workspace コマンドを直接実行
```

`git -> jj-worktree` の symlink を `PATH` の先頭に配置します。jj リポジトリ内では worktree 操作が jj workspace コマンドに変換されます。jj リポジトリの外（または無関係な git サブコマンド）では、本物の `git` が透過的に呼び出されます。

### 変換されるコマンド

| git コマンド | jj 相当 |
|---|---|
| `git worktree add <path> [-b <branch>] [<commit-ish>]` | `jj workspace add` + `jj bookmark set` + `jj new` |
| `git worktree list` | `jj workspace list` + `jj log` |
| `git worktree remove <path>` | `jj workspace forget` + クリーンアップ |
| `git branch -d <name>` | `jj bookmark delete`（管理対象の場合） |
| `git status [--porcelain]` | `jj diff --summary` |
| `git rev-parse <ref>` | `jj log -r <ref> -T commit_id`（`HEAD` → `@`） |

## インストール

```bash
brew install kawaz/tap/jj-worktree
```

またはソースからビルド:

```bash
cargo build --release
```

## 使い方

### Claude Code との併用

```bash
jj-worktree run claude --worktree
```

これは shim への symlink を作成し、それを `PATH` の先頭に追加してコマンドを `exec` します。shim はすべての子プロセスで有効になります。

- **インストール版**（PATH に存在）: `~/.cache/jj-worktree/bin/git` の symlink（共有、永続）
- **Dev ビルド**（PATH に未配置）: `$TMPDIR/jj-worktree.{hash}/git` の symlink（ビルドごと、再起動時にクリーンアップされる）

### 直接コマンド

```bash
jj-worktree add <path> [-b <branch>] [<commit-ish>]
jj-worktree list
jj-worktree remove [--force] <path>
```

## 環境変数

| 変数 | 説明 |
|---|---|
| `JJ_WORKTREE_DISABLED=1` | shim を無効化し、すべてのコマンドを本物の git に渡す |
| `JJ_WORKTREE_DEBUG=1` | デバッグログを stderr に出力（JSONL） |
| `JJ_WORKTREE_LOG_FILE=<path>` | デバッグログをファイルに追記（JSONL） |
| `JJ_WORKTREE_REAL_GIT=<path>` | 本物の git バイナリのパスを明示的に指定 |
| `JJ_WORKTREE_ISSUE_LOG=<path>` | 自己報告ログのパスを上書き（既定: `${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log`） |
| `JJ_WORKTREE_ISSUE_QUIET=1` | `[For AI agents]` stderr ブロックを抑制（JSONL ログは書き込まれる） |

## 未知オプションの取り扱い

`git worktree add` のオプションのうち `jj workspace` に等価のないもの（`--track`, `--no-track`, `--lock`, `--reason`, `--guess-remote`, `--no-guess-remote`, `--detach`）は静かに no-op として受け入れます。それ以外の未知フラグも no-op として受け入れますが、その都度 JSONL の自己報告ログに記録し、`[For AI agents]` ブロックを stderr に出力するため、AI 駆動の呼び出し側が黙ってフォールバックすることを防げます。詳細は [docs/decisions/DR-0001](./docs/decisions/DR-0001-lenient-unknown-options.md) と [DR-0002](./docs/decisions/DR-0002-self-reporting-mechanism.md) を参照。

## ライセンス

[MIT](LICENSE)
