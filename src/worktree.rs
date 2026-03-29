use std::path::{Path, PathBuf};

use crate::{jj, meta};

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
        match args[i].as_str() {
            "-b" | "-B" => {
                i += 1;
                if i >= args.len() {
                    return Err("-b requires a branch name".into());
                }
                branch = Some(args[i].clone());
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown option: {arg}").into());
            }
            _ => {
                if path.is_none() {
                    path = Some(args[i].clone());
                } else if commit_ish.is_none() {
                    commit_ish = Some(args[i].clone());
                } else {
                    return Err(format!("unexpected argument: {}", args[i]).into());
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

    // 2. If -b <branch> specified: jj bookmark set <branch> -r <wsname>@
    if let Some(ref branch) = parsed.branch {
        jj::run_stdout(
            Some(&repo_root),
            &["bookmark", "set", branch, "-r", &format!("{ws_name}@")],
        )?;
    }

    // 3. If <commit-ish> specified: jj -R <ws_abs_path> new <commit-ish>
    if let Some(ref commit_ish) = parsed.commit_ish {
        jj::run_stdout(Some(&ws_abs_path), &["new", commit_ish])?;
    }

    // 4. Save metadata
    let ws_meta = meta::WorkspaceMeta {
        workspace: ws_name.clone(),
        bookmark: parsed.branch.clone(),
        created_at: chrono::Utc::now(),
        path: ws_abs_path.clone(),
    };
    meta::save(&repo_root, &ws_meta)?;

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
    let ws_names: Vec<String> = ws_list_output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Format: "<wsname>: <change-id> <description>"
            trimmed.split(':').next().map(|s| s.trim().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect();

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
    if ws_name == "default" || ws_name == "main" {
        return Err(format!(
            "refusing to remove the '{ws_name}' workspace (main/default workspace is protected)"
        )
        .into());
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

    let ws_names: Vec<String> = ws_list_output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed.split(':').next().map(|s| s.trim().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect();

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
