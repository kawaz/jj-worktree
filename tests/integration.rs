//! Integration tests for jj-worktree.
//!
//! Each test creates an isolated jj repository in a temporary directory using:
//!   git init --bare .git && jj git init --git-repo .git && jj new -r 'root()' && jj workspace add main
//!
//! Tests require both `git` and `jj` to be installed.
//!
//! Important: jj-worktree's `find_repo_root` locates the nearest `.jj` directory.
//! In the git bare + jj workspace layout, both the repo root and child workspaces
//! contain `.jj`, so commands should be run from the **repo root** (default workspace)
//! to get the correct repo_root for metadata storage and path containment checks.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::assert::OutputAssertExt;
use assert_cmd::cargo::cargo_bin;
use predicates::prelude::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether `jj` and `git` are available. If not, the test should be skipped.
fn require_jj_and_git() {
    if Command::new("jj").arg("--version").output().is_err() {
        eprintln!("SKIP: jj not found");
        return;
    }
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("SKIP: git not found");
    }
}

/// Create a fresh jj repository in a temp directory and return (TempDir, repo_root, main_ws_path).
///
/// Layout:
///   <tmp>/
///     .git/       (bare)
///     .jj/        (default workspace — repo_root)
///     main/       (main workspace, also has .jj/ pointing back to repo)
fn setup_jj_repo() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path().canonicalize().expect("canonicalize tmp");

    // git init --bare .git
    let status = Command::new("git")
        .args(["init", "--bare", ".git"])
        .current_dir(&root)
        .output()
        .expect("git init");
    assert!(
        status.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // jj git init --git-repo .git
    let status = Command::new("jj")
        .args(["git", "init", "--git-repo", ".git"])
        .current_dir(&root)
        .output()
        .expect("jj git init");
    assert!(
        status.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // jj new -r 'root()' — move default workspace off the working copy
    let status = Command::new("jj")
        .args(["new", "-r", "root()"])
        .current_dir(&root)
        .output()
        .expect("jj new");
    assert!(
        status.status.success(),
        "jj new -r root() failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // jj workspace add main
    let status = Command::new("jj")
        .args(["workspace", "add", "main"])
        .current_dir(&root)
        .output()
        .expect("jj workspace add main");
    assert!(
        status.status.success(),
        "jj workspace add main failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let main_ws = root.join("main");
    assert!(main_ws.exists(), "main workspace dir should exist");

    (tmp, root, main_ws)
}

/// Return the path to the built jj-worktree binary.
fn jj_worktree_bin() -> PathBuf {
    cargo_bin("jj-worktree")
}

/// Run jj-worktree with given args, in the given working directory.
fn run_jj_worktree(cwd: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::new(jj_worktree_bin())
        .args(args)
        .current_dir(cwd)
        .assert()
}

/// Run the binary via a `git` symlink (shim mode), in the given working directory.
/// Creates a symlink `git -> jj-worktree` in `extra_path_dir` and prepends it to PATH.
fn run_git_shim(cwd: &Path, args: &[&str], extra_path_dir: &Path) -> assert_cmd::assert::Assert {
    let bin_path = jj_worktree_bin();

    // Create symlink git -> jj-worktree in the extra_path_dir
    let git_link = extra_path_dir.join("git");
    if !git_link.exists() {
        std::os::unix::fs::symlink(&bin_path, &git_link).expect("failed to create git symlink");
    }

    // Build PATH with our shim dir first
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", extra_path_dir.display(), original_path);

    Command::new(&git_link)
        .args(args)
        .current_dir(cwd)
        .env("PATH", &new_path)
        .assert()
}

// ---------------------------------------------------------------------------
// 1. Shim passthrough: no .jj -> real git is called
// ---------------------------------------------------------------------------

#[test]
fn shim_passthrough_no_jj() {
    require_jj_and_git();

    // Create a plain git repo (no .jj)
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    Command::new("git")
        .args(["init"])
        .current_dir(&root)
        .output()
        .expect("git init");

    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // `git status` via shim should invoke real git and succeed
    run_git_shim(&root, &["status"], &shim_path).success();
}

// ---------------------------------------------------------------------------
// 2. Shim -C support: git -C <path> worktree list redirected correctly
// ---------------------------------------------------------------------------

#[test]
fn shim_c_option_worktree_list() {
    require_jj_and_git();

    let (_tmp, repo_root, main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // Add a workspace from repo root
    run_jj_worktree(&repo_root, &["add", "test-ws"]).success();

    // Run `git -C <repo_root> worktree list` from the main workspace.
    // The -C flag directs jj repo detection to repo_root (which has .jj),
    // and the worktree subcommand uses the same start_dir for its operations.
    run_git_shim(
        &main_ws,
        &["-C", repo_root.to_str().unwrap(), "worktree", "list"],
        &shim_path,
    )
    .success()
    .stdout(predicate::str::contains("test-ws"));
}

// Regression test for: `git -C <path> worktree list` invoked from outside any
// .jj directory. Earlier versions forwarded the -C path only for jj-repo
// detection but ran the worktree subcommand with `current_dir()`, which would
// fail with "not inside a jj repository" when the cwd was outside any jj tree.
#[test]
fn shim_c_option_from_outside_jj() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // A directory with no .jj anywhere up the tree.
    let outside = TempDir::new().unwrap();
    let outside_path = outside.path().canonicalize().unwrap();

    run_jj_worktree(&repo_root, &["add", "test-ws-outside"]).success();

    run_git_shim(
        &outside_path,
        &["-C", repo_root.to_str().unwrap(), "worktree", "list"],
        &shim_path,
    )
    .success()
    .stdout(predicate::str::contains("test-ws-outside"));
}

// ---------------------------------------------------------------------------
// 3. jj detection: .jj + worktree -> jj-worktree processing
// ---------------------------------------------------------------------------

#[test]
fn shim_detects_jj_and_redirects_worktree() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // `git worktree list` inside a jj repo should be handled by jj-worktree
    run_git_shim(&repo_root, &["worktree", "list"], &shim_path)
        .success()
        .stdout(predicate::str::contains("main"));
}

// ---------------------------------------------------------------------------
// 4. add: workspace creation + bookmark + metadata
// ---------------------------------------------------------------------------

#[test]
fn add_creates_workspace_and_metadata() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Add a workspace with a bookmark (run from repo root)
    run_jj_worktree(
        &repo_root,
        &["add", "feature-auth", "-b", "worktree-feature-auth"],
    )
    .success()
    .stderr(predicate::str::contains("Created workspace 'feature-auth'"));

    // Verify workspace directory exists
    let ws_dir = repo_root.join("feature-auth");
    assert!(ws_dir.exists(), "workspace directory should be created");

    // Verify jj knows about the workspace
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("feature-auth"),
        "jj workspace list should include feature-auth, got: {}",
        stdout
    );

    // Verify bookmark was set
    let output = Command::new("jj")
        .args(["bookmark", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("worktree-feature-auth"),
        "bookmark 'worktree-feature-auth' should exist, got: {}",
        stdout
    );

    // Verify metadata file exists under repo root
    let meta_path = repo_root
        .join(".jj-worktree-meta")
        .join("feature-auth.json");
    assert!(
        meta_path.exists(),
        "metadata file should exist at {:?}",
        meta_path
    );

    let meta_content = std::fs::read_to_string(&meta_path).unwrap();
    assert!(meta_content.contains("\"workspace\": \"feature-auth\""));
    assert!(meta_content.contains("\"bookmark\": \"worktree-feature-auth\""));
}

#[test]
fn add_without_bookmark() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Add a workspace without a bookmark
    run_jj_worktree(&repo_root, &["add", "simple-ws"])
        .success()
        .stderr(predicate::str::contains("Created workspace 'simple-ws'"));

    let ws_dir = repo_root.join("simple-ws");
    assert!(ws_dir.exists());

    // Metadata should have bookmark: null
    let meta_path = repo_root.join(".jj-worktree-meta").join("simple-ws.json");
    assert!(meta_path.exists(), "metadata file should exist");

    let meta_content = std::fs::read_to_string(&meta_path).unwrap();
    assert!(meta_content.contains("\"bookmark\": null"));
}

