# リリースワークフロー調査 (2026-05-08)

明日 (2026-05-09) 以降に続ける作業の判断材料。kawaz リポジトリ群の release/version パターンを網羅調査した結果をまとめる。

## 判明した事実

### CI 側 (release.yml) は既に良い形

**jj-worktree の `.github/workflows/release.yml` は既に「Cargo.toml 変化検知 → check-version → build matrix → publish-release → update-homebrew」というパターン A 実装が入っている**。authsock-warden と同等の完成度で、これ自体は変更不要。

具体的に既に動いている仕組み:
- `on: push: branches: [main], paths: [Cargo.toml]` で Cargo.toml 変化時のみ起動
- `gh release view "v${VERSION}"` で既リリース検知 → 多重実行ガード
- `gh release create v${VERSION} --target $COMMIT_SHA --generate-notes` で **タグも自動生成 + リリースノートも自動生成**
- macOS / Linux x6 ターゲットで matrix build
- homebrew-tap formula を自動更新

つまりユーザの言う「Cargo.toml を bump して push したら CI 側でタグ振って自動リリース」は **既に動いている**。

### 問題は justfile 側だけ

現状の `justfile::release` ターゲットは「ローカルでやりすぎ」状態:

```just
release bump="patch": ensure-clean check test build
    # 1. Cargo.toml 書き換え (sed)
    # 2. CHANGELOG.md update via Claude  ← 重い、不要
    # 3. jj describe + jj new + jj bookmark set + just push
    # 4. sleep 3 + gh run watch
```

`release.yml` 側で `--generate-notes` が動いている以上、**ローカルで Claude を呼んで CHANGELOG を書き換えるのは二重作業**。

### 採用パターンの分布 (kawaz/* 直近1ヶ月)

調査対象: `~/.local/share/repos/github.com/kawaz/*` 約205リポジトリ → `justfile` または `.github/workflows/` を持つもの 47 → 直近1ヶ月以内更新 9リポジトリ/11ファイル。

| パターン | 代表リポジトリ | トリガ | 言語 |
|---|---|---|---|
| **A. version ファイル変化検知** | `port-peeker`, `authsock-warden`, `jj-worktree`(現状) | `on: push: paths: [Cargo.toml or VERSION]` | Rust / Go |
| **B. tag push 検知** | `stable-which` | `on: push: tags: ["v*"]` | Rust |

`release-please` / `semantic-release` / `changesets` など外部ソリューションは **誰も使っていない**。全部自前スクリプトで揃っている。

### ベストプラクティス: port-peeker

`port-peeker` (Go) と `authsock-warden` (Rust) が同一思想で組まれており完成度が高い。特に **port-peeker の justfile が痩せていて参考になる**:

```just
bump-version bump="patch": ensure-clean check test
    # 1. VERSION ファイル (or Cargo.toml) を bump するだけ
    # 2. jj describe -m "Release v${new}" && jj new && just push
    # ↑ それだけ。タグ操作なし、CHANGELOG生成なし
```

`gh release create v${VERSION} --target $COMMIT_SHA --generate-notes` がタグも CHANGELOG もまとめて作るので、ローカルは「version 行を1行書き換えて push」するだけで済む。

## 実用的な示唆 / 適用提案

### jj-worktree への適用

**最小変更で port-peeker 流に痩せさせる**だけで OK。`release.yml` は触らない。

#### 1. justfile の `release` を痩せさせる

新 `bump-version` (port-peeker と命名揃え):

```just
bump-version bump="patch": ensure-clean check test
    #!/usr/bin/env bash
    set -euo pipefail
    current=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    IFS='.' read -r M m p <<< "$current"
    case "{{bump}}" in
        major) M=$((M+1)); m=0; p=0 ;;
        minor) m=$((m+1)); p=0 ;;
        patch) p=$((p+1)) ;;
        *) echo "Error: Invalid bump type '{{bump}}'" >&2; exit 1 ;;
    esac
    new="${M}.${m}.${p}"
    sed -i '' "s/^version = \"${current}\"/version = \"${new}\"/" Cargo.toml
    cargo check --quiet
    jj describe -m "Release v${new}"
    jj new
    just push
    sleep 3
    run_id=$(gh run list --repo kawaz/jj-worktree --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')
    gh run watch "$run_id" --repo kawaz/jj-worktree
```

削除するもの:
- Claude による CHANGELOG.md 自動生成 (release.yml の `--generate-notes` に一任)
- ローカルでの jj tag set / git push tag (release.yml が `gh release create --target` でタグも作る)
- `jj bookmark set main -r @-` (`just push` の前に bookmark を再設定する処理は workflow 全体の都合次第)

#### 2. CHANGELOG.md の扱い

