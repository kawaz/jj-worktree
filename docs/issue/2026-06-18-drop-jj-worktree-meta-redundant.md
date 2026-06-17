# `.jj-worktree-meta/` は不要 — jj 標準 API で完全代替可能

- status: open
- 発見: 2026-06-18 (前 issue 2026-06-17-cc-enter-worktree-no-meta.md の差し戻し再起票)
- 経緯: 2026-06-17 に cache-warden 側で「meta 不在で path 解決が破綻する」事象を観測 → 同名の前 issue を「meta auto-backfill」方向で起票してしまったが、kawaz の指摘で jj 自身が workspace path を返す API を提供している実機証拠が判明。**meta system 自体が redundant** が正しい問題提起。前 issue は本 issue で supersede、既存の方は本 commit で削除

## 観測 (前 issue から踏襲)

Claude Code `EnterWorktree(name: "X")` 経由で作られた workspace に
`<repo_root>/.jj-worktree-meta/X.json` が一切作られず、その状態で `ExitWorktree(remove)` を呼ぶと `git worktree remove` の shim 経由 (`cmd_remove`) で path 逆引きに失敗、dir 残置 + bookmark 残置となる。

詳細な観測ログは cache-warden 側 journal `docs/journal/2026-06-17-cw-discovery-block-incident.md` の終盤参照 (= 別 repo)。

## 真の問題: meta system 自体の設計

`.jj-worktree-meta/<ws>.json` は 2 用途:

| 用途 | 現実装 | 実は jj 側に既に手段がある |
|---|---|---|
| (1) ws path の記録 | `cmd_list` / `cmd_remove::find_workspace_name` が `meta.path` を引く (`src/worktree.rs:322` `get_workspace_path` line 317-336) | **`jj workspace root --name <ws>`** で abs path 取得 / **WorkspaceRef.root()** が template から `.root()` メソッドで abs path を返す |
| (2) ws ↔ bookmark の紐付け | `cmd_add` で `meta.bookmark = parsed.branch` を保存、`cmd_remove` で参照して同名 bookmark を delete | **ws 名と bookmark 名を同名 convention で揃える**だけで十分。`cmd_add` で `-b <branch>` 指定があればそれ、無ければ `ws_name` 自体を bookmark 名に使えば、`cmd_remove` 時は ws 名と同名の bookmark を消すだけ |

= **meta は jj の機能不足を埋めるための代替実装と思い込んでいたが、実際は jj 側に正規 API が揃っており、shim が自前で簿冊を持つ必要がない**。

## 実機実証 (jj 2026 時点)

```
$ jj workspace root --help
Show the workspace root directory
Usage: jj workspace root [OPTIONS]
Options:
      --name <NAME>          Name of the workspace (defaults to current)

$ jj workspace root --name main
/Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/main

$ jj workspace list -T 'name ++ " -> " ++ root ++ "\n"'
default -> /Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden
main -> /Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/main
```

一次資料: `jj help -k templates` の `WorkspaceRef` 型:

```
### `WorkspaceRef` type

* `.name() -> RefSymbol`: Returns the workspace name as a symbol.
* `.target() -> Commit`: Returns the working-copy commit of this workspace.
* `.root() -> Template`: Returns the absolute path to the workspace root.
```

## 改善方向 (= 当事者判断、フラグ止まり)

部外者観点で見える論点を並べるだけ。実装にどう適用するかは jj-worktree 側で判断:

1. **`get_workspace_path` を `jj workspace root --name <ws>` 直叩きに置き換え**
   (`src/worktree.rs:317-336`)。meta::load の経路 + フォールバック `repo_root.join(ws_name)` を撤去。これで `cmd_list` は jj に問い合わせるだけ、`cmd_remove::find_workspace_name` も jj 経由で path 取得 + 比較 OK
2. **`cmd_add` の bookmark 紐付けを「ws 名 = bookmark 名」convention に変更**
   ``-b <branch>`` の明示指定が無い場合は `ws_name` 自体を bookmark 名にする。指定があればそれを優先 (= 名前は ws と異なっても OK、shim は ws 名と同名 bookmark を *無ければ作る* というだけ)。`cmd_remove` も「ws 名と同名 bookmark を delete」のシンプルロジックに統一できる
3. **`src/meta.rs` まるごと削除 + `META_DIR` 撤去**
   - 既存 `.jj-worktree-meta/` ディレクトリは廃止扱い、起動時に存在しても無視で OK (= migration cost ゼロ、自然消滅)
   - tests/integration からも meta 依存テストを削除
4. **(b) のため `-b` 明示指定があった場合の挙動を README で明文化**
   `git worktree remove` 時に「ws 名と同名 bookmark しか自動削除しない」とドキュメント。ユーザが `-b custom-branch` で作った場合は `git branch -d custom-branch` を別途叩く責任を負う、と明示。これにより shim が永続簿冊を持つ必要が完全に消える

## 副次効果

- cache-warden で起きた本件の dir 残置事象は **構造的に消滅**。CC `EnterWorktree` が shim 経由でなく jj 直叩きで ws を作ろうが、jj 自身が path を返すので `cmd_list` も `cmd_remove` も成立する
- migration: 既存 `.jj-worktree-meta/` を持つ repo は何もしなくても新版で動く (= meta::load の経路を全部消すだけ、`.jj-worktree-meta/` 自体は読まれなくなるので無視で OK)
- DR 候補: 「meta system 廃止 / jj API 直接利用」を DR-0004 (or 次番号) として残す価値があるレベルの設計判断 (= shim の独自簿冊放棄)

## 関連

- 削除 (本 commit): `docs/issue/2026-06-17-cc-enter-worktree-no-meta.md` (= 焦点ズレ版)
- 観測元: `kawaz/cache-warden` `docs/journal/2026-06-17-cw-discovery-block-incident.md`
- 命名 convention 上は kawaz の DR 起票運用に乗せる方が筋良い (= 設計判断の本丸)