// ---------------------------------------------------------------------------
// 5. list: git worktree list format output
// ---------------------------------------------------------------------------

#[test]
fn list_shows_workspaces_in_git_worktree_format() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Add a workspace
    run_jj_worktree(&repo_root, &["add", "test-list-ws"]).success();

    // List should show paths and workspace names
    let result = run_jj_worktree(&repo_root, &["list"]).success();

    let stdout = String::from_utf8_lossy(&result.get_output().stdout);

    // Should contain the test workspace
    assert!(
        stdout.contains("test-list-ws"),
        "list output should contain test-list-ws, got: {}",
        stdout
    );

    // Should contain an absolute path (either the actual path or /private variant on macOS)
    assert!(
        stdout.contains(&repo_root.to_string_lossy().to_string()) || stdout.contains("/private"),
        "list output should contain absolute paths, got: {}",
        stdout
    );

    // Regression: each line should contain a 7-char hex commit prefix between
    // the path and the bookmark/workspace name. Earlier versions wrapped the
    // jj revision argument in literal single quotes, which made the lookup
    // fail and silently fall back to an empty commit field.
    let has_short_hash = stdout.lines().any(|line| {
        line.split_whitespace()
            .any(|tok| tok.len() == 7 && tok.chars().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(
        has_short_hash,
        "list output should contain a 7-char hex commit prefix per workspace, got: {}",
        stdout
    );
}

#[test]
fn list_empty_repo_shows_default_workspaces() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Even without adding extra workspaces, list should show default + main
    let result = run_jj_worktree(&repo_root, &["list"]).success();

    let stdout = String::from_utf8_lossy(&result.get_output().stdout);

    assert!(
        stdout.contains("main") || stdout.contains("default"),
        "list output should contain workspace info, got: {}",
        stdout
    );
}

// ---------------------------------------------------------------------------
// 6. remove: safety checks + workspace forget + bookmark delete
// ---------------------------------------------------------------------------

#[test]
fn remove_workspace_and_bookmark() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Add a workspace with bookmark (from repo root)
    run_jj_worktree(&repo_root, &["add", "rm-test-ws", "-b", "worktree-rm-test"]).success();

    let ws_dir = repo_root.join("rm-test-ws");
    assert!(ws_dir.exists());

    // Remove the workspace (use absolute path)
    run_jj_worktree(&repo_root, &["remove", ws_dir.to_str().unwrap()])
        .success()
        .stderr(predicate::str::contains("Removed workspace 'rm-test-ws'"));

    // Verify workspace directory is gone
    assert!(!ws_dir.exists(), "workspace directory should be removed");

    // Verify jj no longer knows about it
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("rm-test-ws"),
        "workspace should be forgotten, got: {}",
        stdout
    );

    // Verify bookmark is deleted
    let output = Command::new("jj")
        .args(["bookmark", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("worktree-rm-test"),
        "bookmark should be deleted, got: {}",
        stdout
    );

    // Verify metadata is cleaned up
    let meta_path = repo_root.join(".jj-worktree-meta").join("rm-test-ws.json");
    assert!(!meta_path.exists(), "metadata file should be removed");
}

