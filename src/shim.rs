use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::jj;
use crate::meta;

/// Result of parsing git global options from the argument list.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedGitArgs {
    /// Path specified by `-C <path>` (last one wins, matching git behavior).
    pub work_dir: Option<String>,
    /// Index of the first non-option argument (the subcommand), if any.
    pub subcmd_index: Option<usize>,
}

/// Parse git global options from the argument list and locate the subcommand.
///
/// This is a pure function suitable for unit testing.
/// It handles the following git global options by skipping them:
///   -C <path>              (consumes next arg as path)
///   -c <key=val>           (consumes next arg)
///   --git-dir=<path>       (= form)
///   --git-dir <path>       (space form, consumes next arg)
///   --work-tree=<path>     (= form)
///   --work-tree <path>     (space form, consumes next arg)
///   --no-pager
///   --no-replace-objects
///   --bare
///   --literal-pathspecs
///   --glob-pathspecs
///   --noglob-pathspecs
///   --icase-pathspecs
pub fn parse_git_global_opts(args: &[String]) -> ParsedGitArgs {
    let mut work_dir: Option<String> = None;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if arg == "-C" {
            // -C <path>: consumes the next argument
            i += 1;
            if i < args.len() {
                work_dir = Some(args[i].clone());
            }
            i += 1;
        } else if arg == "-c" {
            // -c key=val: consumes the next argument
            i += 2;
        } else if arg.starts_with("--git-dir=") || arg.starts_with("--work-tree=") {
            // --git-dir=<path> or --work-tree=<path>: single arg with =
            i += 1;
        } else if arg == "--git-dir" || arg == "--work-tree" {
            // --git-dir <path> or --work-tree <path>: consumes next arg
            i += 2;
        } else if matches!(
            arg.as_str(),
            "--no-pager"
                | "--no-replace-objects"
                | "--bare"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
        ) {
            i += 1;
        } else {
            // First non-option argument: this is the subcommand
            return ParsedGitArgs {
                work_dir,
                subcmd_index: Some(i),
            };
        }
    }

    // No subcommand found (all args were global options)
    ParsedGitArgs {
        work_dir,
        subcmd_index: None,
    }
}

/// Find the real git binary, skipping our own binary.
///
/// 1. If `JJ_WORKTREE_REAL_GIT` is set, use that.
/// 2. Otherwise, search PATH for a `git` that is not our own binary.
fn find_real_git() -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Check environment variable override
    if let Ok(path) = env::var("JJ_WORKTREE_REAL_GIT") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!("JJ_WORKTREE_REAL_GIT points to non-existent path: {path}").into());
    }

    // Get our own canonical path to exclude from search
    let self_path = env::current_exe().ok().and_then(|p| p.canonicalize().ok());

    // Search PATH
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join("git");
            if candidate.is_file() {
                // Skip if it's our own binary
                if let Some(ref self_p) = self_path
                    && let Ok(canonical) = candidate.canonicalize()
                    && &canonical == self_p
                {
                    continue;
                }
                return Ok(candidate);
            }
        }
    }

    Err("could not find real git binary in PATH".into())
}

/// Execute the real git binary with the given arguments, replacing the current process on Unix.
fn exec_real_git(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let git = find_real_git()?;

    crate::jj::debug_message(&format!(
        "exec real git: {} {}",
        git.display(),
        args.join(" ")
    ));

    let mut cmd = Command::new(&git);
    cmd.args(args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // This replaces the current process; it never returns on success.
        let err = cmd.exec();
        Err(format!("failed to exec real git: {err}").into())
    }

    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Parse `branch` subcommand arguments to detect `branch -d <name>`.
/// Returns Some(name) if the pattern matches.
fn parse_branch_delete(args: &[String], subcmd_index: usize) -> Option<String> {
    // args[subcmd_index] == "branch"
    // Look for -d, --delete, or -D followed by a name
    let rest = &args[subcmd_index + 1..];
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        if a == "-d" || a == "--delete" || a == "-D" {
            if i + 1 < rest.len() {
                return Some(rest[i + 1].clone());
            }
            return None;
        }
        i += 1;
    }
    None
}

