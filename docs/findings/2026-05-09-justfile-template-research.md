# justfile / CI / hooks テンプレート調査 (2026-05-09)

kawaz/* リポジトリ群 (約205) の justfile / .github/workflows / hooks を網羅調査し、共通テンプレ化候補と言語別差分、お宝パターンを抽出した結果。

## 判明した事実

### 中核となる設計原理

> 順序保証問題を依存トポロジーで構造的に消し、auto-fix で生じた変更は `ensure-clean: lint` 依存で捕捉する。

claude-statusline (2026-04-22) で確立されたパターン。それ以前の議論 (`justfile` 化, レシピ分解, `--deny-warn`, disable 拒否, test 内包の依存階層) はこの設計に収斂する形で展開された。port-peeker (2026-05-06) や claude-pr-monitor バックポート (2026-04-23) では、このパターンを他リポへ展開する作業が行われた。

### 共通化できるレシピ (横展開済み or 候補)

ユーザの仮説「list / push / ensure-clean / check-translations はほぼそのまま共通化できる」は **概ね Yes**。最新の Rust/TS/Go テンプレで完全一致。MoonBit は別路線 (`default: check test` 派) でも違和感なし。

| レシピ | 共通テンプレ確定形 | 採用率 |
|---|---|---|
| `default` | `@just --list` (45%) または `default: check test` (40%, MoonBit) | 高 |
| `ensure-clean` | `test "$(jj log -r @ --no-graph -T 'empty')" = "true"` | 11/35 で完全一致 |
| `push` | `push: ensure-clean check test\n    jj bookmark set main -r @-\n    jj git push --bookmark main` | Rust/TS/Go 系で完全一致 |
| `check-translations` | jj-worktree 版 (find ベース、jj/git 両対応 file_ts、`grep -qF` 固定文字列) | 共通テンプレに昇華済 (`~/.claude/rules/docs-structure.md`) |
| `lint` | `cargo fmt + cargo clippy --fix --allow-dirty --allow-staged` (Rust)、`bunx oxfmt + oxlint --fix` (TS) | 言語別 |

#### `ensure-clean: lint` の依存パターン

jj-worktree (2026-05-09) で最初に採用された依存設計。

```
push -> ensure-clean -> lint
push -> test (-> lint, 重複排除でスキップ)
```

`just` は同じレシピを自動で重複排除するので lint は1回。どの経路で ensure-clean に至っても、必ず lint 実行後に dirty 判定が動く。`cargo fmt` の auto-fix で生じた差分は確実に検出される。**条件分岐や順序明記なしに、依存トポロジーだけで順序保証問題を消している**。

### 言語別差分 (build / fmt / lint / test)

| 言語 | fmt | check (lint+型) | test | build | 備考 |
|---|---|---|---|---|---|
| **Rust** | `cargo fmt` | `cargo fmt --check && cargo clippy -- -D warnings` | `cargo test` (workspace は `--workspace`) | `cargo build --release` | workspace 系は `-p {cli}` でバイナリ指定 |
| **TS/Bun** | `bunx oxfmt` or `biome format --write` | `biome check . && tsc --noEmit` | `bun test` | `bun run build` | template-ts は biome、claude-statusline は oxfmt+oxlint |
| **MoonBit** | `moon fmt` | `moon check --deny-warn` | `moon test` (target: native/js/wasm-gc/all) | `moon build --release` | `release-check: fmt info check test` がほぼ全 .mbt 共通 |
| **Go** | `gofmt -w .` | `test -z "$(gofmt -l .)" && go vet ./...` | `go test ./...` | `go build -buildvcs=false` | `-buildvcs=false` は jj+git-bare で必須 |

### bump-version (言語固有 — ユーザの仮説通り)

骨格 (case 文での semver 増分) は5+リポジトリで完全一致。対象ファイルの抽出/書き換えだけが言語固有:

```bash
# 共通骨格
IFS='.' read -r major minor patchv <<< "$current"
case "{{bump}}" in
    major) major=$((major + 1)); minor=0; patchv=0 ;;
    minor) minor=$((minor + 1)); patchv=0 ;;
    patch) patchv=$((patchv + 1)) ;;
    *) echo "Error: invalid bump '{{bump}}'" >&2; exit 1 ;;
esac
new_version="${major}.${minor}.${patchv}"
```

| 言語 | バージョン取得 | 書き換え | 後処理 |
|---|---|---|---|
| Rust | `grep '^version' Cargo.toml \| head -1 \| sed ...` | `sed -i '' "s/..."` | `cargo check --quiet` (Cargo.lock 更新) |
| Go | `cat VERSION \| tr -d '[:space:]'` | `printf '%s\n' "${new}" > VERSION` | なし |
| TS (npm) | `npm version` | `npm version {{bump}} --no-git-tag-version` | (一発で済む) |
| TS (claude-plugin) | 自作 `bump` CLI | 3 ファイル (plugin.json, marketplace.json, package.json) 同時 | `jj split` で単一 bump コミット分離 |
| MoonBit | `jq` で moon.mod.json | `jq` で書き換え | なし |

### CI ↔ justfile の役割分担パターン

3 系統:

- **A. ローカル auto-fix / CI 直叩き** (Rust 系の主流): justfile の `lint` で auto-fix、CI は `cargo fmt --check` `cargo clippy -- -D warnings` を直接呼ぶ。`ensure-clean` で auto-fix 結果を捕捉。jj-worktree, claude-cmux-msg
- **B. CI が `just` 一発** (Bun/TS の最新): CI は `just` 1 行だけ。`default: test` の依存階層で全部回る。claude-statusline, idea-storage, template-repo
- **C. 重複型** (古い template): CI が cargo 直書き、justfile も持つが連動なし。template-rust, template-ts (旧版)

A は分担が明確で「CI の仕様」が justfile から独立して固められる。B は「CI とエディタ内チェックの完全一致」というシンプルさが魅力。

### リリースフロー

トリガファイル別の3系統。すべて `gh release view "v${VERSION}"` で既タグ衝突を冪等 skip するイディオム:

| パターン | 監視対象 | 代表 |
|---|---|---|
| Cargo.toml 変化検知 | Rust | jj-worktree, authsock-warden, template-rust |
| VERSION ファイル | Go (シンプル) | port-peeker (110行で完結、最小骨格) |
| package.json | TS / MoonBit npm | template-ts, mdp |
| tag push (`on: push: tags`) | MoonBit ライブラリ | template-moonbit |

`--generate-notes` 一任への移行が直近の方針。port-peeker は CHANGELOG をローカルで作らず GitHub の自動生成に任せる。jj-worktree は本日 (2026-05-09) DR-0003 でこれに移行済。

### hooks (Claude / lefthook / git) の活用

#### Claude hooks 三種

1. **PreToolUse (Bash) で `git push` / `jj git push` を遮断 → `just push` 強制**
   - template-rust, claude-cmux-msg, idea-storage が同一スクリプトを共有 (`/Users/kawaz/.local/share/repos/github.com/kawaz/template-rust/main/.claude/hooks/pre-push-check.sh`)
   - 正規表現 `(^|&&|;|\|\|)\s*(git\s+push|jj\s+git\s+push)\b` でコマンドセパレータ後の位置だけ検知 (コミットメッセージ内の "git push" を誤検出しない)
2. **PostToolUse (Edit|Write) で言語別 auto-format**
   - 一行 inline (template-rust): `if echo "$TOOL_INPUT" | grep -q '"file_path":.*\.rs"'; then just fmt; fi`
   - jq + 拡張子 (claude-statusline): `if jq -re '.tool_input.file_path' | grep -qE '\.[jt]sx?$'; then just; fi` (default = test なので `just` 単体でフルチェック)
   - 別スクリプト + debounce + lock (claude-cmux-msg): `/tmp/cmux-msg-build-${HASH}.lock` で連続編集中の多重実行を抑制
3. **SessionStart hook**: claude-cmux-msg が `matcher: "startup|resume|clear|compact"` で session 復元

#### lefthook (jj の pre-commit 不在を補う)

template-rust / authsock-warden / provide-defer の3件。jj は git pre-commit hook を実行しないので、`pre-push` フックで `cargo fmt --check` `clippy -D warnings` を強制。`output: [failure]` で正常時はサイレント。

`git push` 直接呼び出し用の保険として配置。Claude hook と lefthook で多層防御。

### お宝パターン (プロジェクト固有だが横展開価値あり)

1. **`check-version-bump`** (claude-plugin 系4プラグインで完全テンプレ化): `jj diff --from main@origin --to @- --summary` で「main@origin との差分があるのに version が同じ」状態を検出、エラーメッセージで `bump-version` または `push-without-bump` を提案。`push` と `push-without-bump` の二系統設計
2. **`check-bundle` / `check-versions`** (claude-cmux-msg): バンドル生成検証 + plugin.json/marketplace.json/package.json の三者一致チェック
3. **mdp ↔ claude-session-analysis のバイナリ取り込み**: `gh release view --repo kawaz/mdp` で別リポの最新タグを取り、`gh release download` で取得して bin/ に配置。CI 不要のバイナリ依存解決を justfile + gh で実現
4. **kuu.mbt の `size`**: WASM-GC/WASM/JS の生サイズ + gzip-9 + zstd-22 + brotli を 1 ターゲットでビルドして表形式比較。MoonBit ライブラリのサイズ意識
5. **mdp の polyglot shebang 注入**: `printf '#!/bin/sh\n'"'"':'"'"' //; b=$(command -v bun) && exec "$b" --bun "$0" "$@"; exec node "$0" "$@"\n'` で「bun があれば bun、なければ node」を `#!/bin/sh` 経由で実現
6. **`[confirm]` 修飾子と `_` prefix private**: grapheme.mbt 等で破壊的操作の保護
7. **release watch のデフォルト化**: port-peeker が release.yml を `gh run watch` で自動監視 (`gh run list --workflow=release.yml --limit 1`)
8. **shellcheck の動的ファイル列挙** (claude-pr-monitor): `git ls-files 'scripts/*.sh' 'hooks/*.sh'` で plugin リポの再利用テンプレに

### 連携が網羅的なリポジトリ (参考実装 5件)

1. **claude-cmux-msg**: `just push` に `check-bundle` → `check-versions` → `check-version-bump` → `check-translations` → `validate` を全部チェイン。push 後は marketplace と plugin の両方を `claude plugin update` で自己更新
2. **jj-worktree**: `ensure-clean: lint` の依存パターン、`check-translations` の find ベース動的発見、`file_ts()` の jj/git 自動切替を採用 (2026-05-09 時点での参考)
3. **port-peeker**: release.yml 110 行で「VERSION 変化 → semver 検証 → 既タグ skip → matrix build → `--generate-notes`」を完結
4. **claude-statusline**: CI は `just` 1 行、`default: test → typecheck: lint` で1コマンド全実行、Claude PostToolUse hook も同じ `just` を再利用 (CI とエディタ内チェックの完全一致)
5. **authsock-warden**: Cask/Formula 切替 (DR-013)、.app バンドル + 署名 + notarize + staple の自動化、lefthook + Claude hook を併用

## 実用的な示唆 / ベストプラクティス

### 横展開すべき共通テンプレ (`~/.claude/rules/justfile-template.md` 化候補)

```just
# デフォルト: レシピ一覧
default:
    @just --list

# format + lint (auto-fix 込み、残った警告はエラー) — 言語別に書き換え
lint:
    # Rust:    cargo fmt; cargo clippy --fix --allow-dirty --allow-staged --all-targets -- -D warnings
    # TS/Bun:  bunx oxfmt --write src/; bunx oxlint --fix --deny-warnings src/
    # Go:      gofmt -w .; go vet ./...
    # MoonBit: moon fmt; moon check --deny-warn

# test (cargo test は型チェック + ビルドも含むので Rust では lint→test で階層十分。TS は typecheck を独立)
test: lint
    # Rust:    cargo test
    # TS/Bun:  bun test
    # Go:      go test ./...
    # MoonBit: moon test

# release ビルド (言語別)
build: lint
    # Rust:    cargo build --release
    # TS/Bun:  bun run build
    # Go:      go build -buildvcs=false -o bin/{{name}} ./cmd/{{name}}
    # MoonBit: moon build --release

# ワーキングコピーがクリーン (empty change) であることを確認
# `lint` を依存に取ることで auto-fix で生じた変更を確実に検出する
ensure-clean: lint
    test "$(jj log -r @ --no-graph -T 'empty')" = "true"

# push (依存階層で lint/ensure-clean は重複排除されて1回ずつ実行)
push: ensure-clean test check-translations
    jj bookmark set main -r @-
    jj git push --bookmark main

# 翻訳ペア整合性チェック (テンプレは ~/.claude/rules/docs-structure.md 参照、find ベース言語非依存)
check-translations: ensure-clean
    # ... ~/.claude/rules/docs-structure.md からそのままコピー
```

### bump-version は言語別テンプレを別立て

骨格は共通だが対象ファイルが言語固有のため、言語別の `~/.claude/rules/bump-version-{rust,go,ts,moonbit}.md` を作るのが筋。共通骨格 (case 文) は justfile-template.md に書き、対象ファイル抽出/書き換え/後処理だけ言語別に。

### Claude hooks のテンプレ化

- `pre-push-check.sh` (PreToolUse Bash 遮断): template-rust 版がそのまま使い回せる。`~/.claude/hooks/templates/pre-push-check.sh` 化候補
- PostToolUse auto-format: jq + 拡張子マッチ版が言語非依存で書きやすい

## 検証の詳細

### 調査範囲

- `~/.local/share/repos/github.com/kawaz/` 配下 約 205 リポジトリ
- `justfile` を持つもの: 約 36 (35 リポジトリ、kuu.mbt/grapheme.mbt は workspace 複数)
- 直近 1〜2 ヶ月で更新があった justfile: 13+
- diary 内で justfile に言及した日記: 44 ファイル (2026/01/19 〜 2026/05/06)

### 時系列の主要マイルストーン

- **2026-02-26 (tui.mbt)**: 「問題を解くのではなく問題を踏まないワークフローを作る」の発想
- **2026-03-13 (claude-session-analysis)**: 「justfile はシェルスクリプトではない」40行 bash 化アンチパターンの教訓
- **2026-04-02 (authsock-warden)**: Cargo.toml 変化検知パターンの導入、Formula(Linux) + Cask(macOS) 二本立て (DR-013)
- **2026-04-22 (claude-statusline)**: ★ `ensure-clean: lint` 順序保証問題の依存トポロジー解消 ★ disable コメントの設計シグナル化 ★ 「実装上そう」と「仕様として保証」の区別
- **2026-04-23**: 6リポへのバックポート祭り、テンプレ起源バグの遺伝発見
- **2026-05-06 (port-peeker)**: 「template ではなく実プロジェクトを参考にする」、`bump-version` の VERSION ファイル駆動

### 関連ファイル (絶対パス)

主要参考実装:
- `/Users/kawaz/.local/share/repos/github.com/kawaz/jj-worktree/main/justfile` (2026-05-09 時点の参考実装)
- `/Users/kawaz/.local/share/repos/github.com/kawaz/port-peeker/main/justfile` + `release.yml` (最小骨格)
- `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-statusline/main/justfile` + `.claude/settings.json` (CI 一発型)
- `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-cmux-msg/main/justfile` + `hooks/hooks.json` (総合)
- `/Users/kawaz/.local/share/repos/github.com/kawaz/authsock-warden/main/.github/workflows/release.yml` (.app + Cask/Formula)

テンプレ候補:
- `/Users/kawaz/.local/share/repos/github.com/kawaz/template-rust/main/justfile` (lefthook + Claude hook 重装備)
- `/Users/kawaz/.local/share/repos/github.com/kawaz/template-repo/main/justfile` (`extractions/setup-just@v2` 軽量版)
- `/Users/kawaz/.local/share/repos/github.com/kawaz/template-ts/main/justfile`
- `/Users/kawaz/.local/share/repos/github.com/kawaz/template-moonbit/main/justfile`

Hook 共有スクリプト (使い回し):
- `/Users/kawaz/.local/share/repos/github.com/kawaz/template-rust/main/.claude/hooks/pre-push-check.sh`
- `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-cmux-msg/main/.claude/hooks/auto-build.sh`

### 朝の判断材料 (明日以降の作業候補)

1. **`~/.claude/rules/justfile-template.md` 新設**: 共通テンプレ (default/lint/test/build/ensure-clean/push/check-translations) を1ファイルにまとめ、言語別の中身だけプレースホルダに
2. **bump-version の言語別テンプレ作成**: `~/.claude/rules/bump-version-{rust,go,ts,moonbit}.md` または共通テンプレ内のセクション分け
3. **Claude hooks の共通テンプレ化**: `pre-push-check.sh` を `~/.claude/hooks/templates/` 配下に。PostToolUse auto-format も言語非依存テンプレ
4. **template-rust / template-ts / template-moonbit / template-claude-plugin** の justfile を新テンプレに揃える
5. **MoonBit リポ群 (kuu.mbt, timespec.mbt, time.mbt, syntree.mbt, grapheme.mbt, shimux)** の justfile を新テンプレに揃える (現状は `default: check test` 派、共通テンプレに合わせるかどうかは別判断)
6. **claude-plugin 系 (claude-plugin-jj, claude-pr-monitor, claude-cmux-msg)** で完成している `check-version-bump` / `push-without-bump` パターンを共通テンプレに昇格させるか検討
7. **`ensure-clean: lint` トリックを未採用のリポに横展開** (port-peeker, claude-cmux-msg, authsock-warden, template-* 全部)
