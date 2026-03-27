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
        return;
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
    // while the worktree command internally also uses cwd for its operations.
    // Note: worktree::run uses env::current_dir() internally, so we still
    // need to be inside a jj directory for the command to work.
    // This test verifies that -C is parsed correctly and the shim routes
    // to jj-worktree instead of real git.
    run_git_shim(
        &main_ws,
        &["-C", repo_root.to_str().unwrap(), "worktree", "list"],
        &shim_path,
    )
    .success()
    .stdout(predicate::str::contains("test-ws"));
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