/// Entry point for git shim mode. Called from main.rs when argv[0] is "git".
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // Bypass shim entirely when JJ_WORKTREE_DISABLED=1
    if env::var("JJ_WORKTREE_DISABLED").as_deref() == Ok("1") {
        return exec_real_git(args);
    }

    let parsed = parse_git_global_opts(args);

    // Determine the working directory for jj repo detection
    let start_dir: PathBuf = match &parsed.work_dir {
        Some(p) => PathBuf::from(p),
        None => env::current_dir()?,
    };

    // Check if we're inside a jj repository
    let repo_root = jj::find_repo_root(&start_dir);

    // If no subcommand, just pass through to real git
    let subcmd_index = match parsed.subcmd_index {
        Some(idx) => idx,
        None => return exec_real_git(args),
    };

    let subcmd = &args[subcmd_index];

    match (subcmd.as_str(), &repo_root) {
        ("worktree", Some(_root)) => {
            // Redirect to jj-worktree's worktree module
            let worktree_args: Vec<String> = args[subcmd_index + 1..].to_vec();
            crate::worktree::run(&worktree_args)
        }
        ("branch", Some(root)) => {
            // Check for `branch -d <name>` pattern
            if let Some(name) = parse_branch_delete(args, subcmd_index) {
                // Check if this bookmark is managed by jj-worktree (search by bookmark name)
                if let Ok(Some(_meta)) = meta::find_by_bookmark(root, &name) {
                    // Managed bookmark: convert to jj bookmark delete
                    jj::debug_message(&format!(
                        "converting `git branch -d {name}` to `jj bookmark delete {name}`"
                    ));
                    let output = jj::run_stdout(Some(root), &["bookmark", "delete", &name])?;
                    if !output.is_empty() {
                        println!("{output}");
                    }
                    // Sync jj state to git refs
                    if let Err(e) = jj::run(Some(root), &["git", "export"]) {
                        jj::debug_message(&format!("jj git export failed: {e}"));
                    }
                    return Ok(());
                }
            }
            // Not a managed bookmark or not a delete: fall through to real git
            exec_real_git(args)
        }
        ("status", Some(root)) => {
            // Intercept `git status` in jj workspace to return jj-compatible results.
            // Claude Code uses `git status --porcelain` to check for changes before
            // ExitWorktree; real git returns incorrect results in jj workspaces.
            jj::debug_message("intercepting git status in jj workspace");
            cmd_status(root, &start_dir, &args[subcmd_index + 1..])
        }
        _ => {
            // Not in a jj repo, or unhandled subcommand: fall through to real git
            exec_real_git(args)
        }
    }
}

