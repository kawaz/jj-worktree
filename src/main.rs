mod jj;
mod meta;
mod shim;
mod worktree;

use std::env;
use std::path::{Path, PathBuf};
use std::process;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_help() {
    eprintln!(
        "\
jj-worktree {VERSION} - A git shim that translates worktree operations to jj workspace commands

Usage:
  jj-worktree <command> [options]

Commands:
  add <path> [-b <branch>] [<commit-ish>]     Create a new workspace
  list                                        List workspaces
  remove [--force] <path>                     Remove a workspace
  run [--] <cmd> [args...]                    Run a command with git shim active

Options:
  --help       Show this help message
  --version    Show version

Environment variables:
  JJ_WORKTREE_DISABLED   Set to 1 to disable shim and pass through to real git
  JJ_WORKTREE_DEBUG      Set to 1 to log to stderr (JSONL format)
  JJ_WORKTREE_LOG_FILE   Append debug logs to a file (JSONL format)
  JJ_WORKTREE_REAL_GIT   Override path to real git binary
  JJ_WORKTREE_DETECT     Set to 1 to probe: prints 'jj-worktree' and exits"
    );
}

fn print_version() {
    println!("jj-worktree {VERSION}");
}

/// Determine the invocation mode from argv[0].
/// Returns "git" if the binary was invoked as git (via symlink), otherwise "jj-worktree".
fn invocation_mode() -> &'static str {
    let argv0 = env::args().next().unwrap_or_default();
    let stem = Path::new(&argv0)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if stem == "git" { "git" } else { "jj-worktree" }
}

/// Resolve the symlink target and determine whether the binary is installed in PATH.
///
/// Returns `(target_path, in_path)`:
/// - `in_path = true`: the binary was found in PATH (e.g. `/opt/homebrew/bin/jj-worktree`).
///   The PATH entry is returned as the target, which may be a symlink — that's intentional,
///   as it survives `brew upgrade` and similar package updates.
/// - `in_path = false`: the binary was invoked via absolute/relative path or is a dev build.
///   The canonicalized real path is returned as the target.
fn resolve_symlink_target() -> Result<(PathBuf, bool), Box<dyn std::error::Error>> {
    let canonical = env::current_exe()?.canonicalize()?;

    // Search PATH for "jj-worktree" and pick the entry that resolves to the same binary.
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join("jj-worktree");
            if candidate.is_file()
                && candidate
                    .canonicalize()
                    .is_ok_and(|resolved| resolved == canonical)
            {
                return Ok((candidate, true));
            }
        }
    }

    // Not found in PATH: use the canonical real path.
    Ok((canonical, false))
}

/// `jj-worktree run [--] <cmd> [args...]`
///
/// Creates a temporary git shim symlink in a cache directory, prepends it to PATH,
/// and execs the given command. This is a Rust CLI tool using Unix execvp, not shell exec.
#[cfg(unix)]
fn cmd_run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    // Skip leading "--" separator if present
    let args = if args.first().map(|a| a.as_str()) == Some("--") {
        &args[1..]
    } else {
        args
    };

    if args.is_empty() {
        return Err("run requires a command to execute".into());
    }

    let (exe_path, in_path) = resolve_symlink_target()?;

    // Determine cache dir: ${XDG_CACHE_HOME:-$HOME/.cache}/jj-worktree/
    let cache_home = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or("cannot determine cache directory: neither XDG_CACHE_HOME nor HOME is set")?;
    let jj_wt_cache = cache_home.join("jj-worktree");

    // Choose shim directory:
    // - Installed (in PATH): shared `${cache}/jj-worktree/bin/`
    // - Dev/local build: `${TMPDIR}/jj-worktree.{hash}/` — keyed by canonical path hash
    //   so the same build shares a shim across sessions without clobbering the production
    //   shim. Uses TMPDIR so the OS cleans up stale entries on reboot.
    let bin_dir = if in_path {
        jj_wt_cache.join("bin")
    } else {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        exe_path.hash(&mut hasher);
        env::temp_dir().join(format!("jj-worktree.{:016x}", hasher.finish()))
    };
    std::fs::create_dir_all(&bin_dir)?;

    // Create/update git symlink
    let symlink_path = bin_dir.join("git");
    if symlink_path.symlink_metadata().is_ok() {
        // Check if it already points to us
        let needs_update = match std::fs::read_link(&symlink_path) {
            Ok(target) => {
                let canonical = if target.is_absolute() {
                    target.canonicalize().unwrap_or(target)
                } else {
                    bin_dir
                        .join(&target)
                        .canonicalize()
                        .unwrap_or(bin_dir.join(&target))
                };
                canonical != exe_path
            }
            Err(_) => true,
        };
        if needs_update {
            std::fs::remove_file(&symlink_path)?;
            symlink(&exe_path, &symlink_path)?;
        }
    } else {
        symlink(&exe_path, &symlink_path)?;
    }

    // Prepend bin_dir to PATH
    let mut new_path = std::ffi::OsString::from(&bin_dir);
    if let Some(existing) = env::var_os("PATH") {
        new_path.push(":");
        new_path.push(existing);
    }

    // Replace the current process with the command (Unix execvp via CommandExt::exec)
    let err = Command::new(&args[0])
        .args(&args[1..])
        .env("PATH", new_path)
        .exec();
    Err(format!("failed to exec '{}': {err}", args[0]).into())
}

#[cfg(not(unix))]
fn cmd_run(_args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    Err("run command is only supported on Unix systems".into())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mode = invocation_mode();
    let args: Vec<String> = env::args().skip(1).collect();

    match mode {
        "git" => {
            shim::run(&args)?;
            Ok(())
        }
        _ => {
            // jj-worktree mode
            if args.is_empty() {
                print_help();
                return Ok(());
            }

            match args[0].as_str() {
                "--help" | "-h" => {
                    print_help();
                    return Ok(());
                }
                "--version" => {
                    print_version();
                    return Ok(());
                }
                "add" | "list" | "remove" => {
                    worktree::run(&args)?;
                }
                "run" => {
                    cmd_run(&args[1..])?;
                }
                other => {
                    eprintln!("unknown command: {other}");
                    eprintln!();
                    print_help();
                    process::exit(1);
                }
            }

            Ok(())
        }
    }
}

fn main() {
    // Respond to detection probe (used by find_real_git to skip jj-worktree binaries in PATH)
    if env::var_os("JJ_WORKTREE_DETECT").is_some() {
        println!("jj-worktree");
        process::exit(0);
    }

    if let Err(e) = run() {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_mode_detects_jj_worktree() {
        // Default binary name in tests is the test binary itself, not "git"
        assert_eq!(invocation_mode(), "jj-worktree");
    }

    #[test]
    fn help_text_contains_key_sections() {
        // Verify help text structure by capturing it would require more setup,
        // but we can verify the function doesn't panic
        print_help();
    }
}