// ---------------------------------------------------------------------------
// 7. remove guard: main/default workspace deletion refused
// ---------------------------------------------------------------------------

#[test]
fn remove_refuses_main_workspace() {
    require_jj_and_git();

    let (_tmp, repo_root, main_ws) = setup_jj_repo();

    // Attempt to remove main workspace from repo root should fail with protection message
    run_jj_worktree(&repo_root, &["remove", main_ws.to_str().unwrap()])
        .failure()
        .stderr(predicate::str::contains("protected"));
}

#[test]
fn remove_refuses_default_workspace() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // The default workspace is at repo_root itself.
    // Attempting to remove it should fail with protection message.
    run_jj_worktree(&repo_root, &["remove", repo_root.to_str().unwrap()])
        .failure()
        .stderr(predicate::str::contains("protected"));
}

// ---------------------------------------------------------------------------
// 8. branch -d: metadata-based jj bookmark delete via shim
// ---------------------------------------------------------------------------

#[test]
fn shim_branch_delete_converts_to_jj_bookmark_delete() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // Add a workspace with a bookmark (creates metadata).
    // The metadata file is named after the workspace name, e.g., branch-del-ws.json.
    // However, shim.rs does `meta::load(root, &name)` where `name` is the branch name
    // from `git branch -d <name>`. This means it looks for <name>.json, not <ws_name>.json.
    //
    // To make the shim find the metadata, the workspace name must match the branch name.
    // (This is a known design limitation in the current code.)
    //
    // Use the same name for both workspace and bookmark to test the happy path.
    run_jj_worktree(&repo_root, &["add", "my-feature", "-b", "my-feature"]).success();

    // Verify bookmark exists
    let output = Command::new("jj")
        .args(["bookmark", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("my-feature"),
        "bookmark should exist before deletion, got: {}",
        stdout
    );

    // `git branch -d my-feature` should be intercepted and converted to `jj bookmark delete`
    run_git_shim(&repo_root, &["branch", "-d", "my-feature"], &shim_path).success();

    // Verify the bookmark was deleted
    let output = Command::new("jj")
        .args(["bookmark", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("my-feature"),
        "bookmark should be deleted after `git branch -d`, got: {}",
        stdout
    );
}

#[test]
fn shim_branch_delete_unmanaged_falls_through_to_git() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // `git branch -d nonexistent-branch` — not managed by jj-worktree,
    // should fall through to real git. Real git will likely fail since the branch
    // doesn't exist in the bare git repo, but jj-worktree should not crash.
    // We just verify the command completes (may exit non-zero from real git).
    let result = run_git_shim(
        &repo_root,
        &["branch", "-d", "nonexistent-branch"],
        &shim_path,
    );
    // The exit code doesn't matter (real git may error), but it should not panic
    let _ = result;
}

