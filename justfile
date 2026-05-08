# jj-worktree

# デフォルト: レシピ一覧
default:
    @just --list

# ビルド (release)
build:
    cargo build --release

# テスト (env var を共有するテストがあるため serial 実行)
test:
    cargo test -- --test-threads=1

# lint + format チェック
check:
    cargo fmt --check
    cargo clippy -- -D warnings

# 翻訳ペアの整合性チェック (タイトル直下の相互リンク + 更新タイミング)
check-translations:
    #!/usr/bin/env bash
    set -euo pipefail
    fail=0
    for ja in README-ja.md DESIGN-ja.md MANUAL-ja.md; do
        en="${ja/-ja/}"
        # ja が存在しなければそのペアはスキップ (MANUAL は任意)
        [ -f "$ja" ] || continue
        if [ ! -f "$en" ]; then
            echo "ERROR: $ja exists but $en is missing" >&2
            fail=1
            continue
        fi
        # 相互リンク (先頭5行内)
        if ! head -5 "$ja" | grep -qE '\[English\].*\| 日本語'; then
            echo "ERROR: $ja: missing '[English](./$en) | 日本語' link near the top" >&2
            fail=1
        fi
        if ! head -5 "$en" | grep -qE 'English \|.*\[日本語\]'; then
            echo "ERROR: $en: missing 'English | [日本語](./$ja)' link near the top" >&2
            fail=1
        fi
        # git log の commit timestamp 比較 (jj 環境では git log が動くこと前提)
        ja_ts=$(git --git-dir=.git log -1 --format=%ct -- "$ja" 2>/dev/null || echo 0)
        en_ts=$(git --git-dir=.git log -1 --format=%ct -- "$en" 2>/dev/null || echo 0)
        if [ "$ja_ts" -gt 0 ] && [ "$en_ts" -gt 0 ] && [ "$ja_ts" -gt "$en_ts" ]; then
            echo "ERROR: $ja was updated after $en. Update the English translation before pushing." >&2
            fail=1
        fi
    done
    [ "$fail" -eq 0 ]

# format 適用
fmt:
    cargo fmt

# ワーキングコピーがクリーン（empty）であることを確認
ensure-clean:
    test "$(jj log -r @ --no-graph -T 'empty')" = "true"

# push (check + test + check-translations を通してから push)
push: check test check-translations
    jj git push

# ビルドして実行
run *ARGS: build
    ./target/release/jj-worktree {{ARGS}}

# リリース (bump: major, minor, patch)
release bump="patch": ensure-clean check test build
    #!/usr/bin/env bash
    set -euo pipefail

    # Version bump
    current=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    IFS='.' read -r major minor patchv <<< "$current"
    case "{{bump}}" in
        major) major=$((major + 1)); minor=0; patchv=0 ;;
        minor) minor=$((minor + 1)); patchv=0 ;;
        patch) patchv=$((patchv + 1)) ;;
        *) echo "Error: Invalid bump type '{{bump}}'" >&2; exit 1 ;;
    esac
    new_version="${major}.${minor}.${patchv}"
    sed -i '' "s/^version = \"${current}\"/version = \"${new_version}\"/" Cargo.toml
    cargo check --quiet
    echo "Version: ${current} -> ${new_version}"

    # CHANGELOG.md update via Claude (auto-generate from commit log)
    latest_tag=$(gh release list --repo kawaz/jj-worktree --limit 1 --json tagName -q '.[0].tagName' 2>/dev/null || echo "")
    if [ -n "$latest_tag" ]; then
        changes=$(jj log -r "$latest_tag..@-" --no-graph -T 'description ++ "\n"' 2>/dev/null || echo "")
    else
        changes=$(jj log -r '..@-' --no-graph -T 'description ++ "\n"' 2>/dev/null || echo "")
    fi
    claude -p "CHANGELOG.mdに v${new_version} ($(date +%Y-%m-%d)) のセクションを追加してください。以下のコミットログを元に、利用者視点で重要な順に記載: 新機能 / 動作変更(破壊的変更は特に明記) / バグ修正 / その他。内部リファクタやCI変更など利用者に影響しないものは省略可。コミットログ: ${changes}"

    # Commit and push (GitHub Actions creates tag + release automatically)
    jj describe -m "Release v${new_version}"
    jj new
    jj bookmark set main -r @-
    just push

    # Watch release workflow
    sleep 3
    run_id=$(gh run list --repo kawaz/jj-worktree --limit 1 --json databaseId -q '.[0].databaseId')
    gh run watch "$run_id" --repo kawaz/jj-worktree
