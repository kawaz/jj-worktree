use std::path::{Path, PathBuf};

use crate::{issue_log, jj, meta};

const PROTECTED_WORKSPACES: &[&str] = &["default", "main"];

/// `git worktree add` flags accepted as no-ops because their semantics do not
/// apply to `jj workspace`. Keeping these silent (no AI-directed warning)
/// prevents noise for routine calls. See DR-0001.
///
/// FUTURE (DR-0001): `--detach` is currently a no-op, but `git worktree add
/// --detach <path> [<commit>]` semantically means "create the worktree at the
/// given commit without setting up a branch". The natural jj counterpart is
/// `jj new <commit>` inside the new workspace without `jj bookmark set`. If a
/// caller relies on detached behaviour we may want to honour it; until then,
/// the no-op is safe because jj already starts the workspace on a fresh empty
/// change rooted at the specified commit-ish.
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

/// Build the full argv we report to `issue_log` so that the JSONL entry
/// matches the documented shape (`git worktree add ...`) regardless of which
/// subcommand-tail slice the parser was handed. See DR-0002 sample.
fn full_argv(tail: &[String]) -> Vec<String> {
    let mut v = vec!["git".to_string(), "worktree".to_string(), "add".to_string()];
    v.extend(tail.iter().cloned());
    v
}

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