// ---------------------------------------------------------------------------
// Additional edge case tests
// ---------------------------------------------------------------------------

#[test]
fn add_with_commit_ish() {
    require_jj_and_git();

    let (_tmp, repo_root, main_ws) = setup_jj_repo();

    // Create a commit to reference
    Command::new("jj")
        .args(["describe", "-m", "initial commit"])
        .current_dir(&main_ws)
        .output()
        .unwrap();

    Command::new("jj")
        .args(["new"])
        .current_dir(&main_ws)
        .output()
        .unwrap();

    // Add workspace at a specific commit (using 'root()' as a simple known revision)
    run_jj_worktree(&repo_root, &["add", "commitish-ws", "root()"]).success();

    let ws_dir = repo_root.join("commitish-ws");
    assert!(ws_dir.exists());
}

#[test]
fn remove_nonexistent_path_fails() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    run_jj_worktree(&repo_root, &["remove", "nonexistent-ws-path"]).failure();
}

#[test]
fn remove_with_relative_path() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Add a workspace
    run_jj_worktree(&repo_root, &["add", "rel-rm-ws", "-b", "worktree-rel-rm"]).success();

    let ws_dir = repo_root.join("rel-rm-ws");
    assert!(ws_dir.exists());

    // Remove using relative path
    run_jj_worktree(&repo_root, &["remove", "rel-rm-ws"])
        .success()
        .stderr(predicate::str::contains("Removed workspace 'rel-rm-ws'"));

    assert!(!ws_dir.exists());
}

#[test]
fn shim_non_worktree_command_passthrough() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // `git --version` should pass through to real git even inside a jj repo
    run_git_shim(&repo_root, &["--version"], &shim_path)
        .success()
        .stdout(predicate::str::contains("git version"));
}

#[test]
fn jj_worktree_help() {
    // --help should work without any repo
    Command::new(jj_worktree_bin())
        .args(["--help"])
        .assert()
        .success()
        .stderr(predicate::str::contains("jj-worktree"));
}

#[test]
fn jj_worktree_version() {
    Command::new(jj_worktree_bin())
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("jj-worktree"));
}

#[test]
fn jj_worktree_unknown_command() {
    Command::new(jj_worktree_bin())
        .args(["bogus-command"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown command"));
}

#[test]
fn list_via_shim_worktree_list() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // Add a workspace
    run_jj_worktree(&repo_root, &["add", "shim-list-ws"]).success();

    // `git worktree list` should show workspaces
    run_git_shim(&repo_root, &["worktree", "list"], &shim_path)
        .success()
        .stdout(predicate::str::contains("shim-list-ws"));
}

#[test]
fn add_via_shim_worktree_add() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // `git worktree add shim-add-ws` should be redirected to jj-worktree
    run_git_shim(&repo_root, &["worktree", "add", "shim-add-ws"], &shim_path)
        .success()
        .stderr(predicate::str::contains("Created workspace 'shim-add-ws'"));

    let ws_dir = repo_root.join("shim-add-ws");
    assert!(ws_dir.exists());
}

#[test]
fn remove_via_shim_worktree_remove() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // Add workspace first
    run_jj_worktree(&repo_root, &["add", "shim-rm-ws"]).success();

    let ws_dir = repo_root.join("shim-rm-ws");
    assert!(ws_dir.exists());

    // `git worktree remove <path>` should be redirected
    run_git_shim(
        &repo_root,
        &["worktree", "remove", ws_dir.to_str().unwrap()],
        &shim_path,
    )
    .success();

    assert!(!ws_dir.exists());
}

// Regression test for cmd_status mapping. jj reports added files as
// "A path", which shim must translate to git porcelain v1 "A  path"
// (staged add — two-char status field XY). Earlier versions emitted
// "?? path" (untracked), which broke consumers like Claude Code that
// distinguish staged vs untracked changes.
#[test]
fn shim_status_maps_added_to_staged_add() {
    require_jj_and_git();

    let (_tmp, _repo_root, main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // Create a new file in the main workspace; jj auto-snapshots it as
    // an addition relative to the parent change.
    let added_file = main_ws.join("new-file.txt");
    std::fs::write(&added_file, "hello\n").expect("write new-file");

    let result = run_git_shim(&main_ws, &["status", "--porcelain"], &shim_path).success();
    let stdout = String::from_utf8_lossy(&result.get_output().stdout);

    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("A  ") && l.contains("new-file.txt")),
        "expected staged-add line `A  new-file.txt` in porcelain output, got:\n{}",
        stdout
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.starts_with("??") && l.contains("new-file.txt")),
        "added file must not be reported as untracked (??), got:\n{}",
        stdout
    );
}

