use std::path::{Path, PathBuf};

use crate::{issue_log, jj, meta};

const PROTECTED_WORKSPACES: &[&str] = &["default", "main"];

/// `git worktree add` flags accepted as no-ops because their semantics do not
/// apply to `jj workspace`. Keeping these silent (no AI-directed warning)
/// prevents noise for routine calls. See DR-0001.
const KNOWN_NO_VALUE_FLAGS: &[&str] = &[
    "--track",
    "--no-track",
    "--lock",
    "--guess-remote",
    "--no-guess-remote",
    "--detach",
];

/// `git worktree add` flags that take a value, accepted as no-ops. The value
/// is consumed alongside the flag.
const KNOWN_VALUE_FLAGS: &[&str] = &["--reason"];

/// Parse workspace names from `jj workspace list` output.
/// Each line format: "<wsname>: <change-id> <description>"
fn parse_workspace_names(ws_list_output: &str) -> Vec<String> {
    ws_list_output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed.split(':').next().map(|s| s.trim().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// Entry point called from main.rs. args[0] is the subcommand name.
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("no subcommand provided".into());
    }
    match args[0].as_str() {
        "add" => cmd_add(&args[1..]),
        "list" => cmd_list(&args[1..]),
        "remove" => cmd_remove(&args[1..]),
        other => Err(format!("unknown worktree subcommand: {other}").into()),
    }
}

/// Parse add arguments: add <path> [-b <branch>] [<commit-ish>]
struct AddArgs {
    path: String,
    branch: Option<String>,
    commit_ish: Option<String>,
}

fn parse_add_args(args: &[String]) -> Result<AddArgs, Box<dyn std::error::Error>> {
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut commit_ish: Option<String> = None;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-b" | "-B" => {
                i += 1;
                if i >= args.len() {
                    return Err("-b requires a branch name".into());
                }
                branch = Some(args[i].clone());
            }
            a if KNOWN_NO_VALUE_FLAGS.contains(&a) => {
                jj::debug_message(&format!("ignoring known no-op flag: {a}"));
            }
            a if KNOWN_VALUE_FLAGS.contains(&a) => {
                jj::debug_message(&format!("ignoring known no-op flag with value: {a}"));
                i += 1; // consume the value, if present
            }
            a if a.starts_with("--") && a.contains('=') => {
                // --xxx=val: single-arg form. Accept as no-op and report.
                let opt_name = a.split_once('=').map(|(n, _)| n).unwrap_or(a);
                if !KNOWN_NO_VALUE_FLAGS.contains(&opt_name)
                    && !KNOWN_VALUE_FLAGS.contains(&opt_name)
                {
                    issue_log::report(
                        issue_log::IssueKind::UnknownOption,
                        "git worktree add",
                        Some(opt_name),
                        args,
                    );
                }
            }
            a if a.starts_with('-') => {
                // Unknown bare flag: accept as no-op (do not consume next arg
                // because we cannot tell if it is a value or a positional).
                issue_log::report(
                    issue_log::IssueKind::UnknownOption,
                    "git worktree add",
                    Some(a),
                    args,
                );
            }
            _ => {
                if path.is_none() {
                    path = Some(arg.clone());
                } else if commit_ish.is_none() {
                    commit_ish = Some(arg.clone());
                } else {
                    return Err(format!("unexpected argument: {arg}").into());
                }
            }
        }
        i += 1;
    }

    let path = path.ok_or("missing required argument: <path>")?;
    Ok(AddArgs {
        path,
        branch,
        commit_ish,
    })
}

