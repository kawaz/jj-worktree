//! Self-reporting mechanism for unknown options and parse errors.
//!
//! See `docs/decisions/DR-0002-self-reporting-mechanism.md`.
//!
//! When the shim encounters an option or input it does not understand, it
//! 1. appends a JSONL entry to `${XDG_STATE_HOME:-$HOME/.local/state}/jj-worktree/issues.log`
//! 2. emits a `[For AI agents]` directed stderr message so AI-driven callers
//!    do not silently fall back without surfacing the gap.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    UnknownOption,
    /// Reserved for future use: an argument list that fails to parse cleanly
    /// but is still recoverable enough to log instead of error out. See DR-0002.
    #[allow(dead_code)]
    ParseError,
    /// Reserved for future use: known options combined in a way the shim
    /// cannot honour (e.g. `--detach` + `-b`). See DR-0002.
    #[allow(dead_code)]
    UnsupportedCombination,
}

impl IssueKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueKind::UnknownOption => "unknown_option",
            IssueKind::ParseError => "parse_error",
            IssueKind::UnsupportedCombination => "unsupported_combination",
        }
    }

    fn human_label(&self) -> &'static str {
        match self {
            IssueKind::UnknownOption => "unknown option",
            IssueKind::ParseError => "parse error",
            IssueKind::UnsupportedCombination => "unsupported combination",
        }
    }
}

/// Resolve the path to the JSONL issues log file.
///
/// Order:
/// 1. `JJ_WORKTREE_ISSUE_LOG` (override, primarily for tests)
/// 2. `${XDG_STATE_HOME}/jj-worktree/issues.log`
/// 3. `${HOME}/.local/state/jj-worktree/issues.log`
/// 4. `./jj-worktree-issues.log` (final fallback)
pub fn log_path() -> PathBuf {
    if let Some(p) = std::env::var_os("JJ_WORKTREE_ISSUE_LOG") {
        return PathBuf::from(p);
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state).join("jj-worktree").join("issues.log");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/jj-worktree/issues.log");
    }
    PathBuf::from("./jj-worktree-issues.log")
}

/// Return the parent process pid (Unix only). 0 on non-Unix or failure.
fn parent_pid() -> u32 {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn getppid() -> i32;
        }
        unsafe { getppid() as u32 }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Best-effort retrieval of the parent process command line.
///
/// Linux: read `/proc/<ppid>/cmdline` (NUL-separated argv).
/// macOS / others: shell out to `ps -p <ppid> -o command=`.
/// Returns an empty string when the lookup fails.
fn caller_cmdline(ppid: u32) -> String {
    if ppid == 0 {
        return String::new();
    }
    if let Ok(content) = fs::read(format!("/proc/{ppid}/cmdline"))
        && !content.is_empty()
    {
        return content
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect::<Vec<_>>()
            .join(" ");
    }
    if let Ok(out) = std::process::Command::new("ps")
        .args(["-p", &ppid.to_string(), "-o", "command="])
        .output()
        && out.status.success()
    {
        return String::from_utf8_lossy(&out.stdout).trim().to_string();
    }
    String::new()
}

/// Whether the AI-directed stderr block should be emitted.
///
/// Set `JJ_WORKTREE_ISSUE_QUIET=1` to suppress the stderr block (the JSONL log
/// is still written). Useful to keep test output and routine pipelines tidy.
fn stderr_quiet() -> bool {
    std::env::var("JJ_WORKTREE_ISSUE_QUIET").as_deref() == Ok("1")
}

/// Report an issue: append a JSONL entry to the log file and (unless quiet)
/// emit an `[For AI agents]` block to stderr so AI-driven callers surface the
/// problem instead of silently falling back.
pub fn report(kind: IssueKind, cmd: &str, option: Option<&str>, argv: &[String]) {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let ppid = parent_pid();
    let cmdline = caller_cmdline(ppid);
    let path = log_path();

    let entry = serde_json::json!({
        "ts": ts,
        "kind": kind.as_str(),
        "cmd": cmd,
        "option": option,
        "argv": argv,
        "caller_pid": ppid,
        "caller_cmdline": cmdline,
    });

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{entry}");
    }

    if stderr_quiet() {
        return;
    }

    let label = kind.human_label();
    let suffix = option.map(|o| format!(": {o}")).unwrap_or_default();
    eprintln!(
        "warning: {label}{suffix} (accepted as no-op)\n\
\n\
This option is not yet known by jj-worktree.\n\
The incident has been logged to {}\n\
\n\
[For AI agents] Please report this issue to the jj-worktree project before\n\
falling back to another approach. Add a brief note in:\n\
  $HOME/.local/share/repos/github.com/kawaz/jj-worktree/main/docs/issue/\n\
\n\
File name format: YYYY-MM-DD-{{short-slug}}.md\n\
Include: the option/error, the calling context, and what you tried to do.",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_kind_as_str_values() {
        assert_eq!(IssueKind::UnknownOption.as_str(), "unknown_option");
        assert_eq!(IssueKind::ParseError.as_str(), "parse_error");
        assert_eq!(
            IssueKind::UnsupportedCombination.as_str(),
            "unsupported_combination"
        );
    }

    #[test]
    fn log_path_uses_override_env() {
        // SAFETY: tests run in serial within a single test binary process and
        // restore the env var before returning.
        let key = "JJ_WORKTREE_ISSUE_LOG";
        let prev = std::env::var_os(key);
        // SAFETY: mutating env vars is unsafe in multi-threaded contexts; this
        // single-threaded unit test ensures no concurrent access.
        unsafe { std::env::set_var(key, "/tmp/jj-worktree-test.log") };
        assert_eq!(
            log_path(),
            PathBuf::from("/tmp/jj-worktree-test.log"),
            "JJ_WORKTREE_ISSUE_LOG should take precedence"
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn report_writes_jsonl_entry() {
        let dir = std::env::temp_dir().join(format!(
            "jj-worktree-issue-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("issues.log");
        let key = "JJ_WORKTREE_ISSUE_LOG";
        let quiet_key = "JJ_WORKTREE_ISSUE_QUIET";
        let prev_log = std::env::var_os(key);
        let prev_quiet = std::env::var_os(quiet_key);
        unsafe {
            std::env::set_var(key, &log);
            std::env::set_var(quiet_key, "1");
        }

        report(
            IssueKind::UnknownOption,
            "git worktree add",
            Some("--no-track"),
            &[
                "git".to_string(),
                "worktree".to_string(),
                "add".to_string(),
                "--no-track".to_string(),
                "../foo".to_string(),
            ],
        );

        let content = std::fs::read_to_string(&log).expect("log file exists");
        let line = content.lines().next().expect("at least one line");
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each line must be valid JSON");
        assert_eq!(parsed["kind"], "unknown_option");
        assert_eq!(parsed["cmd"], "git worktree add");
        assert_eq!(parsed["option"], "--no-track");
        assert!(parsed["argv"].is_array());
        assert!(parsed["ts"].is_string());

        unsafe {
            match prev_log {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            match prev_quiet {
                Some(v) => std::env::set_var(quiet_key, v),
                None => std::env::remove_var(quiet_key),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