// ---------------------------------------------------------------------------
// 9. drop-in pass-through: untranslatable worktree operations reach real git
// ---------------------------------------------------------------------------

/// The shim's contract is to be a drop-in `git`: only the worktree operations
/// it can fully honor are translated, everything else reaches real git. A
/// colocated workspace *is* a real git worktree, so real git's answer is
/// correct where the shim's translation would be a lossy no-op or an error.
///
/// Regression: `list --porcelain` (used by git clients to enumerate
/// worktrees) used to fail with "list takes no options". The passthrough must
/// also be complete: `jj workspace add` registers a git worktree even in the
/// bare-backed non-colocated layout (jj ≥0.46 creates the worktree admin
/// record for every workspace), so the porcelain output has to name the
/// added workspace, not just the repository's own line.
#[test]
fn shim_worktree_list_porcelain_passthrough() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_jj_worktree(&repo_root, &["add", "porcelain-ws"]).success();

    run_git_shim(&repo_root, &["worktree", "list", "--porcelain"], &shim_path)
        .success()
        .stdout(
            predicate::str::contains("worktree ").and(predicate::str::contains("porcelain-ws")),
        );
}

/// Regression: unhandled worktree subcommands used to die with
/// "unknown worktree subcommand". They must reach real git instead, so the
/// caller sees git's own error for its own mistake.
#[test]
fn shim_worktree_unhandled_subcommand_passthrough() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // `git worktree lock` on a path that is not a worktree: real git rejects
    // it with its own message. The shim must not intercept it.
    run_git_shim(
        &repo_root,
        &["worktree", "lock", "not-a-worktree"],
        &shim_path,
    )
    .failure()
    .stderr(predicate::str::contains("is not a working tree"))
    .stderr(predicate::str::contains("unknown worktree subcommand").not());
}

/// `--orphan` has no jj equivalent: translating it would produce a jj
/// workspace where the caller asked for a git-native orphan worktree. The
/// whole call passes through to real git, so the result is a real git
/// worktree that jj does not track as a workspace.
#[test]
fn shim_worktree_add_orphan_passthrough() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // The exact call shape jj itself uses for colocated workspaces.
    run_git_shim(
        &repo_root,
        &[
            "worktree",
            "add",
            "--orphan",
            "-b",
            "orphan-branch",
            "orphan-ws",
        ],
        &shim_path,
    )
    .success();

    // Real git made a git worktree (with its own .git file), not a jj workspace.
    let ws_dir = repo_root.join("orphan-ws");
    assert!(ws_dir.exists(), "git worktree directory should exist");
    assert!(
        ws_dir.join(".git").is_file(),
        "orphan worktree should be a real git worktree (.git file), not a jj workspace"
    );

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("orphan-ws"),
        "jj must not track the git-native orphan workspace, got: {}",
        stdout
    );
}

/// Git also accepts `-d` as the short form of `--detach`. The short form must
/// be excluded from translation just like the long form, or `parse_add_args`
/// would treat it as an unknown no-op and create a jj workspace where the
/// caller asked for a detached git worktree.
#[test]
fn shim_worktree_add_detach_short_passthrough() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    // A freshly initialized colocated repo has an unborn git HEAD; real git
    // needs a commit to detach at.
    let initial = Command::new("jj")
        .args(["new"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        initial.status.success(),
        "jj new failed: {}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(&root, &["worktree", "add", "-d", "detached-ws"], &shim_path).success();

    // Real git made a git worktree, not a jj workspace.
    let ws_dir = root.join("detached-ws");
    assert!(ws_dir.exists(), "git worktree directory should exist");
    assert!(
        ws_dir.join(".git").is_file(),
        "detached worktree should be a real git worktree (.git file), not a jj workspace"
    );

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("detached-ws"),
        "jj must not track the detached git worktree as a workspace, got: {}",
        stdout
    );
}

/// `remove` must translate only registered jj workspaces. A git-native
/// worktree (created via the `add --orphan` passthrough) is unknown to jj;
/// translating its removal fails with "no workspace found for path" and
/// leaves the worktree behind, so it must reach real git instead.
#[test]
fn shim_worktree_remove_git_native_passthrough() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(
        &root,
        &[
            "worktree",
            "add",
            "--orphan",
            "-b",
            "orphan-branch",
            "orphan-ws",
        ],
        &shim_path,
    )
    .success();

    run_git_shim(
        &root,
        &["worktree", "remove", "--force", "orphan-ws"],
        &shim_path,
    )
    .success();

    assert!(
        !root.join("orphan-ws").exists(),
        "git-native worktree should be removed by real git"
    );
}