/// Handle `git status` by translating to `jj diff --summary`.
///
/// Converts jj diff output (e.g., "M file.txt") to git porcelain v1 format
/// (e.g., " M file.txt").
fn cmd_status(
    _repo_root: &Path,
    work_dir: &Path,
    _args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let output = jj::run_stdout(Some(work_dir), &["diff", "--summary"]);
    match output {
        Ok(diff) => {
            if diff.is_empty() {
                // No changes — output nothing (matches git status --porcelain with clean tree)
                return Ok(());
            }
            // Convert jj diff --summary format to git porcelain v1.
            // jj: "M file.txt", "A file.txt", "D file.txt", "R {from} {to}"
            // git porcelain v1: " M file.txt", "A  file.txt", " D file.txt", "R  from -> to"
            for line in diff.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some((status, path)) = trimmed.split_once(' ') {
                    match status {
                        "M" => println!(" M {path}"),
                        "A" => println!("?? {path}"),
                        "D" => println!(" D {path}"),
                        "R" => {
                            // jj format: "R {from} {to}" — just report as modified
                            println!(" M {path}");
                        }
                        _ => println!(" M {path}"),
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            // If jj diff fails, fall back to real git
            jj::debug_message(&format!("jj diff failed, falling back to real git: {e}"));
            let all_args: Vec<String> = std::iter::once("status".to_string())
                .chain(_args.iter().cloned())
                .collect();
            exec_real_git(&all_args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    // --- parse_git_global_opts tests ---

    #[test]
    fn parse_no_args() {
        let result = parse_git_global_opts(&[]);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: None,
            }
        );
    }

    #[test]
    fn parse_simple_subcommand() {
        let args = s(&["status"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(0),
            }
        );
    }

    #[test]
    fn parse_c_option_with_subcommand() {
        let args = s(&["-C", "/some/path", "worktree", "list"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: Some("/some/path".to_string()),
                subcmd_index: Some(2),
            }
        );
    }

    #[test]
    fn parse_c_option_last_wins() {
        let args = s(&["-C", "/first", "-C", "/second", "status"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: Some("/second".to_string()),
                subcmd_index: Some(4),
            }
        );
    }

    #[test]
    fn parse_c_config_option() {
        let args = s(&["-c", "user.name=test", "commit"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(2),
            }
        );
    }

    #[test]
    fn parse_git_dir_equals() {
        let args = s(&["--git-dir=/path/to/.git", "log"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    #[test]
    fn parse_git_dir_space() {
        let args = s(&["--git-dir", "/path/to/.git", "log"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(2),
            }
        );
    }

    #[test]
    fn parse_work_tree_equals() {
        let args = s(&["--work-tree=/path/to/worktree", "status"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    #[test]
    fn parse_work_tree_space() {
        let args = s(&["--work-tree", "/path/to/worktree", "status"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(2),
            }
        );
    }

    #[test]
    fn parse_no_pager() {
        let args = s(&["--no-pager", "diff"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    #[test]
    fn parse_bare() {
        let args = s(&["--bare", "init"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    #[test]
    fn parse_pathspec_options() {
        let args = s(&["--literal-pathspecs", "--icase-pathspecs", "add", "."]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(2),
            }
        );
    }

    #[test]
    fn parse_no_replace_objects() {
        let args = s(&["--no-replace-objects", "cat-file", "-p", "HEAD"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    #[test]
    fn parse_multiple_global_options() {
        let args = s(&[
            "-C",
            "/repo",
            "--no-pager",
            "-c",
            "color.ui=always",
            "--git-dir=/repo/.git",
            "worktree",
            "add",
            "/path",
        ]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: Some("/repo".to_string()),
                subcmd_index: Some(6),
            }
        );
    }

    #[test]
    fn parse_only_global_options_no_subcommand() {
        let args = s(&["-C", "/repo", "--no-pager"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: Some("/repo".to_string()),
                subcmd_index: None,
            }
        );
    }

    #[test]
    fn parse_c_without_value() {
        // -C at the end without a following argument
        let args = s(&["-C"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: None,
            }
        );
    }

    #[test]
    fn parse_glob_pathspecs() {
        let args = s(&["--glob-pathspecs", "ls-files"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    #[test]
    fn parse_noglob_pathspecs() {
        let args = s(&["--noglob-pathspecs", "ls-files"]);
        let result = parse_git_global_opts(&args);
        assert_eq!(
            result,
            ParsedGitArgs {
                work_dir: None,
                subcmd_index: Some(1),
            }
        );
    }

    // --- parse_branch_delete tests ---

    #[test]
    fn branch_delete_short_flag() {
        let args = s(&["branch", "-d", "my-branch"]);
        assert_eq!(parse_branch_delete(&args, 0), Some("my-branch".to_string()));
    }

    #[test]
    fn branch_delete_long_flag() {
        let args = s(&["branch", "--delete", "my-branch"]);
        assert_eq!(parse_branch_delete(&args, 0), Some("my-branch".to_string()));
    }

    #[test]
    fn branch_delete_force_flag() {
        let args = s(&["branch", "-D", "my-branch"]);
        assert_eq!(parse_branch_delete(&args, 0), Some("my-branch".to_string()));
    }

    #[test]
    fn branch_delete_no_name() {
        let args = s(&["branch", "-d"]);
        assert_eq!(parse_branch_delete(&args, 0), None);
    }

    #[test]
    fn branch_no_delete() {
        let args = s(&["branch", "-m", "old", "new"]);
        assert_eq!(parse_branch_delete(&args, 0), None);
    }

    #[test]
    fn branch_delete_with_global_opts_offset() {
        // Simulates: git -C /repo branch -d my-branch
        // where subcmd_index=2 (branch is at index 2)
        let args = s(&["-C", "/repo", "branch", "-d", "my-branch"]);
        assert_eq!(parse_branch_delete(&args, 2), Some("my-branch".to_string()));
    }
}
