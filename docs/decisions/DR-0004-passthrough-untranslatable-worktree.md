# DR-0004: 翻訳できない `git worktree` 呼び出しは real git へ pass-through する

- ステータス: Proposed
- 日付: 2026-10-14
- 関連: DR-0001 (寛容な未知オプション pass-through)、DR-0002 (自己報告メカニズム)
- 発端: Kilo Code (VS Code) Agent Manager を shim を system-wide な `git` として
  常駐させた環境 (NixOS module) で運用した際の 3 破綻観測

## 文脈

README の前提は「shim は `jj-worktree run` 経由で呼び出し側ツールにだけ有効化される」。
しかし Kilo Code Agent Manager のように、shim を `git` として PATH 全体に常駐させる
運用が存在する (NixOS module が environment PATH と stable symlink で提供)。この場合
*全プロセス* の *全 git 呼び出し* が shim に着地し、次の 3 破綻が観測された
(jj 0.46.0 / git 2.55.0 / Linux):

1. **無限再帰**: colocated repo で jj 自身が内部起動する
   `git worktree add --orphan -b jj-worktree-<hash> ...` が shim で再翻訳され
   `jj workspace add` → git → ... となり作成が必ず失敗する。
2. **未対応サブコマンド/オプションで死亡**: `list --porcelain` は
   "list takes no options"、`prune` / `move` / `repair` / `lock` は
   "unknown worktree subcommand" で失敗する。git クライアントは porcelain 出力と
   prune によるクリーンアップに依存している。
3. **workspace 残留**: Agent Manager の削除フローは「ディレクトリを rename 退避 →
   `git worktree prune --expire now`」。real git の prune は git 側の管理情報しか
   消さないため、colocated jj workspace レコードが `jj workspace forget` 手動実行
   まで残留する (= docs/issue/2026-06-18-drop-jj-worktree-meta-redundant.md の
   残留症状と同種)。

## 決定

**shim の契約を「drop-in な `git`」と明文化し、完全に再現できる操作だけ翻訳する。**
翻訳できないものはすべて real git に渡す:

1. **翻訳対象**: `--detach` / `-d` / `--orphan` を含まない `add`、jj 登録済み
   workspace への `remove`、オプションなし `list` (従来どおり)。
2. **pass-through 対象**: `prune`、`move`、`repair`、`lock`、
   `list --porcelain` 等のオプション付き形式、`add --detach` / `-d` / `--orphan`、
   git 由来 worktree (pass-through で作成されたもの) への `remove`。
   colocated workspace は本物の git worktree なので real git の応答が常に正で、
   no-op による意図の損失も "unknown subcommand" による死亡も起きない。
   git 由来 worktree の remove を翻訳すると "no workspace found for path" で
   失敗して worktree が残留するため、real git 自身の安全機構
   (未コミット変更、locked、メインワークスペース保護) に委ねる。
3. **jj 由来呼び出しのガード** (`issue_log::caller_is_jj`): 親プロセス名が `jj`
   なら全 `git worktree` 呼び出しを無条件 pass-through する。jj は自ワークスペースの
   バッキング git worktree を管理しており、その内部呼び出しを翻訳すると再帰する。
   判定はプロセス名ベースで、詳細ログ (`JJ_WORKTREE_ISSUE_INCLUDE_CMDLINE`) の
   有無に依存しない。
4. **prune 時の forgotten workspace reap** (`worktree::reap_forgotten_workspaces`):
   呼び出し側起点の `prune` (親 ≠ jj) を real git に渡す前に、
   `jj workspace list` で workspace を列挙し、`jj workspace root --name <ws>` で
   解決したパスが存在しない非保護 workspace を `jj workspace forget` する。
   パス解決を jj 標準 API で行うため `.jj-worktree-meta` 非依存であり、
   shim を経由せず作られた workspace も対象になる
   (= 2026-06-18 issue の方向性と整合、廃止を先取りしない)。見つかった古い
   `.jj-worktree-meta/<ws>.json` は best effort で削除する。
   reap は real git 自身の prune 判定と同期する: 呼び出し側オプションを転送した
   `worktree prune --dry-run` のレポート (stderr に出る) をパースし、
   real git が保持するレコード (`--expire` 未到達、locked) を持つ workspace は
   スキップする。`-n` / `--dry-run` 付きの prune は reap 自体を実行しない。

不採用案:

### 案A: 現状維持 (未対応サブコマンドは厳格にエラー)

呼び出し側が死亡する。DR-0001 と同一の理由で不採用 — shim が透過的に挟まる場面では
呼び出し側を壊すと UX が著しく悪化する。

### 案B: 全サブコマンドを翻訳して jj で模倣する

`prune` / `move` / `repair` / porcelain 出力等を自前実装する案。翻訳の忠実度が
本質的に劣る (porcelain 形式の完全再現、`--expire` の意味論、ロック等) 上、
colocated workspace は既に本物の git worktree なので real git の応答で十分。
保守コストだけ増えるため不採用。

## トレードオフ

- **reap の猶予期間は real git に従う**: 独自の猶予期間は導入せず、
  `--expire` と lock の判定は real git 自身の `--dry-run` 判定に委譲する。
  その上で、ディレクトリを *移動* しただけの workspace も real git が
  prune する条件を満たせば reap 対象になる (jj には lock 概念がなく
  pin できない)。ドキュメントで明示する。
- **reap が `jj workspace root` を workspace 数分呼ぶ**: 呼び出し側起点の prune 時
  のみで、jj 由来の prune では発生しない。加えて prune 1 回につき
  real git の dry-run を 1 回実行する。
- **`caller_is_jj` は親プロセス名依存**: DR-0002 と同じ OS 依存の取得方法で、
  取得失敗時は「jj ではない」側に倒す (翻訳される)。jj の内部呼び出しを取りこぼした
  場合は再帰が再発しうるが、これは保守的なフォールバックである。
- **pass-through では DR-0002 の自己報告が発火しない**: real git が正しく応答する
  ので「埋もれる問題」は存在しない。翻訳対象 (`add` の未知フラグ) については
  DR-0001 / DR-0002 が引き続き支配する。

## 実装

- `src/issue_log.rs`: `caller_is_jj()` (親プロセス名ベース、
  `caller_cmdline` / 詳細ログ設定に非依存)
- `src/shim.rs`: worktree arm の translate 判定 (`add` は `--detach` / `-d` /
  `--orphan` で翻訳除外、`remove` は jj 登録済み workspace のみ翻訳)、
  jj 由来呼び出しの pass-through、prune 直前の reap 呼び出し
  (`-n` / `--dry-run` では実行しない)
- `src/worktree.rs`: `reap_forgotten_workspaces()` (best effort、保護対象除外)、
  `git_prune_view()` / `worktree_path_for_record()` (real git の dry-run
  レポートと管理レコードの照合)、
  `cmd_add` の親ディレクトリ自動作成 (real git と同じ挙動;
  `jj workspace add` は親ディレクトリを要求するため、
  `.kilo/worktrees/<name>` のようなネストしたパスで失敗していた)
- テスト: `tests/integration.rs` に pass-through / reap / jj ガード /
  git 由来 remove / prune 同期 を追加 (統合 41 件)

## 関連

- DR-0001: 寛容な未知オプション pass-through (翻訳対象内の挙動は不変)
- DR-0002: 自己報告メカニズム (`caller_cmdline` の再利用)
- docs/issue/2026-06-18-drop-jj-worktree-meta-redundant.md
  (reap は meta 非依存で当該 issue の残留症状を解消; meta 廃止自体は別件)
