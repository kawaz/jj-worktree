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
  add <path> [-b <branch>] [<commit-ish>]   Create a new workspace
  list                                       List workspaces
  remove [--force] <path>                    Remove a workspace
  setup [--path <dir>]                       Create a 'git' symlink to jj-worktree

Options:
  --help       Show this help message
  --version    Show version

Environment variables:
  JJ_WORKTREE_DEBUG      Set to 1 to log executed commands to stderr
  JJ_WORKTREE_REAL_GIT   Override path to real git binary"
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

/// Get the absolute path of the currently running binary.
fn current_exe_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    let canonical = exe.canonicalize()?;
    Ok(canonical)
}

/// `jj-worktree setup [--path <dir>]`
///
/// Creates a `git` symlink in the target directory pointing to the jj-worktree binary.
#[cfg(unix)]
fn cmd_setup(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    // Parse --path <dir> option
    let mut target_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--path" => {
                i += 1;
                if i >= args.len() {
                    return Err("--path requires an argument".into());
                }
                target_dir = Some(PathBuf::from(&args[i]));
            }
            other => {
                return Err(format!("unknown option for setup: {other}").into());
            }
        }
        i += 1;
    }

    let dir = match target_dir {
        Some(d) => d
            .canonicalize()
            .map_err(|e| format!("cannot resolve path: {e}"))?,
        None => env::current_dir()?,
    };

    let symlink_path = dir.join("git");
    let exe_path = current_exe_path()?;

    // Check if `git` already exists
    if symlink_path.exists() || symlink_path.symlink_metadata().is_ok() {
        // Something exists at the path — check if it's a symlink to ourselves
        match std::fs::read_link(&symlink_path) {
            Ok(link_target) => {
                // Canonicalize the link target to compare absolute paths
                let canonical_target = if link_target.is_absolute() {
                    link_target.canonicalize().unwrap_or(link_target)
                } else {
                    dir.join(&link_target)
                        .canonicalize()
                        .unwrap_or(dir.join(&link_target))
                };
                if canonical_target == exe_path {
                    eprintln!("already set up: {}", symlink_path.display());
                    return Ok(());
                } else {
                    return Err(format!(
                        "git already exists and is not a jj-worktree symlink: {}",
                        symlink_path.display()
                    )
                    .into());
                }
            }
            Err(_) => {
                // Not a symlink (regular file or directory)
                return Err(format!(
                    "git already exists and is not a jj-worktree symlink: {}",
                    symlink_path.display()
                )
                .into());
            }
        }
    }

    symlink(&exe_path, &symlink_path)?;
    eprintln!(
        "created symlink: {} -> {}",
        symlink_path.display(),
        exe_path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn cmd_setup(_args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    Err("setup command is only supported on Unix systems".into())
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
            if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
                print_help();
                return Ok(());
            }
            if args.iter().any(|a| a == "--version") {
                print_version();
                return Ok(());
            }

            match args[0].as_str() {
                "add" | "list" | "remove" => {
                    worktree::run(&args)?;
                }
                "setup" => {
                    cmd_setup(&args[1..])?;
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