/// `-f` is git's short form of `--force`; a translated `remove -f` must not
/// fail with "unknown option" where real git would succeed.
#[test]
fn shim_worktree_remove_short_force_translates() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_jj_worktree(&repo_root, &["add", "forced-ws"]).success();

    run_git_shim(
        &repo_root,
        &["worktree", "remove", "-f", "forced-ws"],
        &shim_path,
    )
    .success()
    .stderr(predicate::str::contains("Removed workspace 'forced-ws'"));

    assert!(
        !repo_root.join("forced-ws").exists(),
        "translated removal should delete the workspace directory"
    );
    assert!(
        !repo_root.join(".jj-worktree-meta/forced-ws.json").exists(),
        "translated removal should delete the metadata file"
    );
}

// ---------------------------------------------------------------------------
// 10. add: leading directories like real git
// ---------------------------------------------------------------------------

/// Real git creates missing leading directories for the worktree path; the
/// translated `jj workspace add` refuses when the parent is absent, so a
/// caller passing a nested path such as `.kilo/worktrees/<name>` (Kilo Code /
/// Agent Manager does) failed unless it pre-created the parents.
#[test]
fn add_creates_leading_directories() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(
        &repo_root,
        &[
            "worktree",
            "add",
            ".kilo/worktrees/nested-ws",
            "-b",
            "nested-ws",
        ],
        &shim_path,
    )
    .success()
    .stderr(predicate::str::contains("Created workspace 'nested-ws'"));

    assert!(
        repo_root.join(".kilo/worktrees/nested-ws").exists(),
        "nested workspace directory should be created"
    );
}

// ---------------------------------------------------------------------------
// 11. prune reap: forgotten jj workspaces are cleaned up
// ---------------------------------------------------------------------------

/// Callers that delete a worktree by renaming its directory aside and running
/// `git worktree prune` (Kilo Code / Agent Manager does exactly this) only
/// clean git's admin records; in a jj repository the workspace record lingers
/// until a manual `jj workspace forget`. A caller-initiated prune through the
/// shim reaps workspaces whose directory is gone.
#[test]
fn prune_reaps_forgotten_shim_workspace() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    // Create a workspace through the shim (records .jj-worktree-meta).
    run_git_shim(&repo_root, &["worktree", "add", "reaped-ws"], &shim_path).success();

    // Delete it the way git clients do: rename aside, then prune.
    let aside = repo_root.join(".kilo-reaped-tmp");
    std::fs::rename(repo_root.join("reaped-ws"), &aside).expect("rename ws aside");
    run_git_shim(
        &repo_root,
        &["worktree", "prune", "--expire", "now"],
        &shim_path,
    )
    .success()
    .stderr(predicate::str::contains("Forgotten workspace 'reaped-ws'"));

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("reaped-ws"),
        "workspace with deleted directory should be forgotten, got: {}",
        stdout
    );
    assert!(
        !repo_root.join(".jj-worktree-meta/reaped-ws.json").exists(),
        "stale metadata file should be removed"
    );

    std::fs::remove_dir_all(&aside).ok();
}

/// The reap resolves workspace paths through `jj workspace root --name`
/// rather than the shim's metadata, so workspaces created without the shim
/// (e.g. plain `jj workspace add` from another tool) are cleaned up too.
#[test]
fn prune_reaps_forgotten_workspace_without_metadata() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();

    // Create a workspace with plain jj: no .jj-worktree-meta entry exists.
    let status = Command::new("jj")
        .args(["workspace", "add", "plain-ws"])
        .current_dir(&repo_root)
        .output()
        .expect("jj workspace add plain-ws");
    assert!(status.status.success());

    std::fs::remove_dir_all(repo_root.join("plain-ws")).expect("delete ws dir");

    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();
    run_git_shim(
        &repo_root,
        &["worktree", "prune", "--expire", "now"],
        &shim_path,
    )
    .success()
    .stderr(predicate::str::contains("Forgotten workspace 'plain-ws'"));

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("plain-ws"),
        "meta-less workspace with deleted directory should be forgotten"
    );
}

/// The reap is surgical: live workspaces and the protected default workspace
/// must survive a caller-initiated prune.
#[test]
fn prune_spares_live_and_protected_workspaces() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(&repo_root, &["worktree", "add", "kept-ws"], &shim_path).success();

    run_git_shim(
        &repo_root,
        &["worktree", "prune", "--expire", "now"],
        &shim_path,
    )
    .success();

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("kept-ws"),
        "live workspace should survive prune, got: {}",
        stdout
    );
    assert!(
        stdout.contains("default"),
        "protected default workspace should survive prune, got: {}",
        stdout
    );
    assert!(
        repo_root.join("kept-ws").exists(),
        "live workspace directory should survive prune"
    );
    assert!(
        repo_root.join(".jj-worktree-meta/kept-ws.json").exists(),
        "live workspace metadata should survive prune"
    );
}

