# jj-worktree

# デフォルト: レシピ一覧
default:
    @just --list

# ビルド (release)
build:
    cargo build --release

# テスト
test:
    cargo test

# lint + format チェック
check:
    cargo fmt --check
    cargo clippy -- -D warnings

# format 適用
fmt:
    cargo fmt

# ビルドして実行
run *ARGS: build
    ./target/release/jj-worktree {{ARGS}}

# リリース (bump: major, minor, patch)
release bump="patch":
    #!/usr/bin/env bash
    set -euo pipefail

    # Pre-checks
    cargo fmt --check || { echo "Error: Run 'cargo fmt' first." >&2; exit 1; }
    cargo clippy -- -D warnings
    cargo test

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

    # Commit, tag, push
    jj describe -m "Release v${new_version}"
    jj new
    jj bookmark set main -r @-
    jj tag set "v${new_version}" -r @-
    jj git push --bookmark main
    jj git export
    GIT_WORK_TREE="$(pwd)" git --git-dir="$(jj root)/../.git" push origin "v${new_version}"

    # Watch workflow
    gh run watch