/// `jj-worktree add <path> [-b <branch>] [<commit-ish>]`
fn cmd_add(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse_add_args(args)?;
    let cwd = std::env::current_dir()?;
    let repo_root =
        jj::find_repo_root(&cwd).ok_or("not inside a jj repository (no .jj directory found)")?;

    // Workspace name is the last component of the path
    let ws_path = Path::new(&parsed.path);
    let ws_name = ws_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid path: cannot determine workspace name")?
        .to_string();

    // 1. jj workspace add <path>
    jj::run_stdout(Some(&repo_root), &["workspace", "add", &parsed.path])?;

    // Resolve the absolute path of the created workspace
    let ws_abs_path = if ws_path.is_absolute() {
        ws_path.to_path_buf()
    } else {
        cwd.join(ws_path)
    };
    let ws_abs_path = normalize_path(&ws_abs_path);

    // Post-add setup: if anything fails, rollback the workspace
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // 2. If -b <branch> specified: jj bookmark set <branch> -r <wsname>@
        if let Some(ref branch) = parsed.branch {
            jj::run_stdout(
                Some(&repo_root),
                &["bookmark", "set", branch, "-r", &format!("{ws_name}@")],
            )?;
        }

        // 3. If <commit-ish> specified: jj -R <ws_abs_path> new <commit-ish>
        if let Some(ref commit_ish) = parsed.commit_ish {
            let jj_rev = translate_git_ref(&repo_root, commit_ish);
            if jj_rev != *commit_ish {
                jj::debug_message(&format!("translated git ref: {commit_ish} -> {jj_rev}"));
            }
            jj::run_stdout(Some(&ws_abs_path), &["new", &jj_rev])?;
        }

        // 4. Save metadata
        let ws_meta = meta::WorkspaceMeta {
            workspace: ws_name.clone(),
            bookmark: parsed.branch.clone(),
            created_at: chrono::Utc::now(),
            path: ws_abs_path.clone(),
        };
        meta::save(&repo_root, &ws_meta)?;

        // Sync jj state to git refs
        if let Err(e) = jj::run(Some(&repo_root), &["git", "export"]) {
            jj::debug_message(&format!("jj git export failed: {e}"));
        }

        Ok(())
    })();

    if let Err(e) = result {
        // Cleanup: forget the workspace we just created
        jj::debug_message(&format!("workspace setup failed, rolling back: {e}"));
        let _ = jj::run(Some(&repo_root), &["workspace", "forget", &ws_name]);
        if ws_abs_path.exists() {
            let _ = std::fs::remove_dir_all(&ws_abs_path);
        }
        return Err(e);
    }

    eprintln!(
        "Created workspace '{}' at {}",
        ws_name,
        ws_abs_path.display()
    );
    Ok(())
}

/// `jj-worktree list`
fn cmd_list(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // Check for --help but otherwise no args expected
    if args.iter().any(|a| a.starts_with('-') && a != "--") {
        return Err("list takes no options".into());
    }

    let cwd = std::env::current_dir()?;
    let repo_root =
        jj::find_repo_root(&cwd).ok_or("not inside a jj repository (no .jj directory found)")?;

    // Get workspace list from jj
    let ws_list_output = jj::run_stdout(Some(&repo_root), &["workspace", "list"])?;

    // Parse workspace names: each line starts with "<wsname>: ..."
    let ws_names = parse_workspace_names(&ws_list_output);

    if ws_names.is_empty() {
        return Ok(());
    }

    // Collect output lines so we can align columns
    let mut entries: Vec<(String, String, String)> = Vec::new(); // (path, commit, name)

    for ws_name in &ws_names {
        // Get workspace path using jj root -R approach
        // First try to find it via metadata, then fall back to jj root
        let ws_path = get_workspace_path(&repo_root, ws_name)?;

        // Get commit info: commit_id short + bookmarks
        let commit_info = jj::run_stdout(
            Some(&repo_root),
            &[
                "log",
                "-r",
                &format!("'{ws_name}@'"),
                "--no-graph",
                "-T",
                r#"commit_id.short(7) ++ " " ++ local_bookmarks.map(|b| b.name()).join(",")"#,
                "--workspace",
                ws_name,
            ],
        )
        .unwrap_or_default();

        let commit_info = commit_info.trim().to_string();
        let (commit_hash, bookmarks) = match commit_info.split_once(' ') {
            Some((hash, bm)) => (hash.to_string(), bm.to_string()),
            None => (commit_info.clone(), String::new()),
        };

        let display_name = if bookmarks.is_empty() {
            format!("[{ws_name}]")
        } else {
            format!("[{bookmarks}]")
        };

        entries.push((ws_path, commit_hash, display_name));
    }

    // Calculate column widths for alignment
    let max_path_len = entries.iter().map(|(p, _, _)| p.len()).max().unwrap_or(0);
    let max_hash_len = entries.iter().map(|(_, h, _)| h.len()).max().unwrap_or(0);

    for (path, hash, name) in &entries {
        println!(
            "{:<path_width$} {:<hash_width$} {name}",
            path,
            hash,
            path_width = max_path_len,
            hash_width = max_hash_len,
        );
    }

    Ok(())
}

