//! Agent-managed include list — the explicit whitelist of arbitrary workspace
//! files the agent opts into syncing via `alf add`.
//!
//! Stored as `<workspace>/.alf-include.json` and itself preserved in
//! `raw/openclaw/`, so the list (the agent's sync config) and the sync log
//! travel on restore. ALF never auto-discovers arbitrary files — the agent
//! declares intent explicitly.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Workspace-relative file name of the include list.
pub const INCLUDE_FILE: &str = ".alf-include.json";

/// Workspace-relative file name of the human/agent-readable sync log.
pub const SYNC_LOG_FILE: &str = ".alf-sync-log.md";

/// The agent-managed whitelist of extra files to sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncludeList {
    #[serde(default)]
    pub files: Vec<IncludeEntry>,
}

/// One tracked file in the include list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncludeEntry {
    /// Workspace-relative, forward-slashed path.
    pub path: String,
    /// When the file was added via `alf add`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_at: Option<DateTime<Utc>>,
}

impl IncludeList {
    /// Load from `<workspace>/.alf-include.json`. A missing file yields an
    /// empty list. A malformed file is an error (callers decide whether to
    /// surface it or degrade gracefully).
    pub fn load(workspace: &Path) -> Result<Self> {
        let path = workspace.join(INCLUDE_FILE);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| format!("{INCLUDE_FILE} is not valid JSON"))
    }

    /// Persist to `<workspace>/.alf-include.json` (pretty JSON).
    pub fn save(&self, workspace: &Path) -> Result<()> {
        let path = workspace.join(INCLUDE_FILE);
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }

    /// Whether `rel` is already tracked.
    pub fn contains(&self, rel: &str) -> bool {
        self.files.iter().any(|e| e.path == rel)
    }

    /// Add a workspace-relative path (idempotent). Returns true if newly added.
    pub fn add(&mut self, rel: &str) -> bool {
        if self.contains(rel) {
            return false;
        }
        self.files.push(IncludeEntry {
            path: rel.to_string(),
            added_at: Some(Utc::now()),
        });
        true
    }

    /// Remove a workspace-relative path. Returns true if it was present.
    pub fn remove(&mut self, rel: &str) -> bool {
        let before = self.files.len();
        self.files.retain(|e| e.path != rel);
        self.files.len() != before
    }

    /// Sorted, de-duplicated tracked paths (deterministic enumeration order).
    pub fn paths(&self) -> Vec<String> {
        let mut p: Vec<String> = self.files.iter().map(|e| e.path.clone()).collect();
        p.sort();
        p.dedup();
        p
    }
}

/// Prune tracked files that no longer exist on disk and record each removal in
/// `<workspace>/.alf-sync-log.md`. Returns the removed paths (for sync output).
///
/// Called at sync time (before export) so the cleaned include list and the
/// updated log are themselves captured in the snapshot/restore. A no-op (and no
/// log write) when nothing is missing.
pub fn prune_and_log_missing(workspace: &Path) -> Result<Vec<String>> {
    let mut list = IncludeList::load(workspace)?;
    let missing: Vec<String> = list
        .files
        .iter()
        .filter(|e| !workspace.join(&e.path).is_file())
        .map(|e| e.path.clone())
        .collect();
    if missing.is_empty() {
        return Ok(missing);
    }
    for path in &missing {
        list.remove(path);
    }
    list.save(workspace)?;
    append_sync_log(workspace, &missing)?;
    Ok(missing)
}

