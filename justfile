# jj-worktree justfile
#
# Canonical task runner aligned with kawaz/bump-semver:
#   - VCS-shaped operations delegate to `bump-semver vcs` subcommands
#   - `check-version-bumped` blocks pushes that touch src/ or Cargo.toml
#     without advancing the Cargo.toml version
#   - Release flow: bump-version → push → release.yml builds tag + GH Release
#
# Recipe declaration order is intentional: most-used first so `just --list`
# (and `default`) surface them prominently.

set shell := ["bash", "-euo", "pipefail", "-c"]

set script-interpreter := ["bash", "-euo", "pipefail"]

set positional-arguments

# default behaviour: alias for `list`
default: list

# show the recipe list
list:
    @just --list --unsorted

# ---------- atomic (lint / test / build) ----------

# cargo fmt + clippy --fix (auto-fix 込み、残った警告はエラー)
[private]
lint-cargo:
    cargo fmt
    cargo clippy --fix --allow-dirty --allow-staged --all-targets -- -D warnings

# just --fmt (justfile self-format check)
[private]
lint-just:
    just --unstable --fmt --check

# lint-cargo + lint-just
lint: lint-cargo lint-just

# cargo test (ARGS forwarded, e.g. `just test worktree::`)
test *ARGS: lint
    cargo test "$@"

# release build -> target/release/jj-worktree
build: lint
    cargo build --release

# build then run the local binary, forwarding all args
run *ARGS: build
    ./target/release/jj-worktree "$@"

# lint + test + build (CI entry point)
ci: lint test build

# ---------- gates (push の内部、利用者が直接叩くことほぼなし) ----------

# working copy is clean (dogfood: bump-semver vcs is clean)
[private]
ensure-clean:
    bump-semver vcs is clean

# fail if bump-trigger-paths changed since main@origin but Cargo.toml was not bumped
# test-only changes (src/test_util.rs, tests/) are excluded from the bump trigger
check-version-bumped: (_check-version-bumped "src/" "Cargo.toml")

# (helper) diff があれば Cargo.toml が main@origin より上がっているか検証
[private]
[script]
_check-version-bumped *target_paths:
    if ! bump-semver vcs diff -q main@origin -- "$@" --excludes 'glob:src/test_util.rs'; then
        bump-semver compare gt Cargo.toml vcs:main@origin
    fi

# fail if Cargo.toml is not greater than the latest release (main@origin の Cargo.toml)
[private]
check-against-latest-release:
    bump-semver compare gt Cargo.toml vcs:main@origin

# translation pair freshness check via `bump-semver vcs outdated`
# 対象: README-ja.md ↔ README.md / docs/DESIGN-ja.md ↔ docs/DESIGN.md
[private]
check-outdated-translations: ensure-clean
    bump-semver vcs outdated 'glob:**/*-ja.md' '$1/$2.md'

# ---------- release flow ----------

# bump Cargo.toml version (default: patch) and create a release commit
bump-version level="patch": ensure-clean
    bump-semver "$1" Cargo.toml --write --quiet
    cargo check --quiet
    bump-semver vcs commit -m "Release v$(bump-semver get Cargo.toml)" Cargo.toml Cargo.lock

# push to origin/main with gates
push: ci check-outdated-translations check-version-bumped
    bump-semver vcs push --branch main --jj-bookmark-auto-advance
    @echo "[hint] gh-monitor:watch-workflow --sha $(bump-semver vcs get commit-id --rev main) --on-success release.yml 'just on-success-release' kawaz/jj-worktree"

# release.yml workflow が success になった時に AI が実行する action
# (watch-workflow の `--on-success release.yml 'just on-success-release'` 経由で
# 通知 event に `[ACTION:release.yml] just on-success-release` が emit される)
on-success-release:
    # tap repo を直接 git pull (= `brew update` 全 tap 巡回より速い)
    git -C "$(brew --repository)/Library/Taps/kawaz/homebrew-tap" pull --ff-only
    brew upgrade kawaz/tap/jj-worktree
    jj-worktree --version

# ---------- utility ----------

# display Cargo.toml version + binary --version output
version:
    echo "Cargo.toml: $(bump-semver get Cargo.toml)"
    if [ -x ./target/release/jj-worktree ]; then echo "binary: $(./target/release/jj-worktree --version)"; fi
    if command -v jj-worktree >/dev/null 2>&1; then echo "local binary: $(jj-worktree --version)"; fi