/// Get the absolute path of a workspace.
/// Tries metadata first, then falls back to repo_root/ws_name convention.
fn get_workspace_path(
    repo_root: &Path,
    ws_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Try metadata first
    if let Some(ws_meta) = meta::load(repo_root, ws_name)? {
        return Ok(ws_meta.path.to_string_lossy().to_string());
    }

    // Fall back: the workspace is typically at repo_root/ws_name
    // For the default workspace, it's the repo_root itself
    if ws_name == "default" {
        return Ok(repo_root.to_string_lossy().to_string());
    }

    let candidate = repo_root.join(ws_name);
    if candidate.exists() {
        let canonical = candidate.canonicalize()?;
        return Ok(canonical.to_string_lossy().to_string());
    }

    // Last resort: just report the expected path
    Ok(candidate.to_string_lossy().to_string())
}

/// `jj-worktree remove [--force] <path>`
fn cmd_remove(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut force = false;
    let mut path: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            a if a.starts_with('-') => {
                return Err(format!("unknown option: {a}").into());
            }
            _ => {
                if path.is_some() {
                    return Err(format!("unexpected argument: {arg}").into());
                }
                path = Some(arg.clone());
            }
        }
    }

    let path_str = path.ok_or("missing required argument: <path>")?;
    let cwd = std::env::current_dir()?;
    let repo_root =
        jj::find_repo_root(&cwd).ok_or("not inside a jj repository (no .jj directory found)")?;

    // 1. Resolve to canonical path
    let target_path = resolve_path(&cwd, &path_str)?;

    // 5. Verify the path is under the jj repo root
    let canonical_repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());
    if !target_path.starts_with(&canonical_repo_root) {
        return Err(format!(
            "path {} is not inside the jj repository at {}",
            target_path.display(),
            canonical_repo_root.display()
        )
        .into());
    }

    // 2. Find workspace name by reverse-lookup from jj workspace list
    let ws_name = find_workspace_name(&repo_root, &target_path)?;

    // 4. Protect main/default workspace
    if PROTECTED_WORKSPACES.contains(&ws_name.as_str()) {
        return Err(
            format!("refusing to remove the '{ws_name}' workspace (protected workspace)").into(),
        );
    }

    // 3. Load metadata for bookmark info
    let ws_meta = meta::load(&repo_root, &ws_name)?;

    // Check for uncommitted changes (unless --force)
    if !force {
        // Check if workspace has modifications by examining jj status
        let status_output = jj::run_stdout(
            Some(&repo_root),
            &[
                "diff",
                "-r",
                &format!("'{ws_name}@'"),
                "--stat",
                "--workspace",
                &ws_name,
            ],
        );
        if let Ok(ref status) = status_output
            && !status.trim().is_empty()
        {
            return Err(format!(
                "workspace '{}' has uncommitted changes. Use --force to remove anyway.",
                ws_name
            )
            .into());
        }
    }

    // Deletion steps:

    // 1. Delete bookmark recorded in metadata
    if let Some(ref meta) = ws_meta
        && let Some(ref bookmark) = meta.bookmark
    {
        let result = jj::run_stdout(Some(&repo_root), &["bookmark", "delete", bookmark]);
        if let Err(e) = result {
            eprintln!("warning: failed to delete bookmark '{}': {}", bookmark, e);
        }
    }

    // 2. jj workspace forget <wsname>
    jj::run_stdout(Some(&repo_root), &["workspace", "forget", &ws_name])?;

    // 3. Remove the directory
    if target_path.exists() {
        std::fs::remove_dir_all(&target_path).map_err(|e| {
            format!(
                "failed to remove directory {}: {}",
                target_path.display(),
                e
            )
        })?;
    }

    // 4. Remove metadata
    meta::remove(&repo_root, &ws_name)?;

    // Sync jj state to git refs
    if let Err(e) = jj::run(Some(&repo_root), &["git", "export"]) {
        jj::debug_message(&format!("jj git export failed: {e}"));
    }

    eprintln!(
        "Removed workspace '{}' at {}",
        ws_name,
        target_path.display()
    );
    Ok(())
}