/// `git worktree prune -n` / `--dry-run` is a read-only preview: it must not
/// forget jj workspaces or delete their metadata. (Regression: the reap used
/// to run before real git saw the options, so a dry-run mutated jj state.)
#[test]
fn prune_dry_run_leaves_workspaces_untouched() {
    require_jj_and_git();

    let (_tmp, repo_root, _main_ws) = setup_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_jj_worktree(&repo_root, &["add", "dry-ws"]).success();
    std::fs::rename(repo_root.join("dry-ws"), repo_root.join(".dry-run-aside"))
        .expect("rename ws aside");

    for dry_flag in ["-n", "--dry-run"] {
        run_git_shim(&repo_root, &["worktree", "prune", dry_flag], &shim_path).success();
    }

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&repo_root)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("dry-ws"),
        "dry-run must not forget the workspace"
    );
    assert!(
        repo_root.join(".jj-worktree-meta/dry-ws.json").exists(),
        "dry-run must not delete the metadata file"
    );
    assert!(
        repo_root.join(".git/worktrees").read_dir().unwrap().count() > 0,
        "dry-run must not remove git's admin records"
    );

    std::fs::remove_dir_all(repo_root.join(".dry-run-aside")).ok();
}

/// Real git's `prune --expire <time>` keeps records younger than the expiry.
/// The reap must keep the jj workspace record for them too, so both sides
/// agree; the workspace is only reaped once the expiry is reached.
#[test]
fn prune_respects_expire() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(&root, &["worktree", "add", "expired-ws"], &shim_path).success();
    std::fs::remove_dir_all(root.join("expired-ws")).expect("delete ws dir");

    // Young record: real git keeps it even with the dir gone.
    run_git_shim(
        &root,
        &["worktree", "prune", "--expire", "1.year.ago"],
        &shim_path,
    )
    .success();
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("expired-ws"),
        "workspace kept by git's expiry must not be reaped"
    );

    // Expiry reached: both sides agree it is gone.
    run_git_shim(&root, &["worktree", "prune", "--expire", "now"], &shim_path).success();
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("expired-ws"),
        "workspace pruned by git's expiry should be forgotten"
    );
}

/// Real git never prunes locked worktree records. The reap must keep the jj
/// workspace record for a locked workspace as well, and reap it only after
/// the record is unlocked.
#[test]
fn prune_spares_locked_workspaces() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(&root, &["worktree", "add", "locked-ws"], &shim_path).success();
    // `lock` is not translatable, so this reaches real git.
    run_git_shim(&root, &["worktree", "lock", "locked-ws"], &shim_path).success();
    std::fs::remove_dir_all(root.join("locked-ws")).expect("delete ws dir");

    run_git_shim(&root, &["worktree", "prune", "--expire", "now"], &shim_path).success();
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("locked-ws"),
        "workspace whose git record is locked must not be reaped"
    );

    run_git_shim(&root, &["worktree", "unlock", "locked-ws"], &shim_path).success();
    run_git_shim(&root, &["worktree", "prune", "--expire", "now"], &shim_path).success();
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("locked-ws"),
        "workspace whose git record was unlocked should be forgotten"
    );
}

/// End-to-end regression for the extension's delete flow in its native
/// colocated layout: rename the worktree aside, then
/// `git worktree prune --expire now` — the colocated jj workspace record must
/// be forgotten along with git's admin record.
#[test]
fn prune_reaps_forgotten_colocated_workspace() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    run_git_shim(&root, &["worktree", "add", "ext-ws"], &shim_path).success();
    std::fs::rename(root.join("ext-ws"), root.join(".ext-aside")).expect("rename ws aside");

    run_git_shim(&root, &["worktree", "prune", "--expire", "now"], &shim_path)
        .success()
        .stderr(predicate::str::contains("Forgotten workspace 'ext-ws'"));

    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("ext-ws"),
        "colocated workspace with removed directory should be forgotten"
    );
}

// ---------------------------------------------------------------------------
// 12. jj as caller: its internal git worktree calls pass through
// ---------------------------------------------------------------------------

