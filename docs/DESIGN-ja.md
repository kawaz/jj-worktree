# jj-worktree 設計書

> [English](./DESIGN.md) | 日本語

## 背景

Claude Code は `git worktree` による並列セッション隔離機能を持つが、jj リポジトリでは:
- `.git` があるため git リポジトリと判定され、git 経由の操作が試みられる
- しかし `git status` や ref 解決など git の各種操作が jj 環境では正しく動作せず、worktree フロー全体が破綻する
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
├── main.rs       # エントリポイント、argv[0] 判定、run コマンド
├── shim.rs       # git shim: グローバルオプション解析、real git 検出、コマンド振り分け
├── worktree.rs   # add/list/remove + prune reap: jj workspace 操作の実装
├── jj.rs         # jj コマンド実行ヘルパー、リポジトリ検出
├── meta.rs       # メタデータ CRUD (.jj-worktree-meta/*.json)
├── issue_log.rs  # 自己報告メカニズム (DR-0002): unknown_option 等を JSONL 記録 + AI 誘導 stderr
└── test_util.rs  # テスト専用ヘルパー (env を mutate するテスト用の crate-wide env_lock)
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

git worktree prune [--expire ...]   （呼び出し側起点: 親プロセスは jj ではない）
  ↓ shim.rs: ディレクトリが消滅した workspace を reap
  ├── jj workspace list → workspace 名一覧
  ├── 保護対象以外の各 workspace: jj workspace root --name <ws> で記録パス取得
  └── パスが存在しない → jj workspace forget + 古いメタデータ削除（best effort）
  ↓ exec_real_git() — real git が git 側の管理情報を prune

git worktree <翻訳不能な操作> (prune/move/repair/lock、list --porcelain、add --detach/--orphan)
  ↓ shim.rs → exec_real_git()
  (colocated workspace は本物の git worktree なので real git が正。
   親プロセスが jj である全 `git worktree` 呼び出しも同様 —
   jj 自身の内部呼び出しを翻訳すると再帰する。)

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

git rev-parse [--verify] <ref>
  ↓ shim.rs: subcmd=="rev-parse" + jj repo 内
  ├── フラグ解析: --verify のみ許容、他フラグ → real git にフォールバック
  ├── jj git export でリポジトリ状態を同期
  ├── <ref> が "HEAD" の場合は "@" に変換
  ├── jj log -r <ref> --no-graph -T commit_id で hash を取得
  └── hash を出力（解決失敗時は real git にフォールバック）

git <other>
  ↓ shim.rs → exec_real_git() (Unix: process replace via exec)
