# Claude Code `EnterWorktree` が作る workspace に meta が残らず、`git worktree list/remove` が path を取り違える

- status: open
- 発見: 2026-06-17 (kawaz/cache-warden での dogfood 中、別件作業の cleanup フェーズで観測)
- 報告者: 部外者セッション (cache-warden 側で本 issue に該当する事象を観測)

## 現象

Claude Code `EnterWorktree(name: "fix-launchd-discovery-block")` で worktree を作って commit/push まで完走 (= jj-worktree shim 経由で git worktree add 等の翻訳が裏で走ったはず)、最後に `ExitWorktree(action: "remove", discard_changes: true)` を呼んだら以下の応答:

```
Exited worktree but could not remove it — kept at
/Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/.claude/worktrees/fix-launchd-discovery-block
```

= 「session の cwd は呼出元 (`cache-warden/main`) に戻したが、worktree dir は消せなかった」。残置 dir のフルパスは `cache-warden/.claude/worktrees/fix-launchd-discovery-block` (= `main/` の **外側**)。

その時点での状態:

```
$ git worktree list      # jj-worktree shim 経由
/Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/main                             cb03051 [default]
/Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/main/fix-launchd-discovery-block d19a6f5 [fix-launchd-discovery-block]
/Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/main/main                        1f6ce96 [main]

$ jj workspace list
default: psxyrnyw cb030518 (no description set)
fix-launchd-discovery-block: mtmyvkpx d19a6f50 (empty) (no description set)
main: lokptmuy 1f6ce969 (empty) (no description set)
```

注目: `git worktree list` (= `src/worktree.rs:237 cmd_list`) は `fix-launchd-discovery-block` の path を **`cache-warden/main/fix-launchd-discovery-block`** と報告。しかし **実 dir は `cache-warden/.claude/worktrees/fix-launchd-discovery-block`** (= `main/` の外)。報告 path は実在しない。

## 推定原因

`src/meta.rs:24` の `meta_dir(repo_root) = "<repo_root>/.jj-worktree-meta"` を確認したところ、cache-warden リポにこのディレクトリ自体が存在しない:

```
$ ls -la /Users/kawaz/.local/share/repos/github.com/kawaz/cache-warden/main/.jj-worktree-meta/
ls: ... No such file or directory
```

= **CC が EnterWorktree で作った workspace に対して `meta::save` が呼ばれていない** (= shim の `cmd_add` を経由していない、もしくは経由したが meta save パスを通っていない)。

その帰結として、`src/worktree.rs:317 get_workspace_path` の挙動:

```
// line 322-323: メタデータが見つからない (今回ここを抜ける)
if let Some(ws_meta) = meta::load(...) { return Ok(ws_meta.path); }

// line 332-: フォールバック = repo_root/ws_name を返す
let candidate = repo_root.join(ws_name);
```

→ `repo_root` (= `cache-warden/main`) + `ws_name` (= `fix-launchd-discovery-block`) = `cache-warden/main/fix-launchd-discovery-block` という **実在しない path** が報告される。

これが `cmd_remove` (line 343) の `find_workspace_name(target_path)` (line 464) で:
- target = 実 dir パス `.claude/worktrees/fix-launchd-discovery-block` (canonical)
- 候補 = `repo_root/ws_name` (canonicalize に失敗 → そのまま)
- → 一致せず、`no matching workspace` でエラー終了

結果として CC の `ExitWorktree(remove)` は「dir を消せない」とだけ返し、jj workspace の forget も dir の rm も走らない。

## 再現コマンド (Claude Code から)

```
EnterWorktree(name: "test-meta-bug")          # cache-warden 等の jj リポで実行
# → .claude/worktrees/test-meta-bug が作られる
# → jj workspace list には test-meta-bug が現れる
# → でも .jj-worktree-meta/test-meta-bug.json は作られない (本 issue の根)
ExitWorktree(action: "remove", discard_changes: true)
# → "could not remove" で dir 残置
```

## ワークアラウンド (今回採用)

cwd 側から手動で:

```bash
jj workspace forget fix-launchd-discovery-block   # 名前で forget = OK
rm -rf .claude/worktrees/fix-launchd-discovery-block  # ← 安全 hook が止めるかも
jj bookmark delete worktree-fix-launchd-discovery-block
```

`jj workspace forget` は **名前** を取るので path 解決不要 → 動く。dir 削除は cc 側の安全 hook で blockable (= 別の事故源)。

## 改善方向 (= 当事者判断、フラグ止まり)

部外者観点で見える論点を並べるだけ。実装にどう適用するかは jj-worktree 側で判断:

1. **meta 不在ケースで `get_workspace_path` のフォールバック経路を強化**: 現状 `repo_root/ws_name` 決め打ちだが、jj には workspace の path を直接問い合わせる経路がある可能性。たとえば `jj workspace root --workspace <name>` 等。一次資料は jj の `workspace` サブコマンド help (`jj workspace --help`)。なお `jj root` は global、workspace 毎の path 取得 API があるかどうかは jj 側の確認が必要 (= 部外者の自分は確認していない)
2. **shim 経由でない workspace 追加を検知する手段**: cache-warden の `.claude/worktrees/` のように shim の `cmd_add` を経由しない経路で workspace が作られたケースを **`cmd_list` の初回呼び出し時点で検出して meta を自動 backfill** する案。jj workspace list と meta dir の差分から不足分が分かる
3. **CC 側ヘの上流 issue 検討**: Claude Code の `EnterWorktree` が jj-worktree shim 経由ではなく独自に jj 直叩きしている可能性もある (= shim を bypass する別経路で workspace 作成)。CC 側に「jj 環境では shim 経由で worktree add せよ」と要請する経路もある (これは私の観測範囲外、jj-worktree 側の責務として「meta 不在を救う」のと、CC 側の責務として「shim 経由でやる」のとどちらが筋良いかは kawaz 判断)

## 関連

- `kawaz/cache-warden` の本件発火セッション (= cache-warden v0.22.1 land + cleanup フェーズ)。journal `cache-warden/docs/journal/2026-06-17-cw-discovery-block-incident.md` の終盤
- README.md "Translated commands" 表で `git worktree remove <path>` → `jj workspace forget` + cleanup と謳ってる挙動が、CC 経由作成の workspace で破綻する
- 上流 (Claude Code) 側で `EnterWorktree` 実装が shim を bypass しているかは未検証 (部外者観点)