選択肢:
- **A) CHANGELOG.md ごと削除**: GitHub Releases ページが事実上の CHANGELOG。リポジトリ内に重複して持たない。
- **B) CHANGELOG.md は残すが手動更新**: 別レシピ `update-changelog`（手動、リリース直前に Claude で生成）として分離。`bump-version` のクリティカルパスからは外す。
- **C) release.yml に CHANGELOG.md 自動更新ジョブ追加**: `--generate-notes` の出力を CHANGELOG.md に prepend して autocommit。

A が最もミニマル。kawaz の他リポジトリで CHANGELOG.md を維持しているリポジトリがどれくらいあるかは未調査。port-peeker は CHANGELOG.md を持っていない (要再確認)。

#### 3. 注意点

- `paths: [Cargo.toml]` トリガは `Cargo.lock` 単独変更では発火しないが、`Cargo.toml` を一緒に touch すると発火する → version 行の `gh release view "v${V}" 2>/dev/null` skip 分岐で多重実行ガード済み (jj-worktree の release.yml にも実装済)
- 現状の `update-homebrew` ジョブは安定パスで symlink 切り替えする方式。formula バージョンが上がるごとに homebrew-tap が更新される
- macOS 署名/notarize は authsock-warden の release.yml が一番完成度高い (jj-worktree は今のところ不要)

### 関連: 共通 justfile の check-translations パス問題

調査と無関係だが、justfile を共通化するなら `docs/DESIGN-ja.md` ハードコードを `find . -name '*-ja.md'` ベースに置き換える必要がある。docs-structure.md ルール側で「`DESIGN.md` `STRUCTURE.md` 等は `docs/` 配下、`README.md` だけリポジトリ直下」と固めてから justfile を直すのが筋。

なお check-translations の timestamp 比較は jj 管理リポジトリでは `jj log committer.timestamp().format("%s")` がネイティブ。docs-structure.md の表現は「stat mtime ではなく jj/git log を使う」(jj 管理なら jj log、git 管理なら git log) が正しい。

## 検証の詳細

### 調査範囲

- ベース: `~/.local/share/repos/github.com/kawaz/*`
- 約205リポジトリ
- うち `justfile` または `.github/workflows/` を持つもの: 47 (main-layout 24 + direct-layout 23)
- 直近1ヶ月以内 (2026-04-08 以降) に更新: 9リポジトリ / 11ファイル
  - `claude-pr-monitor`, `claude-statusline`, `claude-cmux-msg`, `stable-which`, `authsock-warden` (release.yml), `idea-storage`, `claude-plugin-jj`, `mdp`, `dotfiles`
  - jj 化で git ref が無いため mtime 確認: `port-peeker` (release.yml が 2026-05-07 更新), `template-rust` (2026-04-03 で枠外だが基準として参照)

### 関連ファイル (絶対パス)

| ファイル | 用途 |
|---|---|
| `~/.local/share/repos/github.com/kawaz/port-peeker/main/.github/workflows/release.yml` | パターンA 最小骨格 |
| `~/.local/share/repos/github.com/kawaz/port-peeker/main/justfile` | `bump-version` ターゲット (痩せた版) |
| `~/.local/share/repos/github.com/kawaz/authsock-warden/main/.github/workflows/release.yml` | Rust + Cask 連携実装 |
| `~/.local/share/repos/github.com/kawaz/authsock-warden/main/justfile` | 現 jj-worktree と同じ「太い」release ターゲット (反面教師) |
| `~/.local/share/repos/github.com/kawaz/stable-which/main/.github/workflows/release.yml` | tag push 駆動 + crates.io publish の参考 |
| `~/.local/share/repos/github.com/kawaz/jj-worktree/main/.github/workflows/release.yml` | 既に良い形 (変更不要、255行) |
| `~/.local/share/repos/github.com/kawaz/jj-worktree/main/justfile` | port-peeker 流に痩せさせる対象 |

### 追記 (2026-05-09): 結論

本調査を踏まえ、リリースフローの方針は **DR-0003 (Cargo.toml 変化検知 → tag 自動生成 + GitHub Release)** として確定した。CHANGELOG.md は削除し `gh release create --generate-notes` に一任、`justfile::release` は port-peeker 流の `bump-version` に痩せさせた。詳細は `docs/decisions/DR-0003-release-flow.md` を参照。

共通 justfile (check-translations) のパス問題は、docs-structure ルールを「`README.md` だけリポジトリ直下、`DESIGN.md` 等は `docs/` 配下」に確定したうえで、`find . -name '*-ja.md'` ベースの言語非依存テンプレに置き換え済 (詳細は `docs/findings/2026-05-09-justfile-template-research.md`)。

### サブエージェント呼び出し記録

調査エージェント `ac25ed242b83c2cb5` が `~/.local/share/repos/github.com/kawaz/` 配下を網羅的に walk し、`justfile` `*.yml` を抽出 → mtime / commit timestamp で直近1ヶ月絞り込み → release/version パターンを grep → port-peeker と authsock-warden を最良として抽出 → jj-worktree の現状と差分を提示。
