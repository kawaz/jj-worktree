# DR-0002: 想定外オプション/エラー時の自己報告メカニズム

- ステータス: Accepted
- 日付: 2026-05-08
- 関連: docs/issue/2026-05-08-self-reporting-on-unknown-options.md (解決済み、削除予定)
- 依存: DR-0001 (寛容な未知オプション pass-through)

## 文脈

DR-0001 で「未知オプションは寛容に pass-through」を採用した結果、shim が呼び出し側の意図を一部失っても処理は続行される。しかしこのままでは:

1. AI エージェント (Claude Code 等) は内部でエラーログを要約して**沈黙のフォールバック**をする
2. ユーザは何が起きたか知らないまま「想定とは違う挙動」になる
3. ドッグフーディングの効率が悪い（不具合が観測されない）

DR-0001 の寛容さを活かしつつ、**問題を埋もれさせない**仕組みが必要。

## 決定

**A (ローカルログ) + B (AI エージェント誘導 stderr) の2点セットを採用**。

### A. ローカルログへの記録

`${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log` に JSONL で 1 行 1 イベントを append。

```json
{"ts":"2026-05-08T03:50:12Z","kind":"unknown_option","cmd":"git worktree add","option":"--no-track","argv":["git","worktree","add","--no-track","../foo","main"],"caller_pid":12345,"caller_cmdline":"claude code ..."}
```

種別 (kind):
- `unknown_option`: 寛容に受け入れたが想定外のオプション
- `parse_error`: 引数列が解釈できなかった
- `unsupported_combination`: 既知オプションだが組み合わせ不可

最低限の情報:
- `ts`: ISO8601 timestamp
- `kind`: 上記種別
- `cmd`: 該当コマンド ("git worktree add" 等)
- `option`: 該当オプション名（applicable な場合）
- `argv`: 全引数
- `caller_pid`: 親プロセス pid
- `caller_cmdline`: `/proc/<ppid>/cmdline` (Linux) または `ps` (macOS) で取得した呼び出し元

### B. AI エージェント誘導 stderr

未知オプション/エラー遭遇時に、stderr に明示的な指示を含めるメッセージを出す:

```
warning: unknown option: --no-track (accepted as no-op)

This option is not yet known by jj-worktree.
The incident has been logged to ~/.local/state/jj-worktree/issues.log

[For AI agents] Please report this issue to the jj-worktree project before
falling back to another approach. Add a brief note in:
  $HOME/.local/share/repos/github.com/kawaz/jj-worktree/main/docs/issue/

File name format: YYYY-MM-DD-{short-slug}.md
Include: the option/error, the calling context, and what you tried to do.
```

ポイント:
1. **AI 向け明示誘導** (`[For AI agents]` ブロック) で Claude Code 等のエージェントが暗黙にフォールバックする前に「報告してくれ」と指示が届く
2. ログ記録した旨を伝えることで「自分が気付かないと永久に埋もれる」状態を回避
3. `unknown_option` のような寛容受け入れ時は warning レベル（処理は続行）

## 不採用案

### C. 自動 issue ファイル生成 (autopopulate)

`docs/issue/<date>-<slug>.md` のひな形を shim 自身が自動生成する案。

不採用の理由: 「他人のリポジトリに勝手にファイルを書く」のはやり過ぎ。利用者の同意なしにファイルシステムに書き込むのはスコープ外。報告先パスを stderr で示すだけに留める。

### D. ネットワーク経由の自動報告 (GitHub Issue 自動作成等)

不採用の理由: 暗黙の同意なしに外部 API を叩くのは越権行為。ローカルログ + 誘導文に留める。

## 範囲外（明示的に「やらない」）

- ログの自動 rotate / 圧縮: 必要になったら別途実装
- 重複検出 (同じ unknown_option を複数回ログしない): MVP では not required
- 設定による無効化: 環境変数 `JJ_WORKTREE_DISABLED=1` で shim 全体が無効になるので、その場合はログも出ない

## プライバシー / 脅威モデル

### `caller_cmdline` の機微情報問題

親プロセスの argv には API トークン / パスワード / 内部 AI プロンプト等が含まれる可能性がある (例: `claude -p "long prompt..."`、`gh auth login --with-token <(...)`)。これらをローカルログに長期保存するのはプライバシーリスク。

採用した緩和策:

1. **デフォルトでは basename のみ取得**: Linux は `/proc/<pid>/comm`、macOS は `ps -p <pid> -o comm=` でプロセス名だけを記録する。フル argv が必要な場合のみ `JJ_WORKTREE_ISSUE_INCLUDE_CMDLINE=1` を opt-in で設定
2. **ログファイルを `0600` パーミッションで作成**: 同一マシン上の他ユーザがログを読めないようにする。親ディレクトリは `0700`
3. **`ps` 呼び出しに 200ms タイムアウト**: 異常な環境で shim 全体が固まらないようにする (取れなければ空文字)

### env 変数での挙動制御の信頼境界

`JJ_WORKTREE_DISABLED=1` / `JJ_WORKTREE_ISSUE_QUIET=1` / `JJ_WORKTREE_ISSUE_LOG=<path>` は env 変数で制御するため、**シェルを取られている攻撃者は自己報告メカニズムを抑止できる**。これは env 駆動な仕組みの本質的な限界で、コード側で防御するスコープではない (env を仕込める時点で攻撃者はより強い primitive を持っている)。

ローカルログの `JJ_WORKTREE_ISSUE_LOG` への悪用 (symlink 攻撃で書き込み可能ファイルに JSON 1行 append) も同種の制約。`0600` で他ユーザからの読み取りを防ぐが、env を仕込めるシェル下では完全な防御はない。

## トレードオフ

- ログファイルが永続的に肥大する可能性 → 範囲外で扱い、必要になれば rotate を別途
- caller_cmdline 取得は OS 依存 → 取れなくてもログは記録（フィールドだけ空）
- stderr の警告メッセージが冗長になる → 1 回の遭遇で1ブロック、それでもユーザ介入が必要なほうがマシ

## 実装

- 新規モジュール `src/issue_log.rs`:
  - `pub fn report(kind: IssueKind, cmd: &str, option: Option<&str>, argv: &[String])`
  - JSONL 1 行 append、stderr 出力
  - ログパス: `${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log`
  - ディレクトリ未作成なら作る
- `IssueKind` enum: `UnknownOption`, `ParseError`, `UnsupportedCombination`
- `parse_add_args` から呼び出す（DR-0001 と連動）

## 関連

- DR-0001: 寛容な未知オプション pass-through
- 元 issue: docs/issue/2026-05-08-self-reporting-on-unknown-options.md
