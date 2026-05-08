# jj-worktree

# デフォルト: レシピ一覧
default:
    @just --list

# ビルドして実行
run *ARGS: build
    ./target/release/jj-worktree {{ARGS}}

# ビルド (release)
build:
    cargo build --release

test:
    cargo test

check: lint fmt

lint:
    cargo clippy -- -D warnings

fmt:
    cargo fmt

# push (check + test + check-translations を通してから push)
push: check test check-translations
    jj git push

# ワーキングコピーがクリーン（empty）であることを確認
ensure-clean:
    test "$(jj log -r @ --no-graph -T 'empty')" = "true"

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
 
