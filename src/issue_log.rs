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
/// Privacy: the parent's full argv may contain secrets passed as CLI flags
/// (API tokens, passwords, internal AI prompts, etc.). To avoid persisting
/// those, callers must opt in via `JJ_WORKTREE_ISSUE_INCLUDE_CMDLINE=1`.
/// When the env var is unset (the default), only the parent process basename
/// (`comm`) is captured.
///
/// Lookup paths:
/// - Linux: read `/proc/<ppid>/cmdline` (full argv) or `/proc/<ppid>/comm`
///   (basename only).
/// - macOS / others: shell out to `ps -p <ppid> -o command=` (full) or
///   `ps -p <ppid> -o comm=` (basename). The `ps` invocation has a hard
///   timeout to prevent the shim from hanging on a misbehaving system.
///
/// Returns an empty string on any failure or unsupported platform.
fn caller_cmdline(ppid: u32) -> String {
    if ppid == 0 {
        return String::new();
    }
    let include_full = std::env::var("JJ_WORKTREE_ISSUE_INCLUDE_CMDLINE").as_deref() == Ok("1");

    // Linux: read from /proc.
    let proc_path = if include_full {
        format!("/proc/{ppid}/cmdline")
    } else {
        format!("/proc/{ppid}/comm")
    };
    if let Ok(content) = fs::read(&proc_path)
        && !content.is_empty()
    {
        let s = if include_full {
            // /proc/<pid>/cmdline is NUL-separated; rejoin with spaces.
            content
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).to_string())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::from_utf8_lossy(&content).trim().to_string()
        };
        return s;
    }

    // macOS / BSD: shell out to ps with a timeout so a stuck environment
    // cannot deadlock the shim. We rely on `Command::output` returning
    // promptly under normal conditions; the deadline is enforced via a
    // thread + try_wait loop because the standard library lacks a built-in.
    let ps_field = if include_full { "command=" } else { "comm=" };
    if let Ok(mut child) = std::process::Command::new("ps")
        .args(["-p", &ppid.to_string(), "-o", ps_field])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // Poll up to ~200ms for the child to exit. ps should respond in <10ms
        // in practice; anything slower means the system is misbehaving and we
        // prefer an empty result over blocking the shim.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let mut out = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        use std::io::Read;
                        let _ = stdout.read_to_string(&mut out);
                    }
                    return out.trim().to_string();
                }
                Ok(Some(_)) => return String::new(),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return String::new();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return String::new(),
            }
        }
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

    // The directory and log file may carry caller_cmdline (when opt-in) or
    // argv values that are sensitive. Restrict to user-only permissions.
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let mut open_opts = fs::OpenOptions::new();
    open_opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.mode(0o600);
    }
    if let Ok(mut f) = open_opts.open(&path) {
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
This option is not yet known by jj-worktree, but the operation continued.\n\
If you intended this option to take effect, the result is likely NOT what\n\
you expected. The incident has been logged to {}\n\
\n\
[For AI agents] Please surface this to the human user (do not silently\n\
fall back to a different approach). To report it upstream, open an issue at\n\
  https://github.com/kawaz/jj-worktree/issues/new\n\
including: the option/error, the calling context, and what you tried to do.",
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
        // Hold the crate-wide env lock so concurrent tests don't trample
        // JJ_WORKTREE_ISSUE_LOG while we mutate it here.
        let _lock = crate::test_util::env_lock();
        let key = "JJ_WORKTREE_ISSUE_LOG";
        let prev = std::env::var_os(key);
        // SAFETY: env mutation is serialised by the lock above.
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
        let _lock = crate::test_util::env_lock();
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