/// Entry point called from main.rs and shim.rs. args[0] is the subcommand name.
///
/// `cwd` is the working directory used to locate the jj repository. When the
/// shim is invoked as `git -C <path> worktree ...`, callers must pass `<path>`
/// here so that the subcommands resolve the right `.jj` (and not the
/// process-level current directory, which may be elsewhere).
pub fn run(cwd: &Path, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("no subcommand provided".into());
    }
    match args[0].as_str() {
        "add" => cmd_add(cwd, &args[1..]),
        "list" => cmd_list(cwd, &args[1..]),
        "remove" => cmd_remove(cwd, &args[1..]),
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
                        &full_argv(args),
                    );
                }
            }
            a if a.starts_with('-') => {
                // Unknown bare flag: accept as no-op (do not consume next arg
                // because we cannot tell if it is a value or a positional).
                // Hint the caller toward `--xxx=val` form so a value, if any,
                // does not get silently consumed as a path/commit-ish.
                eprintln!(
                    "hint: jj-worktree treats `{a}` as a single flag and does \
                     not consume the next argument as a value. If `{a}` should \
                     take a value, pass it as `{a}=<value>` to keep positional \
                     arguments intact."
                );
                issue_log::report(
                    issue_log::IssueKind::UnknownOption,
                    "git worktree add",
                    Some(a),
                    &full_argv(args),
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
fn cmd_add(cwd: &Path, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse_add_args(args)?;
    let repo_root =
        jj::find_repo_root(cwd).ok_or("not inside a jj repository (no .jj directory found)")?;

    // Workspace name is the last component of the path
    let ws_path = Path::new(&parsed.path);
    let ws_name = ws_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid path: cannot determine workspace name")?
        .to_string();

    // Match real git, which creates any missing leading directories for the
    // worktree path. `jj workspace add` requires the destination's parent to
    // exist, so a caller that passes e.g. `.kilo/worktrees/<name>` fails when
    // `.kilo/worktrees` is absent. (An existing, empty destination is fine.)
    let dest = if ws_path.is_absolute() {
        ws_path.to_path_buf()
    } else {
        cwd.join(ws_path)
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

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
fn cmd_list(cwd: &Path, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // Check for --help but otherwise no args expected
    if args.iter().any(|a| a.starts_with('-') && a != "--") {
        return Err("list takes no options".into());
    }

    let repo_root =
        jj::find_repo_root(cwd).ok_or("not inside a jj repository (no .jj directory found)")?;

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

        // Get commit info: commit_id short + bookmarks.
        // Note: pass `<ws_name>@` as a bare argument — jj receives argv directly
        // (no shell interpretation), so wrapping it in single quotes would be
        // taken as a literal `'<ws_name>@'` and revision lookup would fail.
        // `--workspace` is not a flag on `jj log`; the workspace is selected by
        // the `<ws>@` revset itself.
        let commit_info = jj::run_stdout(
            Some(&repo_root),
            &[
                "log",
                "-r",
                &format!("{ws_name}@"),
                "--no-graph",
                "-T",
                r#"commit_id.short(7) ++ " " ++ local_bookmarks.map(|b| b.name()).join(",")"#,
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

/// `jj-worktree remove [-f | --force] <path>`
fn cmd_remove(cwd: &Path, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut force = false;
    let mut path: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--force" | "-f" => force = true,
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
    let repo_root =
        jj::find_repo_root(cwd).ok_or("not inside a jj repository (no .jj directory found)")?;

    // 1. Resolve to canonical path
    let target_path = resolve_path(cwd, &path_str)?;

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

    // Check for uncommitted changes (unless --force).
    // Use `jj log -T empty` rather than `jj diff --stat` because diff --stat
    // always prints a "0 files changed, …" summary line even for empty
    // changes, which is a non-empty string and would falsely block removal.
    if !force {
        let empty_check = jj::run_stdout(
            Some(&repo_root),
            &[
                "log",
                "-r",
                &format!("{ws_name}@"),
                "--no-graph",
                "-T",
                "empty",
            ],
        );
        if let Ok(ref s) = empty_check
            && s.trim() != "true"
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

/// Whether `git worktree prune <args>` is a dry-run (`--dry-run`, `-n`, or a
/// short-option cluster containing `n`, e.g. `-vn`). A dry-run is a read-only
/// preview: it must not mutate jj state either.
pub fn prune_is_dry_run(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "--dry-run" || (a.starts_with('-') && !a.starts_with("--") && a.contains('n'))
    })
}

/// Real git's view of the worktree admin records, mirroring the decision the
/// upcoming `git worktree prune` will make.
///
/// - `tracked`: worktree roots real git has admin records for.
/// - `prunable`: worktree roots real git *this invocation* would prune
///   (from its `--dry-run` report on stderr, so `--expire` and lock semantics
///   come from real git itself rather than being reimplemented here).
///
/// `None` when real git is unavailable or the dry-run fails: the caller then
/// falls back to forgetting every directory-gone workspace (the behavior
/// before mirroring was introduced).
struct GitPruneView {
    tracked: std::collections::HashSet<PathBuf>,
    prunable: std::collections::HashSet<PathBuf>,
}

fn git_prune_view(repo_root: &Path, prune_args: &[String]) -> Option<GitPruneView> {
    let git = crate::shim::find_real_git().ok()?;
    let output = std::process::Command::new(&git)
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "prune", "--dry-run"])
        .args(prune_args)
        .output()
        .ok()?;
    if !output.status.success() {
        jj::debug_message(&format!(
            "prune reap: dry-run failed ({:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return None;
    }
    // The dry-run report ("Removing worktrees/<id>: <reason>") goes to
    // stderr, not stdout (verified against git 2.55); parse both streams so
    // the mirror follows whichever channel git emits.
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    jj::debug_message(&format!("prune reap: dry-run report: {report:?}"));
    let mut prunable = std::collections::HashSet::new();
    for line in report.lines() {
        // Dry-run prints "Removing worktrees/<id>: <reason>" per record that
        // this prune would remove. Locked or not-yet-expired records get no
        // such line.
        let Some(rest) = line.strip_prefix("Removing worktrees/") else {
            continue;
        };
        let Some((id, _reason)) = rest.split_once(':') else {
            continue;
        };
        if let Some(path) = worktree_path_for_record(repo_root, id.trim()) {
            prunable.insert(path);
        }
    }
    let mut tracked = std::collections::HashSet::new();
    let admin_dir = repo_root.join(".git").join("worktrees");
    if let Ok(entries) = std::fs::read_dir(&admin_dir) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().to_string();
            if let Some(path) = worktree_path_for_record(repo_root, &id) {
                tracked.insert(path);
            }
        }
    }
    Some(GitPruneView { tracked, prunable })
}

/// The worktree root path recorded in `<git_common_dir>/worktrees/<id>/gitdir`.
///
/// The file holds the path of the worktree's `.git` file, either absolute or
/// relative to the record directory. Returns `None` when the record cannot be
/// read or the path cannot be determined.
fn worktree_path_for_record(repo_root: &Path, id: &str) -> Option<PathBuf> {
    let record_dir = repo_root.join(".git").join("worktrees").join(id);
    let content = std::fs::read_to_string(record_dir.join("gitdir")).ok()?;
    let git_file = Path::new(content.trim());
    let resolved = if git_file.is_absolute() {
        git_file.to_path_buf()
    } else {
        record_dir.join(git_file)
    };
    let resolved = normalize_path(&resolved);
    // The gitdir points at the worktree's `.git` file; the worktree root is
    // its parent directory.
    if resolved.file_name().and_then(|n| n.to_str()) == Some(".git") {
        resolved.parent().map(|p| p.to_path_buf())
    } else {
        Some(resolved)
    }
}

/// Reap jj workspaces whose working directory no longer exists.
///
/// Callers that delete a worktree by renaming its directory aside and running
/// `git worktree prune` (Kilo Code / Agent Manager does exactly this) only
/// clean git's own admin records: the colocated jj workspace record then
/// lingers until a manual `jj workspace forget`. Workspaces created without
/// this shim (no `.jj-worktree-meta` entry) are covered too, because the
/// recorded path comes from `jj workspace root --name` itself.
///
/// The reap mirrors real git's prune decision (see [`GitPruneView`]) so the
/// two sides never disagree about which worktrees exist: a workspace whose
/// admin record real git keeps (`--expire` not reached, or the record is
/// locked) keeps its jj workspace record, while one git prunes -- or one with
/// no git admin record at all -- is forgotten. Dry-runs never reach this
/// function (see [`prune_is_dry_run`]).
///
/// Best effort: failures are reported on stderr and skipped, never
/// propagated, so the subsequent real-git prune is not disturbed. Only the
/// protected workspaces are exempt.
pub fn reap_forgotten_workspaces(cwd: &Path, prune_args: &[String]) {
    let repo_root = match jj::find_repo_root(cwd) {
        Some(root) => root,
        None => return,
    };
    let ws_list_output = match jj::run_stdout(Some(&repo_root), &["workspace", "list"]) {
        Ok(output) => output,
        Err(_) => return,
    };
    let prune_view = git_prune_view(&repo_root, prune_args);
    for ws_name in parse_workspace_names(&ws_list_output) {
        if PROTECTED_WORKSPACES.contains(&ws_name.as_str()) {
            continue;
        }
        // Ask jj for the recorded path instead of consulting metadata, so
        // workspaces created outside this shim (no .jj-worktree-meta entry)
        // are reaped as well.
        let recorded =
            match jj::run_stdout(Some(&repo_root), &["workspace", "root", "--name", &ws_name]) {
                Ok(path) => PathBuf::from(path),
                Err(e) => {
                    jj::debug_message(&format!(
                        "prune reap: cannot resolve path of workspace '{ws_name}': {e}"
                    ));
                    continue;
                }
            };
        let recorded = normalize_path(Path::new(&recorded));
        if recorded.exists() {
            continue;
        }
        // Mirror real git: keep the jj record for a worktree whose admin
        // record real git is keeping (locked, or not yet expired).
        if let Some(view) = &prune_view
            && view.tracked.contains(&recorded)
            && !view.prunable.contains(&recorded)
        {
            jj::debug_message(&format!(
                "prune reap: real git keeps the record of workspace '{ws_name}', skipping"
            ));
            continue;
        }
        if let Err(e) = jj::run_stdout(Some(&repo_root), &["workspace", "forget", &ws_name]) {
            eprintln!("warning: failed to forget workspace '{ws_name}': {e}");
            continue;
        }
        // Best effort: drop the stale metadata file if one exists.
        let _ = meta::remove(&repo_root, &ws_name);
        eprintln!("Forgotten workspace '{ws_name}' at {}", recorded.display());
    }
}

/// Whether a shim-mode `git worktree remove <args>` targets a workspace
/// registered with jj.
///
/// Git-native worktrees (created via the `add --detach` / `-d` / `--orphan`
/// passthrough) are unknown to jj; translating their removal would fail with
/// "no workspace found for path" and leave the worktree behind. Such targets
/// belong to real git, which applies its own safety checks (uncommitted
/// changes, locked worktrees, main-worktree protection). Direct-mode
/// `jj-worktree remove` is unaffected: it always runs jj semantics and errors
/// on unregistered paths.
pub fn remove_targets_registered_workspace(cwd: &Path, args: &[String]) -> bool {
    // args[0] is the "remove" head; the target is the first positional, and
    // flags (`--force` / `-f`) do not affect it.
    let rest = if args.first().map(String::as_str) == Some("remove") {
        &args[1..]
    } else {
        args
    };
    let mut path: Option<&String> = None;
    for arg in rest {
        if arg.starts_with('-') {
            continue;
        }
        if path.is_none() {
            path = Some(arg);
        }
    }
    let Some(path_str) = path else {
        return false;
    };
    let Some(repo_root) = jj::find_repo_root(cwd) else {
        return false;
    };
    let Ok(target_path) = resolve_path(cwd, path_str) else {
        return false;
    };
    // Mirror cmd_remove's containment check so a path outside the repository
    // is never translated on the strength of the basename fallback alone.
    let canonical_repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());
    if !target_path.starts_with(&canonical_repo_root) {
        return false;
    }
    find_workspace_name(&repo_root, &target_path).is_ok()
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

    /// Quiet the AI-directed stderr block emitted by `issue_log::report`,
    /// redirect the JSONL log to a tempfile, and hold the crate-wide test env
    /// lock for the lifetime of the guard so concurrent tests cannot trample
    /// the env vars we're mutating.
    struct IssueLogGuard {
        log_file: PathBuf,
        prev_log: Option<std::ffi::OsString>,
        prev_quiet: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl IssueLogGuard {
        fn new() -> Self {
            let lock = crate::test_util::env_lock();
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
                _lock: lock,
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

    #[test]
    fn prune_is_dry_run_matches_dry_run_flags() {
        assert!(prune_is_dry_run(&s(&["-n"])));
        assert!(prune_is_dry_run(&s(&["--dry-run"])));
        assert!(prune_is_dry_run(&s(&["-vn"])));
        assert!(prune_is_dry_run(&s(&["--expire", "now", "-n"])));
        assert!(!prune_is_dry_run(&s(&[])));
        assert!(!prune_is_dry_run(&s(&["--expire", "now"])));
        assert!(!prune_is_dry_run(&s(&["-v"])));
        // Long options must not be treated as short clusters: `--no-*` does
        // not contain the dry-run flag.
        assert!(!prune_is_dry_run(&s(&["--verbose"])));
    }
}
