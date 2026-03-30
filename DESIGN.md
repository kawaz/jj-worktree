# jj-worktree 設計書

## 背景

Claude Code は `git worktree` による並列セッション隔離機能を持つが、jj リポジトリでは:
- `.git` (bare) があるため git リポジトリと判定される
- しかし `git worktree add` は detached HEAD / bare 環境で失敗 (Issue #27466)
- WorktreeCreate/WorktreeRemove hooks は git リポジトリ内で無視される (Issue #36205)

## 解決策

`git` コマンドの shim を作り、`worktree` サブコマンドだけ jj workspace に変換する。

## アーキテクチャ

### busybox パターン

単一バイナリが `argv[0]` で動作モードを切り替える:

```
argv[0] == "git"          → shim モード (shim.rs)
argv[0] == "jj-worktree"  → 直接モード (main.rs → worktree.rs)
```

symlink で `git → jj-worktree` を作成し、PATH の先頭に置くことで透過的に動作。

### モジュール構成

```
src/
├── main.rs       # エントリポイント、argv[0] 判定、setup/run コマンド
├── shim.rs       # git shim: グローバルオプション解析、real git 検出、コマンド振り分け
├── worktree.rs   # add/list/remove: jj workspace 操作の実装
├── jj.rs         # jj コマンド実行ヘルパー、リポジトリ検出
└── meta.rs       # メタデータ CRUD (.jj-worktree-meta/*.json)
```

### コマンドフロー

```
git worktree add -B branch path origin/main
  ↓ shim.rs: argv[0]=="git" → parse global opts → subcmd=="worktree" → .jj あり
  ↓ worktree.rs: cmd_add()
  ├── jj workspace add <path>
  ├── jj bookmark set <branch> -r <wsname>@
  ├── translate_git_ref("origin/main") → "main@origin"
  ├── jj -R <ws_path> new main@origin
  └── meta::save() → .jj-worktree-meta/<wsname>.json

git worktree list
  ↓ worktree.rs: cmd_list()
  ├── jj workspace list → workspace 名一覧
  ├── 各 workspace: メタデータ or repo_root/ws_name でパス取得
  ├── jj log -r '<ws>@' で commit hash + bookmarks 取得
  └── git worktree list 互換フォーマットで出力

git worktree remove <path>
  ↓ worktree.rs: cmd_remove()
  ├── パス → canonical path に正規化
  ├── main/default workspace → 拒否
  ├── --force なし: 未コミット変更チェック
  ├── メタデータの bookmark → jj bookmark delete
  ├── jj workspace forget
  ├── ディレクトリ削除
  └── メタデータ削除

git branch -d <name>
  ↓ shim.rs: subcmd=="branch" + -d/-D/--delete
  ├── meta::find_by_bookmark() で管理対象か確認
  ├── 管理対象 → jj bookmark delete
  └── 非管理対象 → real git にフォールバック

git status [--porcelain] [-C <path>]
  ↓ shim.rs: subcmd=="status" + jj repo 内
  ├── jj diff --summary を実行
  ├── 出力を git porcelain v1 形式に変換 (M→" M", A→"??", D→" D")
  └── jj diff 失敗時は real git にフォールバック

git <other>
  ↓ shim.rs → exec_real_git() (Unix: process replace via exec)
```

### git ref → jj ref 変換 (translate_git_ref)

Claude Code は git 形式のリモート参照を渡す。jj では記法が異なるため変換が必要:

| git ref | jj ref | 備考 |
|---------|--------|------|
| `origin/main` | `main@origin` | remote/branch → branch@remote |
| `origin/HEAD` | `trunk()` | リモート HEAD は trunk() で代替 |
| `HEAD` | `HEAD` | ローカル ref はそのまま |
| `abc1234` | `abc1234` | コミットハッシュはそのまま |

変換時に `jj git remote list` で既知のリモート名を確認し、リモート名に一致する場合のみ変換。

### git グローバルオプション解析 (parse_git_global_opts)

shim が傍受する前に、git のグローバルオプションをスキップしてサブコマンドを特定する:

```
git [-C path] [-c key=val] [--git-dir=path] [--work-tree=path]
    [--no-pager] [--bare] [--literal-pathspecs] [--glob-pathspecs]
    [--noglob-pathspecs] [--icase-pathspecs] [--no-replace-objects]
    <subcommand> [args...]
```

`-C <path>` は jj リポジトリ検出の起点として使用。

### real git 検出 (find_real_git)

1. `JJ_WORKTREE_REAL_GIT` 環境変数 → あればそのパスを使用
2. PATH 走査 → `env::current_exe()` の canonical path と比較し、自分自身を除外
3. 見つからなければエラー

### jj リポジトリ検出 (find_repo_root)

カレントディレクトリ（または `-C` 指定パス）から上方向に `.jj` ディレクトリを探索。`jj root` コマンドは使わずファイルシステム確認のみで高速判定。`.jj` が見つからなければ全コマンドを real git にフォールバック。

## メタデータ

保存先: `<repo_root>/.jj-worktree-meta/<wsname>.json`

```json
{
  "workspace": "feature-auth",
  "bookmark": "worktree-feature-auth",
  "created_at": "2026-03-27T12:00:00Z",
  "path": "/absolute/path/to/feature-auth"
}
```

用途:
- `list`: workspace の絶対パス取得
- `remove`: path → workspace 名の逆引き、bookmark 名の取得
- `branch -d`: bookmark 名 → 管理対象かの判定 (`find_by_bookmark`)

bookmark 削除はメタデータに記録された名前のみを対象とし、パターンマッチによる広範な削除は行わない。

## セットアップ方法

### `jj-worktree run` (推奨、一時的)

```bash
jj-worktree run claude --worktree
```

`${XDG_CACHE_HOME:-$HOME/.cache}/jj-worktree/bin/git` に symlink を作成し、PATH の先頭に追加してから `exec` でコマンドを起動。子プロセス全てで shim が有効になる。永続的な変更なし。

### `jj-worktree setup` (永続的)

```bash
jj-worktree setup --path ~/.local/bin/
```

指定ディレクトリに `git` symlink を作成。既に存在する場合は自分自身を指す symlink かチェック。

## 環境変数

| 変数 | 用途 |
|------|------|
| `JJ_WORKTREE_DISABLED=1` | shim を無効化、全コマンドを real git にフォールバック |
| `JJ_WORKTREE_DEBUG=1` | デバッグログを stderr に JSONL 出力 |
| `JJ_WORKTREE_LOG_FILE=<path>` | デバッグログをファイルに JSONL append 出力（DEBUG と併用可） |
| `JJ_WORKTREE_REAL_GIT=/path` | real git バイナリパスを明示指定 |

ログ形式 (JSONL):
```json
{"ts":"2026-03-30T09:00:00.123Z","pid":1234,"msg":"exec real git: /usr/bin/git --version"}
```

## 安全機構

1. **main/default workspace 保護**: `remove` で削除を拒否
2. **未コミット変更チェック**: `remove` で `jj diff --stat` を確認（`--force` で強制）
3. **パス containment**: `remove` で対象パスが repo_root 配下かを検証
4. **bookmark 削除のスコープ制限**: メタデータに記録された名前のみ対象
5. **shim バイパス**: `JJ_WORKTREE_DISABLED=1` で即座に real git にフォールバック

## ビルド・リリース

### ターゲット

| ターゲット | OS | Arch |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Linux | x86_64 |
| `x86_64-unknown-linux-musl` | Linux (static) | x86_64 |
| `aarch64-unknown-linux-gnu` | Linux | ARM64 |
| `aarch64-unknown-linux-musl` | Linux (static) | ARM64 |
| `x86_64-apple-darwin` | macOS | Intel |
| `aarch64-apple-darwin` | macOS | Apple Silicon |

Windows 非対応（symlink + exec が本質的に Unix 依存）。

### リリースフロー

```
just release [patch|minor|major]
  ↓
cargo fmt/clippy/test → バージョン bump → jj commit → tag → push
  ↓
GitHub Actions (release.yml)
  ↓
6ターゲットビルド → GitHub Release 作成 → homebrew-tap Formula 自動更新
```

### インストール

```bash
brew install kawaz/tap/jj-worktree
```

## テスト

61 テスト (39 unit + 22 integration)

### ユニットテスト

- `jj.rs`: find_repo_root, build_command (4)
- `meta.rs`: save/load/remove/list/serialization (9)
- `shim.rs`: parse_git_global_opts (18), parse_branch_delete (6)
- `main.rs`: invocation_mode, help (2)

### 統合テスト (tests/integration.rs)

各テストで一時ディレクトリに jj リポジトリを初期化して実行:

1. shim パススルー（.jj なし → real git）
2. shim `-C` 対応
3. jj 検出 + worktree リダイレクト
4. add: workspace + bookmark + メタデータ
5. add: bookmark なし / commit-ish 指定
6. list: フォーマット / 空リポジトリ / shim 経由
7. remove: 安全性チェック / main 保護 / 相対パス / shim 経由
8. branch -d: 管理対象 → jj bookmark delete / 非管理対象 → real git

## リポジトリ

- パス: `~/.local/share/repos/github.com/kawaz/jj-worktree/main/`
- GitHub: https://github.com/kawaz/jj-worktree
- Homebrew: `brew install kawaz/tap/jj-worktree`
