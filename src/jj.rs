use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Find the jj repository root by walking up from `start` looking for a `.jj` directory.
/// Returns the directory that contains `.jj`, or None if not found.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    loop {
        if current.join(".jj").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Build a `jj` command with `-R <repo_root>` if provided.
fn build_command(repo_root: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new("jj");
    if let Some(root) = repo_root {
        cmd.arg("-R");
        cmd.arg(root);
    }
    cmd.args(args);
    cmd
}

/// Log the command to stderr if JJ_WORKTREE_DEBUG=1.
fn debug_log(cmd: &Command) {
    if std::env::var("JJ_WORKTREE_DEBUG").as_deref() == Ok("1") {
        eprintln!("[jj-worktree debug] {:?}", cmd);
    }
}

/// Run a jj command and return the full Output.
pub fn run(repo_root: Option<&Path>, args: &[&str]) -> io::Result<Output> {
    let mut cmd = build_command(repo_root, args);
    debug_log(&cmd);
    cmd.output()
}

/// Run a jj command and return stdout as a trimmed String.
/// Returns an error if the command exits with a non-zero status.
pub fn run_stdout(repo_root: Option<&Path>, args: &[&str]) -> io::Result<String> {
    let output = run(repo_root, args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "jj command failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn find_repo_root_returns_none_for_nonexistent() {
        // /tmp should not contain .jj
        assert!(find_repo_root(Path::new("/tmp")).is_none());
    }

    #[test]
    fn find_repo_root_finds_jj_dir() {
        let tmp = std::env::temp_dir().join("jj_worktree_test_find_repo");
        let _ = fs::remove_dir_all(&tmp);
        let nested = tmp.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(tmp.join(".jj")).unwrap();

        // From nested dir, should find the root
        let result = find_repo_root(&nested);
        assert_eq!(result, Some(tmp.clone()));

        // From the root itself
        let result = find_repo_root(&tmp);
        assert_eq!(result, Some(tmp.clone()));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_command_without_repo_root() {
        let cmd = build_command(None, &["workspace", "list"]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, vec!["workspace", "list"]);
    }

    #[test]
    fn build_command_with_repo_root() {
        let cmd = build_command(Some(Path::new("/repo")), &["workspace", "list"]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, vec!["-R", "/repo", "workspace", "list"]);
    }
}
