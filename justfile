# jj-worktree

# デフォルト: レシピ一覧
default:
    @just --list

# format + lint (auto-fix 込み、残った警告はエラー)
lint:
    cargo fmt
    cargo clippy --fix --allow-dirty --allow-staged --all-targets -- -D warnings

# テスト (cargo test は内部で型チェック + ビルドも回るので Rust では lint → test で階層十分)
test: lint
    cargo test

# release ビルド
build: lint
    cargo build --release

# ビルドして実行
run *ARGS: build
    ./target/release/jj-worktree {{ARGS}}

# ワーキングコピーがクリーン (empty change) であることを確認
# `lint` を依存に取ることで、auto-fix で生じた変更を確実に検出する
# (just は依存重複を排除するので lint は1回だけ走る)
ensure-clean: lint
    test "$(jj log -r @ --no-graph -T 'empty')" = "true"

# push (依存階層で lint/ensure-clean は重複排除されて1回ずつ実行)
push: ensure-clean test check-translations
    jj bookmark set main -r @-
    jj git push

# 翻訳ペア (*-ja.md / *.md) の整合性チェック
# テンプレ: ~/.claude/rules/docs-structure.md の「check-translations の実装」セクション
# - リポジトリ内のすべての *-ja.md を発見
# - 対応する *.md の存在 + 相互リンク + commit timestamp 順序を検証
check-translations: ensure-clean
    #!/usr/bin/env bash
    set -euo pipefail
    die() { echo "$*" >&2; exit 1; }

    # commit timestamp 取得 (jj 管理リポジトリなら jj log、それ以外は git log)
    file_ts() {
        local f="$1"
        if [ -d .jj ]; then
            jj log --no-graph -T 'committer.timestamp().format("%s")' \
                -r "latest(::@ & files('$f'))" 2>/dev/null || echo 0
        else
            git log -1 --format=%ct -- "$f" 2>/dev/null || echo 0
        fi
    }

    while IFS= read -r ja; do
        en="${ja/-ja/}"
        [ -f "$en" ] || die "ERROR: $ja exists but $en is missing"
        # 相互リンク (先頭 5 行内、固定文字列で正確に検出)
        head -5 "$ja" | grep -qF "> [English](./${en##*/}) | 日本語" \
            || die "ERROR: $ja: missing '> [English](./${en##*/}) | 日本語' link near the top"
        head -5 "$en" | grep -qF "> English | [日本語](./${ja##*/})" \
            || die "ERROR: $en: missing '> English | [日本語](./${ja##*/})' link near the top"
        # ja のほうが新しい (= en の翻訳が遅れている) ことを検出
        ja_ts=$(file_ts "$ja")
        en_ts=$(file_ts "$en")
        [ "$ja_ts" -le "$en_ts" ] \
            || die "ERROR: $ja was updated after $en. Update the English translation before pushing."
    done < <(find . -name '*-ja.md' -not -path './.git/*' -not -path './.jj/*')

# Cargo.toml の version を bump して Release commit を push (CI が tag + GitHub Release を作成)
# 詳細: docs/decisions/DR-0003-release-flow.md, docs/findings/2026-05-08-release-workflow-research.md
bump-version bump="patch": ensure-clean test build
    #!/usr/bin/env bash
    set -euo pipefail

    # Cargo.toml の version 変更が main に push されると release.yml が検出して
    # tag (v$VERSION) と GitHub Releases (リリースノート --generate-notes 含む) を
    # 自動作成する。tag を人が打つ必要はない。

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
    # sed の -i は BSD と GNU で構文が違う。`-i.bak` ＋ rm で両 OS 互換にする。
    sed -i.bak "s/^version = \"${current}\"/version = \"${new_version}\"/" Cargo.toml
    rm -f Cargo.toml.bak
    cargo check --quiet  # Cargo.lock を新 version で更新
    jj describe -m "Release v${new_version}"
    jj new

    # push (release.yml がここから走る)
    just push

    # release.yml を watch
    sleep 3
    run_id=$(gh run list --repo kawaz/jj-worktree --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')
    gh run watch "$run_id" --repo kawaz/jj-worktree
