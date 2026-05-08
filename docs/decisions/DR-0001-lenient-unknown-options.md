# DR-0001: 未知の `git worktree` オプションは寛容に pass-through する

- ステータス: Accepted
- 日付: 2026-05-08
- 関連: docs/issue/2026-05-08-support-no-track-option.md (解決済み、削除予定)

## 文脈

jj-worktree は `git worktree` 系コマンドを `jj workspace` に翻訳する shim。実際に shim を呼ぶのは git クライアントだったり Claude Code (Agent ツール) だったりするが、**呼び出し側のバージョンアップで想定外オプションが追加されるケースは不可避**。

例: 2026-05-08 に Claude Code の `isolation: "worktree"` が `git worktree add --no-track ...` を呼ぶようになり、shim の `parse_add_args` が `unknown option: --no-track` でエラーを返す問題が顕在化した。

`git worktree add` には他にも shim が把握しておくべきフラグが多数ある:

- `--track` / `--no-track`: 新 branch のリモート追跡（jj には branch tracking 概念なし）
- `--detach`: detached HEAD（jj では空 change 相当）
- `--lock` / `--reason <reason>`: workspace lock（jj に対応概念なし）
- `--guess-remote` / `--no-guess-remote`: tracking 推定
- 将来追加されるかもしれない他のフラグ

## 決定

**未知のオプションはエラーにせず、警告ログを出した上で no-op で受け入れる**（寛容な pass-through）。

具体的に:

1. **明示的に no-op として受け入れるリスト** (`-b`/`-B` 以外)
   - `--track` / `--no-track`: jj に branch tracking 概念がないので無視
   - `--lock` / `--reason <val>`: jj workspace に lock 概念がないので無視
   - `--guess-remote` / `--no-guess-remote`: tracking 推定なので無視
   - `--detach`: 現状は no-op (将来 jj 側で空 change を作る挙動に拡張する余地を残す)
2. **未知の `--xxx` / `-x` フラグ**
   - 警告メッセージを stderr に出して受け入れる（処理を続行）
   - 値を取るかどうかは判定できないので、`--xxx=val` 形式は1引数、`--xxx` の後の非フラグ引数は値かどうか曖昧
   - **保守的な扱い**: `=` 付きは1引数として消費、それ以外は単独フラグとして扱う。値を取るオプションを誤って位置引数扱いにすると path や commit-ish を破壊するリスクがあるが、呼び出し側を壊すよりはマシ

## 不採用案

### 案A: 厳格にエラーを返す（現状の挙動）

呼び出し側が止まる。Claude Code の `isolation: "worktree"` のように shim が透過的に挟まる場面では、呼び出し側を壊すと UX が著しく悪化する。Unix philosophy 的にも厳しすぎる。

### 案B: 全オプションをホワイトリストで網羅

`git worktree add` のオプションは増え続ける可能性があり、追従しきれない。網羅できない時点でメンテナンスコストが残る。

## トレードオフ

- **デメリット**: 寛容に受け入れると、shim が呼び出し側の意図を一部失う可能性がある（例: `--detach` を no-op にすると本来 detached HEAD が期待される箇所で違う挙動になる）
- **緩和策**: DR-0002 で定める自己報告メカニズム（ローカルログ + AI 誘導 stderr）で、未知オプション遭遇を**埋もれさせない**仕組みを追加する。寛容さと可視性の両立。

## 実装

- `src/worktree.rs::parse_add_args`: 未知オプションを警告ログ + no-op で受け入れる
- 既知 no-op リストは `KNOWN_NO_OP_FLAGS` (値なし) と `KNOWN_NO_OP_VALUE_FLAGS` (`--reason` 等値あり) に分けて定義
- 未知オプション遭遇時は DR-0002 のログ機能を呼び出す

## 関連

- DR-0002: 自己報告メカニズム
- 元 issue: docs/issue/2026-05-08-support-no-track-option.md