```

### git ref → jj ref 変換 (translate_git_ref)

Claude Code は git 形式のリモート参照を渡す。jj では記法が異なるため変換が必要:

| git ref | jj ref | 備考 |
|---------|--------|------|
| `origin/main` | `main@origin` | remote/branch → branch@remote |
| `origin/HEAD` | `trunk()` | リモート HEAD は trunk() で代替 |
| `HEAD` | `@` | ワーキングコピーの現在リビジョン |
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

### `jj-worktree run`

```bash
jj-worktree run claude --worktree
```

shim への symlink を作成し、PATH の先頭に追加してから `exec` でコマンドを起動。子プロセス全てで shim が有効になる。

symlink の配置先はバイナリの起動方法で分岐:
- **インストール版** (PATH 上): `${XDG_CACHE_HOME:-$HOME/.cache}/jj-worktree/bin/git` — 複数セッションで共有
- **開発版** (ローカルビルド等): `${TMPDIR}/jj-worktree.{hash}/git` — バイナリの canonical path をハッシュ化したキーで隔離。本番シムを上書きしない。OS 再起動時に自動クリーンアップ

PATH 上に同一バイナリを指す `jj-worktree` エントリがあればインストール版と判定し、brew upgrade でバージョン付きパスが消えても安定パス（例: `/opt/homebrew/bin/jj-worktree`）を symlink 先に使うことで耐障害性を確保。

## 環境変数

| 変数 | 用途 |
|------|------|
| `JJ_WORKTREE_DISABLED=1` | shim を無効化、全コマンドを real git にフォールバック |
| `JJ_WORKTREE_DEBUG=1` | デバッグログを stderr に JSONL 出力 |
| `JJ_WORKTREE_LOG_FILE=<path>` | デバッグログをファイルに JSONL append 出力（DEBUG と併用可） |
| `JJ_WORKTREE_REAL_GIT=<path>` | real git バイナリパスを明示指定 |
| `JJ_WORKTREE_ISSUE_LOG=<path>` | 自己報告ログ (issues.log) のパスを上書き（既定: `${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log`） |
| `JJ_WORKTREE_ISSUE_QUIET=1` | 自己報告の stderr 出力を抑制（JSONL 記録は継続） |

ログ形式 (JSONL):
```json
{"ts":"2026-03-30T09:00:00.123Z","pid":1234,"msg":"exec real git: /usr/bin/git --version"}
```

## pass-through ポリシー (DR-0001 / DR-0002 / DR-0004)

`git worktree add` のオプションは呼び出し側 (Claude Code 等) のバージョンアップで増える可能性があり、shim を system-wide な `git` として常駐させる呼び出し側もある。`jj-worktree` はこれを層状のポリシーで扱う:

1. **明示的な no-op リスト（翻訳対象呼び出しのみ）**: `--track` / `--no-track` / `--lock` / `--reason <val>` / `--guess-remote` / `--no-guess-remote` は、翻訳される `add` に対して静かに無視する。jj workspace 側に対応概念がないため。`--detach` / `-d` / `--orphan` は翻訳せず、呼び出し全体を real git に渡す。
2. **未知の `--xxx` / `-x` フラグ**: エラーにせず、警告として `issue_log` に記録 + stderr に `[For AI agents]` 誘導ブロックを出力した上で no-op として受け入れる。値あり/なしの区別がつかないため、`--xxx=val` 形式は1引数で消費、`--xxx` 単独は次の引数を値とは見なさない。
3. **drop-in 契約 (DR-0004)**: shim が翻訳するのは完全に再現できる worktree 操作（`--detach`/`-d`/`--orphan` を含まない `add`、jj 登録済み workspace への `remove`、オプションなし `list`）のみ。それ以外 — `prune`、`move`、`repair`、`lock`、`list --porcelain` 等のオプション付き形式 — はすべてそのまま real git に渡す。親プロセスが `jj` である全 `git worktree` 呼び出しも同様（`caller_is_jj` ガード）。jj は自ワークスペースのバッキング git worktree を管理しており、その呼び出しを翻訳すると再帰する。git 由来の worktree（`--detach` / `-d` / `--orphan` pass-through で作成）への `remove` も real git に渡され、git 自身の安全機構（未コミット変更、locked、メインワークスペース保護）が働く。
4. **prune 時の reap (DR-0004)**: 呼び出し側起点の `prune` を real git に渡す前に、記録パス（`jj workspace root --name`、meta 非依存）が存在しない workspace を forget し、古いメタデータを削除する。reap は real git 自身の prune 判定と同期する — 呼び出し側オプションを転送した `--dry-run` レポート（stderr に出る）をパースし、real git が保持する workspace はスキップする。`--expire` や locked レコードの判定はここで再実装せず real git に委譲する。`--dry-run` / `-n` は reap 自体に届かず、保護対象 workspace と jj 自身の prune は除外 / 純粋な pass-through のまま。

自己報告ログ (`issues.log`) には種別 (kind) として `unknown_option` / `parse_error` / `unsupported_combination` を記録し、ts / cmd / option / argv / caller_pid / caller_cmdline を含む。AI エージェントが暗黙にフォールバックする前にユーザに気付かせる仕組み。

## 安全機構

1. **main/default workspace 保護**: `remove` で削除を拒否。prune reap でも決して forget しない
2. **未コミット変更チェック**: `remove` で `jj diff --stat` を確認（`--force` で強制）
3. **パス containment**: `remove` で対象パスが repo_root 配下かを検証
4. **bookmark 削除のスコープ制限**: メタデータに記録された名前のみ対象
5. **shim バイパス**: `JJ_WORKTREE_DISABLED=1` で即座に real git にフォールバック
6. **best effort の prune reap**: reap 失敗は stderr 警告に留まり、後続の real git prune を決して妨げない

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
just bump-version [patch|minor|major]
  ↓
ensure-clean → test → build → Cargo.toml の version 行を bump → jj describe + new → just push
  ↓
GitHub Actions (release.yml) が main の Cargo.toml 変化を検出
  ↓
6ターゲットビルド → gh release create --target <sha> --generate-notes
  ↓
tag v<X.Y.Z> / GitHub Releases ノート / homebrew-tap Formula を自動作成・更新
```

ローカルでの tag 操作を排除した経緯と CHANGELOG.md を `--generate-notes` に一任する判断は `docs/decisions/DR-0003-release-flow.md` を参照。

### インストール

```bash
brew install kawaz/tap/jj-worktree
```

## テスト

110 テスト (69 unit + 41 integration)

### ユニットテスト

- `jj.rs`: find_repo_root, build_command (4)
- `meta.rs`: save/load/remove/list/serialization (9)
- `shim.rs`: parse_git_global_opts (18), parse_branch_delete (6), parse_rev_parse_refs (9)
- `worktree.rs`: parse_add_args (16), prune_is_dry_run (1)
- `issue_log.rs`: kind 文字列、log path 上書き、JSONL エントリ、caller_is_jj (4)
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
9. drop-in パススルー: `list --porcelain`、未対応サブコマンド、`add --orphan` / `add -d` は real git へ
10. add: real git と同様に親ディレクトリを自動作成
11. prune 時の reap: 消滅済み workspace（shim 作成・meta なし双方）を forget / 生存・保護対象は残留 / `--expire` と locked レコードは real git の判定と同期 / dry-run は jj 状態を変更しない / colocated repo も reap 対象
12. jj が呼び出し元: colocated repo で jj の内部 git worktree 呼び出しは pass-through（再帰なし、詳細 issue ログ有効時も同様）、jj 自身の prune は純粋 pass-through
13. remove: jj 登録済み workspace のみ翻訳 / git 由来 worktree（`-d` / `--orphan` 生成）は real git へ（`--force` と `-f` の両方）

## リポジトリ

- パス: `~/.local/share/repos/github.com/kawaz/jj-worktree/main/`
- GitHub: https://github.com/kawaz/jj-worktree
- Homebrew: `brew install kawaz/tap/jj-worktree`