/// Append removal notes to `<workspace>/.alf-sync-log.md` (created if absent).
/// The log is plain Markdown so the agent can read it to answer "what happened
/// to notes.txt".
fn append_sync_log(workspace: &Path, removed: &[String]) -> Result<()> {
    use std::io::Write;

    let path = workspace.join(SYNC_LOG_FILE);
    let date = Utc::now().format("%Y-%m-%d");
    let mut entry = String::new();
    if !path.is_file() {
        entry.push_str("# ALF sync log\n\nFiles removed from sync because they were deleted.\n\n");
    }
    for rel in removed {
        entry.push_str(&format!(
            "- {date}: removed `{rel}` from sync (file no longer present)\n"
        ));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    file.write_all(entry.as_bytes())
        .with_context(|| format!("Failed to append to {}", path.display()))?;
    Ok(())
}

/// Resolve a user-supplied path to a workspace-relative, forward-slashed path
/// for `alf add`. The path is interpreted relative to the workspace (absolute
/// paths are accepted if they resolve inside it). Rejects paths that escape the
/// workspace, non-files, and the alf-managed sentinel files themselves.
pub fn normalize_include_path(workspace: &Path, input: &str) -> Result<String> {
    let ws_canon = workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", workspace.display()))?;

    let input_path = Path::new(input);
    let candidate = if input_path.is_absolute() {
        input_path.to_path_buf()
    } else {
        ws_canon.join(input_path)
    };

    let abs = candidate
        .canonicalize()
        .with_context(|| format!("file not found: {input}"))?;

    if !abs.is_file() {
        bail!("not a file: {input}");
    }

    let rel = abs
        .strip_prefix(&ws_canon)
        .map_err(|_| anyhow::anyhow!("{input} is outside the workspace"))?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    if rel_str == INCLUDE_FILE || rel_str == SYNC_LOG_FILE {
        bail!("{rel_str} is managed by alf and cannot be added");
    }

    Ok(rel_str)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn load_missing_is_empty() {
        let dir = ws();
        let list = IncludeList::load(dir.path()).unwrap();
        assert!(list.files.is_empty());
    }

    #[test]
    fn add_save_load_round_trip() {
        let dir = ws();
        let mut list = IncludeList::default();
        assert!(list.add("notes.txt"));
        assert!(!list.add("notes.txt")); // idempotent
        assert!(list.add("data/x.csv"));
        list.save(dir.path()).unwrap();

        let reloaded = IncludeList::load(dir.path()).unwrap();
        assert_eq!(reloaded.paths(), vec!["data/x.csv", "notes.txt"]);
    }

    #[test]
    fn remove_works() {
        let mut list = IncludeList::default();
        list.add("a.txt");
        list.add("b.txt");
        assert!(list.remove("a.txt"));
        assert!(!list.remove("a.txt"));
        assert_eq!(list.paths(), vec!["b.txt"]);
    }

    #[test]
    fn load_malformed_errors() {
        let dir = ws();
        fs::write(dir.path().join(INCLUDE_FILE), "{ not json").unwrap();
        assert!(IncludeList::load(dir.path()).is_err());
    }

    #[test]
    fn normalize_accepts_workspace_file() {
        let dir = ws();
        fs::write(dir.path().join("notes.txt"), "hi").unwrap();
        let rel = normalize_include_path(dir.path(), "notes.txt").unwrap();
        assert_eq!(rel, "notes.txt");
    }

    #[test]
    fn normalize_accepts_nested_file() {
        let dir = ws();
        fs::create_dir_all(dir.path().join("my-project")).unwrap();
        fs::write(dir.path().join("my-project/data.csv"), "x").unwrap();
        let rel = normalize_include_path(dir.path(), "my-project/data.csv").unwrap();
        assert_eq!(rel, "my-project/data.csv");
    }

    #[test]
    fn normalize_rejects_missing() {
        let dir = ws();
        assert!(normalize_include_path(dir.path(), "nope.txt").is_err());
    }

    #[test]
    fn normalize_rejects_escape() {
        let dir = ws();
        // A real file outside the workspace, referenced via ..
        let outside = dir.path().parent().unwrap().join("outside.txt");
        fs::write(&outside, "x").unwrap();
        let escape = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(normalize_include_path(dir.path(), &escape).is_err());
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn normalize_rejects_managed_sentinels() {
        let dir = ws();
        fs::write(dir.path().join(INCLUDE_FILE), "{\"files\":[]}").unwrap();
        assert!(normalize_include_path(dir.path(), INCLUDE_FILE).is_err());
    }

    #[test]
    fn prune_removes_missing_and_writes_log() {
        let dir = ws();
        fs::write(dir.path().join("kept.txt"), "k").unwrap();
        let mut list = IncludeList::default();
        list.add("kept.txt");
        list.add("gone.txt"); // never created on disk
        list.save(dir.path()).unwrap();

        let removed = prune_and_log_missing(dir.path()).unwrap();
        assert_eq!(removed, vec!["gone.txt".to_string()]);

        // Include list no longer tracks the missing file.
        let reloaded = IncludeList::load(dir.path()).unwrap();
        assert_eq!(reloaded.paths(), vec!["kept.txt"]);

        // The log records the removal in a form the agent can read.
        let log = fs::read_to_string(dir.path().join(SYNC_LOG_FILE)).unwrap();
        assert!(log.contains("gone.txt"));
        assert!(log.contains("removed"));
    }

    #[test]
    fn prune_noop_when_nothing_missing() {
        let dir = ws();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        let mut list = IncludeList::default();
        list.add("a.txt");
        list.save(dir.path()).unwrap();

        let removed = prune_and_log_missing(dir.path()).unwrap();
        assert!(removed.is_empty());
        // No log written when nothing was pruned.
        assert!(!dir.path().join(SYNC_LOG_FILE).is_file());
    }
}