/// Find workspace name by matching the canonical path against known workspaces.
fn find_workspace_name(
    repo_root: &Path,
    target_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let ws_list_output = jj::run_stdout(Some(repo_root), &["workspace", "list"])?;
    let ws_names = parse_workspace_names(&ws_list_output);

    // Try to match by path
    for ws_name in &ws_names {
        let ws_path = get_workspace_path(repo_root, ws_name)?;
        let ws_canonical = PathBuf::from(&ws_path);
        let ws_canonical = ws_canonical
            .canonicalize()
            .unwrap_or_else(|_| ws_canonical.clone());

        if ws_canonical == target_path {
            return Ok(ws_name.clone());
        }
    }

    // Fall back: try to match by the last component of the path
    jj::debug_message(&format!(
        "workspace path match failed, falling back to name match for '{}'",
        target_path.display()
    ));
    if let Some(dir_name) = target_path.file_name().and_then(|n| n.to_str()) {
        for ws_name in &ws_names {
            if ws_name == dir_name {
                return Ok(ws_name.clone());
            }
        }
    }

    Err(format!("no workspace found for path: {}", target_path.display()).into())
}

/// Resolve a path relative to cwd, returning a canonical (absolute) path.
fn resolve_path(cwd: &Path, path_str: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let p = Path::new(path_str);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    // Try canonicalize first; if the path doesn't exist yet, normalize manually
    match abs.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(_) => Ok(normalize_path(&abs)),
    }
}

/// Translate a git-style remote reference to jj-style.
/// E.g., "origin/main" -> "main@origin", "origin/HEAD" -> "trunk()"
/// Non-remote refs are returned as-is.
fn translate_git_ref(repo_root: &Path, git_ref: &str) -> String {
    // Bare "HEAD" → jj's "@" (current working copy)
    if git_ref == "HEAD" {
        return "@".to_string();
    }
    if let Some((maybe_remote, branch)) = git_ref.split_once('/') {
        if branch == "HEAD" {
            return "trunk()".to_string();
        }
        // Check if branch@remote exists as a jj revision
        let jj_ref = format!("{branch}@{maybe_remote}");
        if jj::run_stdout(
            Some(repo_root),
            &[
                "log",
                "-r",
                &jj_ref,
                "--no-graph",
                "-T",
                "commit_id.short(7)",
            ],
        )
        .is_ok()
        {
            return jj_ref;
        }
        jj::debug_message(&format!(
            "git ref '{git_ref}' not resolved as jj ref '{jj_ref}', using as-is"
        ));
    }
    git_ref.to_string()
}

