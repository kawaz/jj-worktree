use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata for a workspace created by jj-worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceMeta {
    /// Workspace name (jj workspace name).
    pub workspace: String,
    /// Bookmark name associated with this workspace (if any).
    pub bookmark: Option<String>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Absolute path to the workspace directory.
    pub path: PathBuf,
}

/// Directory name under the jj repo root for storing metadata.
const META_DIR: &str = ".jj-worktree-meta";

/// Return the metadata directory path for a given jj repo root.
pub fn meta_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(META_DIR)
}

/// Return the metadata file path for a given workspace name.
pub fn meta_path(repo_root: &Path, ws_name: &str) -> PathBuf {
    meta_dir(repo_root).join(format!("{}.json", ws_name))
}

/// Save workspace metadata to disk.
pub fn save(repo_root: &Path, meta: &WorkspaceMeta) -> io::Result<()> {
    let dir = meta_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    let path = meta_path(repo_root, &meta.workspace);
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize error: {e}")))?;
    std::fs::write(&path, json)
}

/// Load workspace metadata from disk. Returns None if the file does not exist.
pub fn load(repo_root: &Path, ws_name: &str) -> io::Result<Option<WorkspaceMeta>> {
    let path = meta_path(repo_root, ws_name);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let meta: WorkspaceMeta = serde_json::from_str(&content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("deserialize error: {e}"),
        )
    })?;
    Ok(Some(meta))
}

/// Remove workspace metadata from disk. Returns Ok(true) if the file existed and was removed.
pub fn remove(repo_root: &Path, ws_name: &str) -> io::Result<bool> {
    let path = meta_path(repo_root, ws_name);
    if path.exists() {
        std::fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Find workspace metadata by bookmark name.
pub fn find_by_bookmark(repo_root: &Path, bookmark: &str) -> io::Result<Option<WorkspaceMeta>> {
    for meta in list(repo_root)? {
        if meta.bookmark.as_deref() == Some(bookmark) {
            return Ok(Some(meta));
        }
    }
    Ok(None)
}

/// List all workspace metadata files in the metadata directory.
pub fn list(repo_root: &Path) -> io::Result<Vec<WorkspaceMeta>> {
    let dir = meta_dir(repo_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)?;
            if let Ok(meta) = serde_json::from_str::<WorkspaceMeta>(&content) {
                result.push(meta);
            }
        }
    }
    result.sort_by(|a, b| a.workspace.cmp(&b.workspace));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn setup_temp_dir(suffix: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "jj_worktree_test_meta_{suffix}_{id}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    fn sample_meta() -> WorkspaceMeta {
        WorkspaceMeta {
            workspace: "feature-auth".to_string(),
            bookmark: Some("worktree-feature-auth".to_string()),
            created_at: Utc::now(),
            path: PathBuf::from("/path/to/feature-auth"),
        }
    }

    #[test]
    fn meta_dir_path() {
        let root = Path::new("/repo");
        assert_eq!(meta_dir(root), PathBuf::from("/repo/.jj-worktree-meta"));
    }

    #[test]
    fn meta_path_for_workspace() {
        let root = Path::new("/repo");
        assert_eq!(
            meta_path(root, "feature-auth"),
            PathBuf::from("/repo/.jj-worktree-meta/feature-auth.json")
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = setup_temp_dir("roundtrip");
        let meta = sample_meta();

        save(&tmp, &meta).unwrap();
        let loaded = load(&tmp, "feature-auth").unwrap().unwrap();

        assert_eq!(loaded.workspace, meta.workspace);
        assert_eq!(loaded.bookmark, meta.bookmark);
        assert_eq!(loaded.path, meta.path);
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let tmp = setup_temp_dir("load_none");
        let loaded = load(&tmp, "nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn remove_existing_meta() {
        let tmp = setup_temp_dir("remove_existing");
        let meta = sample_meta();
        save(&tmp, &meta).unwrap();

        let removed = remove(&tmp, "feature-auth").unwrap();
        assert!(removed);

        let loaded = load(&tmp, "feature-auth").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let tmp = setup_temp_dir("remove_none");
        let removed = remove(&tmp, "nonexistent").unwrap();
        assert!(!removed);
    }

    #[test]
    fn list_returns_sorted_entries() {
        let tmp = setup_temp_dir("list_sorted");

        let meta_b = WorkspaceMeta {
            workspace: "beta".to_string(),
            bookmark: None,
            created_at: Utc::now(),
            path: PathBuf::from("/path/to/beta"),
        };
        let meta_a = WorkspaceMeta {
            workspace: "alpha".to_string(),
            bookmark: Some("bm-alpha".to_string()),
            created_at: Utc::now(),
            path: PathBuf::from("/path/to/alpha"),
        };

        save(&tmp, &meta_b).unwrap();
        save(&tmp, &meta_a).unwrap();

        let entries = list(&tmp).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].workspace, "alpha");
        assert_eq!(entries[1].workspace, "beta");
    }

    #[test]
    fn list_empty_returns_empty_vec() {
        let tmp = setup_temp_dir("list_empty");
        let entries = list(&tmp).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn serialization_format() {
        let meta = WorkspaceMeta {
            workspace: "test-ws".to_string(),
            bookmark: Some("test-bookmark".to_string()),
            created_at: "2026-03-27T12:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            path: PathBuf::from("/absolute/path/to/test-ws"),
        };

        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert!(json.contains("\"workspace\": \"test-ws\""));
        assert!(json.contains("\"bookmark\": \"test-bookmark\""));
        assert!(json.contains("\"created_at\": \"2026-03-27T12:00:00Z\""));
        assert!(json.contains("\"path\": \"/absolute/path/to/test-ws\""));

        // Deserialize back
        let deserialized: WorkspaceMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, meta);
    }
}
