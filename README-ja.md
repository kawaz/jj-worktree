# jj-worktree

> [English](./README.md) | 日本語

`git worktree` の操作を `jj workspace` コマンドに変換する git shim。

**対象ユーザ**: jj 上で [Claude Code](https://docs.anthropic.com/en/docs/claude-code) を `isolation: "worktree"` で使う、もしくは jj リポジトリ内で `git worktree` を呼ぶツールを使うユーザ。それ以外の場合はおそらく不要です。

## クイックスタート

```bash
brew install kawaz/tap/jj-worktree
jj-worktree run claude       # shim を有効化した状態で Claude Code を起動 (子プロセス全部に効く)
```

`jj-worktree run <cmd>` は `git → jj-worktree` の symlink を作って `PATH` の先頭に追加し、`<cmd>` を `exec` します。子プロセス内で `git worktree` (および関連する一部の `git` サブコマンド) が `jj workspace` に翻訳されます。`jj-worktree run` を経由しないと shim は **有効になりません** — Claude Code を直接起動した場合は素の `git` が呼ばれます。

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

それ以外はすべて本物の `git` にそのまま渡されます。colocated workspace のバッキングストアは本物の git worktree なので、忠実な翻訳が存在しない操作では real git が正です: `git worktree prune`、`move`、`repair`、`lock`、`list --porcelain` のようなオプション付き形式、そして `add --detach` / `-d` / `--orphan` はすべて real git に pass-through します。`jj` 自身が発行する `git worktree` 呼び出しも同様です（jj は自ワークスペースのバッキング git worktree を管理しており、これを翻訳すると再帰します）。`git worktree remove` は `jj` に登録済みの対象のみ翻訳し、git 由来の worktree（detach/orphan pass-through で作成）は real git に渡されます。この場合 git 自身の安全機構（未コミット変更チェック、locked、メインワークスペース保護）が適用されます。

呼び出し側起点の `git worktree prune` はさらに、作業ディレクトリが存在しない jj workspace を reap します: 各 workspace に対して `jj workspace forget` を実行し（パスは `jj workspace root --name` で解決するため、本 shim を経由せず作られた workspace も対象）、古い `.jj-worktree-meta` エントリを削除します。ディレクトリを退避してから prune する形で worktree を削除する呼び出し側（例: Kilo Code の Agent Manager）が jj workspace レコードを残留させないための仕組みです。reap は real git 自身の prune 判定と同期します — 呼び出し側オプションを転送した `--dry-run` レポートをパースするため、`--expire` や locked レコードの判定は git 自身に委譲されます — そして `--dry-run` / `-n` 付きの prune は何も reap しません。保護対象 workspace（`default`, `main`）は決して reap せず、`jj` 自身の発行する prune は純粋な pass-through のままです。

## インストール

```bash
brew install kawaz/tap/jj-worktree
```

`kawaz/tap` は [`kawaz/homebrew-tap`](https://github.com/kawaz/homebrew-tap) リポジトリのこと。同等の2ステップ形式: `brew tap kawaz/tap && brew install jj-worktree`。

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

翻訳対象の `git worktree add` では、`jj workspace` に等価のないオプション（`--track`, `--no-track`, `--lock`, `--reason`, `--guess-remote`, `--no-guess-remote`）は静かに no-op として受け入れます。`--detach`、`-d`、`--orphan` は翻訳せず、呼び出し全体を real git に渡します。それ以外の未知フラグも no-op として受け入れますが、その都度 JSONL の自己報告ログに記録し、`[For AI agents]` ブロックを stderr に出力するため、AI 駆動の呼び出し側が黙ってフォールバックすることを防げます。詳細は [docs/decisions/DR-0001](./docs/decisions/DR-0001-lenient-unknown-options.md)、[DR-0002](./docs/decisions/DR-0002-self-reporting-mechanism.md)、[DR-0004](./docs/decisions/DR-0004-passthrough-untranslatable-worktree.md) を参照。

## ライセンス

[MIT](LICENSE)