/// Normalize a path by resolving `.` and `..` components without requiring the path to exist.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                components.push(component);
            }
        }
    }
    components.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    /// Quiet the AI-directed stderr block emitted by `issue_log::report` and
    /// redirect the JSONL log to a tempfile so tests don't pollute the real
    /// `~/.local/state/jj-worktree/issues.log`.
    struct IssueLogGuard {
        log_file: PathBuf,
        prev_log: Option<std::ffi::OsString>,
        prev_quiet: Option<std::ffi::OsString>,
    }
    impl IssueLogGuard {
        fn new() -> Self {
            let key = "JJ_WORKTREE_ISSUE_LOG";
            let qkey = "JJ_WORKTREE_ISSUE_QUIET";
            let prev_log = std::env::var_os(key);
            let prev_quiet = std::env::var_os(qkey);
            let log_file = std::env::temp_dir().join(format!(
                "jj-worktree-parse-add-args-test-{}-{}.log",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            unsafe {
                std::env::set_var(key, &log_file);
                std::env::set_var(qkey, "1");
            }
            Self {
                log_file,
                prev_log,
                prev_quiet,
            }
        }
    }
    impl Drop for IssueLogGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.log_file);
            unsafe {
                let key = "JJ_WORKTREE_ISSUE_LOG";
                let qkey = "JJ_WORKTREE_ISSUE_QUIET";
                match self.prev_log.take() {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
                match self.prev_quiet.take() {
                    Some(v) => std::env::set_var(qkey, v),
                    None => std::env::remove_var(qkey),
                }
            }
        }
    }

    #[test]
    fn parse_add_args_basic_path() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
        assert!(parsed.branch.is_none());
        assert!(parsed.commit_ish.is_none());
    }

    #[test]
    fn parse_add_args_with_branch() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["-b", "feat/x", "../foo"])).unwrap();
        assert_eq!(parsed.branch, Some("feat/x".to_string()));
        assert_eq!(parsed.path, "../foo");
    }

    #[test]
    fn parse_add_args_with_uppercase_b_branch() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["-B", "feat/x", "../foo"])).unwrap();
        assert_eq!(parsed.branch, Some("feat/x".to_string()));
        assert_eq!(parsed.path, "../foo");
    }

    #[test]
    fn parse_add_args_with_commit_ish() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["../foo", "main"])).unwrap();
        assert_eq!(parsed.path, "../foo");
        assert_eq!(parsed.commit_ish, Some("main".to_string()));
    }

    #[test]
    fn parse_add_args_no_track_is_accepted_as_noop() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["--no-track", "../foo", "main"])).unwrap();
        assert_eq!(parsed.path, "../foo");
        assert_eq!(parsed.commit_ish, Some("main".to_string()));
    }

    #[test]
    fn parse_add_args_track_is_accepted_as_noop() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["--track", "../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
    }

    #[test]
    fn parse_add_args_detach_is_accepted_as_noop() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["--detach", "../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
    }

    #[test]
    fn parse_add_args_lock_is_accepted_as_noop() {
        let _g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["--lock", "../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
    }

    #[test]
    fn parse_add_args_reason_consumes_value() {
        let _g = IssueLogGuard::new();
        // --reason VAL must consume VAL, leaving "../foo" as the path.
        let parsed = parse_add_args(&s(&["--reason", "doing-stuff", "../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
        assert!(parsed.commit_ish.is_none());
    }

    #[test]
    fn parse_add_args_guess_remote_variants_accepted() {
        let _g = IssueLogGuard::new();
        assert!(parse_add_args(&s(&["--guess-remote", "../foo"])).is_ok());
        assert!(parse_add_args(&s(&["--no-guess-remote", "../foo"])).is_ok());
    }

    #[test]
    fn parse_add_args_unknown_flag_is_accepted_with_log() {
        let g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["--brand-new-flag", "../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
        // Verify the JSONL log captured the unknown option.
        let content = std::fs::read_to_string(&g.log_file).expect("log written");
        assert!(content.contains("unknown_option"));
        assert!(content.contains("--brand-new-flag"));
    }

    #[test]
    fn parse_add_args_unknown_equals_form_is_accepted_with_log() {
        let g = IssueLogGuard::new();
        let parsed = parse_add_args(&s(&["--brand-new-flag=val", "../foo"])).unwrap();
        assert_eq!(parsed.path, "../foo");
        let content = std::fs::read_to_string(&g.log_file).expect("log written");
        // The option name (without "=val") should be reported.
        assert!(content.contains("--brand-new-flag"));
        // The "=val" suffix must not break path parsing — single-arg consumption.
        assert!(parsed.commit_ish.is_none());
    }

    #[test]
    fn parse_add_args_known_flag_does_not_log() {
        let g = IssueLogGuard::new();
        let _ = parse_add_args(&s(&["--no-track", "../foo"])).unwrap();
        // The log file must not be created for known flags.
        assert!(
            !g.log_file.exists() || std::fs::read_to_string(&g.log_file).unwrap().is_empty(),
            "known no-op flags should not trigger issue logging"
        );
    }

    #[test]
    fn parse_add_args_missing_path_errors() {
        let _g = IssueLogGuard::new();
        assert!(parse_add_args(&s(&[])).is_err());
        assert!(parse_add_args(&s(&["--no-track"])).is_err());
    }

    #[test]
    fn parse_add_args_b_without_value_errors() {
        let _g = IssueLogGuard::new();
        assert!(parse_add_args(&s(&["-b"])).is_err());
    }

    #[test]
    fn parse_add_args_too_many_positionals_errors() {
        let _g = IssueLogGuard::new();
        assert!(parse_add_args(&s(&["a", "b", "c"])).is_err());
    }
}
