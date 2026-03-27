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
