# DR-0003: リリースフロー (Cargo.toml 変化検知 → tag 自動生成 + GitHub Release)

- ステータス: Accepted
- 日付: 2026-05-09
- 関連: docs/findings/2026-05-08-release-workflow-research.md

## 文脈

従来の `justfile::release` ターゲットは「ローカルでやりすぎ」状態だった:

- Cargo.toml の version を sed で書き換え
- Claude を呼んで `CHANGELOG.md` を自動生成（外部依存・実行時間長い・差分の品質が安定しない）
- `jj describe` + `jj new` + `jj bookmark set main -r @-` + `just push`
- GitHub Actions の workflow を `gh run watch`

一方、`.github/workflows/release.yml` は既に成熟しており:

- `on: push: branches: [main], paths: [Cargo.toml]` で Cargo.toml 変化検知
- `gh release view "v${VERSION}"` で既リリース判定 → 多重実行ガード
- `gh release create v${VERSION} --target $COMMIT_SHA --generate-notes` で **タグ自動生成 + リリースノート自動生成**
- macOS / Linux 計 6 ターゲットの matrix build
- homebrew-tap formula を自動更新

つまり「Cargo.toml を書き換えて push したら CI 側でタグ振って自動リリース」フローは既に完成している。`justfile::release` がそれと重複した二重実装になっていた。

## 決定

### 1. CHANGELOG.md は削除し、GitHub Releases ページに一任

- `gh release create --generate-notes` が commit メッセージから自動生成する
- ローカルに `CHANGELOG.md` を二重管理する意味が薄い (jj-worktree は単機能 OSS)
- README.md など他のファイルから `CHANGELOG.md` への参照は無いので副作用なし
- 過去のリリースノートを参照したいユーザは GitHub Releases ページを見ればよい

### 2. `justfile::release` → `justfile::bump-version` に改名 + 痩せさせる

port-peeker / authsock-warden 流に揃える。実装:

```just
# Cargo.toml の version を bump して Release commit を push (CI が tag + GitHub Release を作成)
bump-version bump="patch": ensure-clean test build
    #!/usr/bin/env bash
    set -euo pipefail

    # Cargo.toml の version 変更が main に push されると release.yml が検出して
    # tag (v$VERSION) と GitHub Releases (CHANGELOG 含む) を自動作成する。
    # tag を人が打つ必要はない。

    current=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    IFS='.' read -r major minor patchv <<< "$current"
    case "{{bump}}" in
        major) major=$((major + 1)); minor=0; patchv=0 ;;
        minor) minor=$((minor + 1)); patchv=0 ;;
        patch) patchv=$((patchv + 1)) ;;
        *) echo "Error: invalid bump '{{bump}}'" >&2; exit 1 ;;
    esac
    new_version="${major}.${minor}.${patchv}"
    echo "Version: ${current} -> ${new_version}"

    # @ は空 change (ensure-clean で確認済)。Cargo.toml を書き換えて Release commit に
    sed -i '' "s/^version = \"${current}\"/version = \"${new_version}\"/" Cargo.toml
    cargo check --quiet
    jj describe -m "Release v${new_version}"
    jj new

    # push (release.yml がここから走る)
    just push

    # release.yml を watch
    sleep 3
    run_id=$(gh run list --repo kawaz/jj-worktree --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')
    gh run watch "$run_id" --repo kawaz/jj-worktree
```

削除するもの:
- Claude による `CHANGELOG.md` 自動生成 (`--generate-notes` に一任)
- `jj bookmark set main -r @-` (`just push` の中で実行されるので二重に書かない)
- `cargo check --quiet` 後の `echo "Version: ${current} -> ${new_version}"` は echo として残す（人間向けの確認情報）

依存階層 (DR-0001/DR-0002 の lint→ensure-clean パターンと整合):
- `bump-version: ensure-clean test build` で lint は3経路すべてから依存され、重複排除で1回

## 不採用案

### A. release-please / semantic-release / changesets

外部ツール依存。kawaz リポジトリはどこも自前スクリプトで揃っており、外部ツール導入の利益が薄い。Conventional Commits の強制も今は不要。

### B. CHANGELOG.md を残して CI で auto-commit

CI で `--generate-notes` の出力を CHANGELOG.md に prepend して push する案。手動更新とのコンフリクトが発生しうる、リリース直後にもう1コミット飛ぶ、二重管理が解消されない、と煩雑さに見合わない。

### C. tag push トリガ (`on: push: tags: ["v*"]`)

stable-which が採用しているパターン。ローカルで `jj tag set` + `jj git push --tag` する必要があり、port-peeker / authsock-warden の Cargo.toml 変化検知の方が「ローカルではタグ操作不要」で軽い。

## 実装手順

1. `CHANGELOG.md` を削除
2. `justfile::release` を `bump-version` に置き換え (port-peeker 流)
3. INDEX.md に DR-0003 追加
4. commit + push (リリースは発生しない、Cargo.toml 変更がないため)

## 関連

- findings: docs/findings/2026-05-08-release-workflow-research.md
- 参考実装: kawaz/port-peeker, kawaz/authsock-warden
- DR-0001 (寛容な未知オプション pass-through), DR-0002 (自己報告メカニズム): bump-version の依存階層 `ensure-clean test build` は両 DR の lint→ensure-clean パターンと整合