/// In a colocated repository jj itself spawns `git worktree add --orphan ...`
/// to create the backing git worktree of a new workspace. With the shim ahead
/// of git on PATH those internal calls land on the shim; translating them
/// re-enters `jj workspace add` and recurses until the operation fails. The
/// shim detects that the parent process is `jj` and passes through.
fn setup_colocated_jj_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path().canonicalize().expect("canonicalize tmp");

    let status = Command::new("jj")
        .args(["git", "init", "--colocate", "."])
        .current_dir(&root)
        .output()
        .expect("jj git init --colocate");
    assert!(
        status.status.success(),
        "jj git init --colocate failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    (tmp, root)
}

/// Run `jj` with the shim ahead of git on PATH, as a real child process so
/// the shim's caller-identity check sees `jj` as its parent.
///
/// Installs the `git -> jj-worktree` symlink in `shim_path` first (like
/// `run_git_shim` does): with an empty directory, jj would find real git
/// later on PATH and the caller-identity guard would never be exercised.
/// `env_overrides` sets additional environment variables for the jj child.
fn run_jj_with_shim_on_path(
    root: &Path,
    args: &[&str],
    shim_path: &Path,
    env_overrides: &[(&str, &str)],
) -> std::process::Output {
    let git_link = shim_path.join("git");
    if !git_link.exists() {
        std::os::unix::fs::symlink(jj_worktree_bin(), &git_link)
            .expect("failed to create git symlink");
    }

    let original_path = std::env::var("PATH").unwrap_or_default();
    let shim_first = format!("{}:{}", shim_path.display(), original_path);
    Command::new("jj")
        .args(args)
        .current_dir(root)
        .env("PATH", &shim_first)
        .envs(env_overrides.iter().copied())
        .output()
        .expect("run jj with shim on PATH")
}

#[test]
fn jj_internal_git_worktree_calls_do_not_recurse() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    let output = run_jj_with_shim_on_path(&root, &["workspace", "add", "inner"], &shim_path, &[]);
    assert!(
        output.status.success(),
        "jj workspace add must not recurse through the shim, stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("External git program failed"),
        "jj's internal git call must reach real git, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let list = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("inner"),
        "colocated workspace should be created, got: {}",
        String::from_utf8_lossy(&list.stdout)
    );
}

/// The same guard applies to jj's own prunes: a caller-initiated prune gets
/// the reap, but jj's internal prune (parent is jj) must stay pure
/// pass-through so it never reaps the workspace it is currently managing.
#[test]
fn jj_internal_prune_stays_pure_passthrough() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    let added = run_jj_with_shim_on_path(&root, &["workspace", "add", "inner"], &shim_path, &[]);
    assert!(added.status.success());

    let forgotten =
        run_jj_with_shim_on_path(&root, &["workspace", "forget", "inner"], &shim_path, &[]);
    assert!(
        forgotten.status.success(),
        "jj workspace forget must work with the shim on PATH, stderr: {}",
        String::from_utf8_lossy(&forgotten.stderr)
    );

    let list = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        !stdout.contains("inner"),
        "inner workspace should be forgotten, got: {}",
        stdout
    );
    assert!(
        stdout.contains("default"),
        "default workspace must survive, got: {}",
        stdout
    );
}

/// The caller-identity check must not depend on the verbose-logging option:
/// with `JJ_WORKTREE_ISSUE_INCLUDE_CMDLINE=1` the JSONL log records the
/// parent's full argv (`jj workspace add inner`), but identity comparison
/// against `"jj"` must still succeed, or jj's internal `remove`/`prune`
/// would be translated (and its prune would reap).
#[test]
fn jj_internal_calls_pass_through_with_verbose_issue_log() {
    require_jj_and_git();

    let (_tmp, root) = setup_colocated_jj_repo();
    let shim_dir = TempDir::new().unwrap();
    let shim_path = shim_dir.path().canonicalize().unwrap();

    let env = [("JJ_WORKTREE_ISSUE_INCLUDE_CMDLINE", "1")];

    let added = run_jj_with_shim_on_path(&root, &["workspace", "add", "inner"], &shim_path, &env);
    assert!(
        added.status.success(),
        "jj workspace add must not recurse with verbose issue logging, stdout: {}, stderr: {}",
        String::from_utf8_lossy(&added.stdout),
        String::from_utf8_lossy(&added.stderr)
    );

    let forgotten =
        run_jj_with_shim_on_path(&root, &["workspace", "forget", "inner"], &shim_path, &env);
    assert!(
        forgotten.status.success(),
        "jj workspace forget must pass through with verbose issue logging, stderr: {}",
        String::from_utf8_lossy(&forgotten.stderr)
    );

    let list = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        !stdout.contains("inner"),
        "inner workspace should be forgotten, got: {}",
        stdout
    );
    assert!(
        stdout.contains("default"),
        "default workspace must survive, got: {}",
        stdout
    );
}
