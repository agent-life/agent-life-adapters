//! Agent-managed include list — the explicit whitelist of arbitrary workspace
//! files the agent opts into syncing via `alf add`.
//!
//! Stored as `<workspace>/.alf-include.json` and itself preserved in
//! `raw/{runtime}/`, so the list (the agent's sync config) and the sync log
//! travel on restore. ALF never auto-discovers arbitrary files — the agent
//! declares intent explicitly.
//!
//! This module is runtime-agnostic: it deals only in workspace-relative paths
//! and two sentinel file names, with no knowledge of any framework's layout.
//! It lives in `alf-core` so every adapter (and the CLI) can share one
//! implementation — see the OpenClaw and ZeroClaw adapters' `export`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Workspace-relative file name of the include list.
pub const INCLUDE_FILE: &str = ".alf-include.json";

/// Workspace-relative file name of the human/agent-readable sync log.
pub const SYNC_LOG_FILE: &str = ".alf-sync-log.md";

/// The agent-managed whitelist of extra files to sync.
///
/// **Forward compatible (MIN-7).** This file is rewritten in place by routine
/// operations — `mark_external_inert` on every restore/import,
/// `prune_and_log_missing` on every sync — and it travels inside the archive,
/// so an older binary's rewrite propagates to the cloud and to every other
/// machine. Mixed versions are routine (a runtime image bakes `alf` at build
/// time and lags the user's laptop), so unknown fields written by a newer alf
/// are preserved verbatim rather than silently dropped, matching the
/// `Manifest`/`AgentMetadata` discipline (spec §8.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IncludeList {
    #[serde(default)]
    pub files: Vec<IncludeEntry>,
    /// Unknown document-level fields preserved for forward compatibility.
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

/// One tracked file in the include list.
///
/// In-workspace entries (the common case) carry just a workspace-relative
/// `path`. External entries (D3 — files outside the workspace, e.g. a project
/// `AGENTS.md`) additionally set `external`, record their absolute `source` for
/// provenance + re-validation, and use `verified` for inert-on-restore: a
/// `false` value means "restored from an archive, do not pack until the local
/// user re-confirms" (so a hostile archive's external entries do nothing).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncludeEntry {
    /// For in-workspace entries: the workspace-relative, forward-slashed path.
    /// For external entries: the sanitized archive name under `external/`.
    pub path: String,
    /// When the file was added via `alf add`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_at: Option<DateTime<Utc>>,
    /// True for files tracked from outside the workspace (D3).
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
    /// Absolute source path (external entries only) — provenance + the path
    /// re-validated at export against the host-local allowed roots + denylist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Whether this entry is blessed for packing. Always true for in-workspace
    /// entries and freshly-added externals; an external entry restored from an
    /// archive is imported `false` (inert) until the local user re-confirms.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub verified: bool,
    /// Unknown entry-level fields preserved for forward compatibility (MIN-7).
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_true(b: &bool) -> bool {
    *b
}
fn default_true() -> bool {
    true
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

    /// Persist to `<workspace>/.alf-include.json` (pretty JSON, atomic — a
    /// crash mid-save can never leave a torn include list).
    pub fn save(&self, workspace: &Path) -> Result<()> {
        let path = workspace.join(INCLUDE_FILE);
        let json = serde_json::to_string_pretty(self)?;
        crate::fs_atomic::write_atomic(&path, json.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
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
            external: false,
            source: None,
            verified: true,
            extra: HashMap::new(),
        });
        true
    }

    /// Add an external file (D3): `sanitized` is its archive name under
    /// `external/`, `source_abs` its absolute on-disk path. Freshly added
    /// externals are `verified` (the human gate happens in the CLI before this).
    /// Idempotent on the sanitized name.
    pub fn add_external(&mut self, sanitized: &str, source_abs: &str) -> bool {
        if self.files.iter().any(|e| e.external && e.path == sanitized) {
            return false;
        }
        self.files.push(IncludeEntry {
            path: sanitized.to_string(),
            added_at: Some(Utc::now()),
            external: true,
            source: Some(source_abs.to_string()),
            verified: true,
            extra: HashMap::new(),
        });
        true
    }

    /// The external entries (verified or not).
    pub fn externals(&self) -> impl Iterator<Item = &IncludeEntry> {
        self.files.iter().filter(|e| e.external)
    }

    /// External entries verified on THIS host — the only externals export
    /// packs (`external_entries_for_export`) and therefore the only ones a
    /// watch surface may root. A restored archive's externals come back
    /// inert (`verified: false`, D3 — "a hostile archive's externals do
    /// nothing"): watching them would fire tracked syncs that capture nothing
    /// and let a crafted archive choose filesystem watch roots.
    pub fn verified_externals(&self) -> impl Iterator<Item = &IncludeEntry> {
        self.externals().filter(|e| e.verified)
    }

    /// Remove a workspace-relative path. Returns true if it was present.
    pub fn remove(&mut self, rel: &str) -> bool {
        let before = self.files.len();
        self.files.retain(|e| e.path != rel);
        self.files.len() != before
    }

    /// Sorted, de-duplicated **in-workspace** tracked paths (deterministic
    /// enumeration order). External entries (D3) are excluded — their `path` is
    /// a sanitized archive name, not a workspace-relative path; use
    /// [`IncludeList::externals`] for those.
    pub fn paths(&self) -> Vec<String> {
        let mut p: Vec<String> = self
            .files
            .iter()
            .filter(|e| !e.external)
            .map(|e| e.path.clone())
            .collect();
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
    // Only in-workspace entries are pruned by workspace-relative existence;
    // external entries (D3) are validated against their absolute `source` at
    // export, not here.
    let missing: Vec<String> = list
        .files
        .iter()
        .filter(|e| !e.external && !workspace.join(&e.path).is_file())
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

/// Flip restored external include entries to `verified = false` (inert). Returns
/// the count changed.
///
/// Called by adapter imports right after the raw tree (which carries
/// `.alf-include.json`) is restored: a hostile/compromised archive's external
/// entries must do nothing on the next sync until the local user re-confirms
/// them with `alf add --external` (D3 inert-on-restore). In-workspace entries
/// are untouched; idempotent (already-inert entries stay inert).
pub fn mark_external_inert(workspace: &Path) -> Result<usize> {
    let path = workspace.join(INCLUDE_FILE);
    if !path.is_file() {
        return Ok(0);
    }
    let mut list = IncludeList::load(workspace)?;
    let mut changed = 0;
    for e in list.files.iter_mut() {
        if e.external && e.verified {
            e.verified = false;
            changed += 1;
        }
    }
    if changed > 0 {
        list.save(workspace)?;
    }
    Ok(changed)
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

/// Re-validate a stored include entry at **export** time — the control point
/// that closes finding A4.2.
///
/// `alf add` validates a path once (via [`normalize_include_path`]), but the
/// stored `.alf-include.json` travels in the archive and can be restored from a
/// hostile/compromised source. Export must therefore re-check every entry, not
/// trust the list: this canonicalizes `<workspace>/<rel>` (resolving symlinks),
/// confirms it stays inside the workspace, and rejects the managed sentinels.
/// Returns the canonical in-workspace file path, or an error explaining the
/// rejection (escape, symlinked-out, sentinel, or no longer a file). Callers
/// skip+log on error rather than packing the entry.
pub fn safe_include_path(workspace: &Path, rel: &str) -> Result<PathBuf> {
    let ws_canon = workspace
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", workspace.display()))?;
    // join() on an absolute `rel` replaces the base; canonicalize then resolves
    // any `..`/symlinks. Either way the strip_prefix below is the real gate.
    let abs = ws_canon
        .join(rel)
        .canonicalize()
        .with_context(|| format!("tracked path not found: {rel}"))?;
    if !abs.is_file() {
        bail!("tracked path is not a file: {rel}");
    }
    let stripped = abs
        .strip_prefix(&ws_canon)
        .map_err(|_| anyhow::anyhow!("tracked path {rel} resolves outside the workspace"))?;
    let rel_str = stripped.to_string_lossy().replace('\\', "/");
    if rel_str == INCLUDE_FILE || rel_str == SYNC_LOG_FILE {
        bail!("{rel_str} is managed by alf and cannot be tracked");
    }
    // MAJ-7: the sensitive-path denylist applies to stored in-workspace
    // entries too — `alf add` refuses them up front, but the list can be
    // restored from an archive or hand-edited, so export must re-check here
    // (callers skip+warn, same as the other rejections).
    if is_denylisted(&abs) {
        bail!(
            "tracked path {rel} matches the sensitive-path denylist — not packed \
             (secrets belong in the encrypted vault: alf vault add)"
        );
    }
    Ok(abs)
}

// ---------------------------------------------------------------------------
// D3 — external-file support (allowed roots + denylist + sanitized names)
// ---------------------------------------------------------------------------

/// Host-local policy file listing the directories the human has blessed as
/// allowed roots for external `alf add`. Newline-delimited absolute paths.
/// **Never** written into an archive — a restored entry is honored only if it
/// still satisfies the *local* policy.
pub fn allowed_roots_path() -> Option<PathBuf> {
    crate::home_dir().map(|h| h.join(".alf").join("external-roots"))
}

/// Load the blessed external roots (canonicalized; unreadable lines skipped).
pub fn load_allowed_roots() -> Vec<PathBuf> {
    let Some(path) = allowed_roots_path() else {
        return Vec::new();
    };
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| Path::new(l).canonicalize().ok())
        .collect()
}

/// Bless `dir` as an allowed external root (idempotent). Returns the canonical
/// path that was blessed.
pub fn add_allowed_root(dir: &Path) -> Result<PathBuf> {
    let canon = dir
        .canonicalize()
        .with_context(|| format!("allowed root not found: {}", dir.display()))?;
    if !canon.is_dir() {
        bail!("allowed root is not a directory: {}", canon.display());
    }
    let path = allowed_roots_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut roots = load_allowed_roots();
    if !roots.iter().any(|r| r == &canon) {
        roots.push(canon.clone());
        let body: String = roots.iter().map(|r| format!("{}\n", r.display())).collect();
        crate::fs_atomic::write_atomic(&path, body.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(canon)
}

/// Non-overridable sensitive-path denylist (checked against the **canonical**
/// resolved path). Always rejected regardless of allowed roots or flags — the
/// A1.12/A4.4 mitigation (e.g. an injected agent trying to add the vault key).
pub fn is_denylisted(canonical: &Path) -> bool {
    // Home-rooted sensitive directories + runtime secret stores.
    if let Some(home) = crate::home_dir() {
        let dirs = [
            ".alf",
            ".ssh",
            ".aws",
            ".config/gcloud",
            ".openclaw/credentials",
        ];
        for d in dirs {
            if canonical.starts_with(home.join(d)) {
                return true;
            }
        }
        for f in [".hermes/.env", ".zeroclaw/.secret_key"] {
            if canonical == home.join(f) {
                return true;
            }
        }
    }
    // Filename patterns.
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.starts_with("id_rsa")
        || name.ends_with("_ed25519")
}

/// Validate an external source path for `alf add`/export: canonicalize (resolving
/// symlinks — the TOCTOU guard), require it to be a file under an allowed root,
/// enforce the per-entry size cap, and reject denylisted paths. Returns the
/// canonical path on success.
///
/// The size cap runs at add-time AND at export-time re-validation (via
/// [`external_entries_for_export`]), so a file that grows past the cap after
/// being added becomes an export-time skip + warning, never an oversized
/// archive member.
pub fn validate_external_source(source: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf> {
    let canon = source
        .canonicalize()
        .with_context(|| format!("external file not found: {}", source.display()))?;
    if !canon.is_file() {
        bail!("external path is not a file: {}", canon.display());
    }
    let len = fs::metadata(&canon)
        .with_context(|| format!("reading metadata for {}", canon.display()))?
        .len();
    if len > crate::MAX_RAW_ENTRY_BYTES {
        bail!(
            "{} is {len} bytes, over the {} byte per-file cap (a restore would reject it)",
            canon.display(),
            crate::MAX_RAW_ENTRY_BYTES
        );
    }
    if is_denylisted(&canon) {
        bail!(
            "{} is on the non-overridable sensitive-path denylist and cannot be added",
            canon.display()
        );
    }
    if !allowed_roots.iter().any(|root| canon.starts_with(root)) {
        bail!(
            "{} is not under any blessed external root; bless one with `alf add --allow-root <dir>`",
            canon.display()
        );
    }
    Ok(canon)
}

/// Sanitized, collision-resistant archive name for an external file:
/// `<8-hex-of-source>-<safe-basename>`. The original absolute path is recorded
/// as entry metadata; the *archive member name* is always this sanitized form,
/// so `safe_extract_path` confines the restore write.
pub fn sanitized_external_name(canonical_source: &Path) -> String {
    let h = fnv1a(canonical_source.to_string_lossy().as_bytes());
    let base: String = canonical_source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{h:08x}-{base}")
}

/// Stable FNV-1a (32-bit) — a dependency-free name disambiguator (not crypto).
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// What an export should pack for external entries: `(archive_relative_path,
/// canonical_source)` for every **verified** external entry that still passes
/// validation. Unverified (inert-on-restore) and failing entries are returned as
/// human-readable skip reasons instead — the caller logs them.
///
/// `archive_relative_path` is `external/<sanitized>` (the adapter prefixes
/// `raw/{runtime}/`).
pub fn external_entries_for_export(
    list: &IncludeList,
    allowed_roots: &[PathBuf],
) -> (Vec<(String, PathBuf)>, Vec<String>) {
    let mut packable = Vec::new();
    let mut skipped = Vec::new();
    for e in list.externals() {
        let Some(source) = e.source.as_deref() else {
            skipped.push(format!(
                "external entry {} has no source path; skipped",
                e.path
            ));
            continue;
        };
        if !e.verified {
            skipped.push(format!(
                "external entry {source} is inert (restored, not re-confirmed); not packed until re-added"
            ));
            continue;
        }
        match validate_external_source(Path::new(source), allowed_roots) {
            Ok(canon) => packable.push((format!("external/{}", e.path), canon)),
            Err(err) => skipped.push(format!("external entry {source} rejected at export: {err}")),
        }
    }
    (packable, skipped)
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

    // -- A4.2: export-time re-validation of a hostile stored include list ----

    #[test]
    fn safe_include_path_accepts_in_workspace_file() {
        let dir = ws();
        fs::write(dir.path().join("notes.txt"), "hi").unwrap();
        let abs = safe_include_path(dir.path(), "notes.txt").unwrap();
        assert!(abs.ends_with("notes.txt"));
    }

    #[test]
    fn safe_include_path_rejects_parent_traversal() {
        let dir = ws();
        // A real file outside the workspace; a poisoned list names it via `..`.
        let outside = dir.path().parent().unwrap().join("secret.txt");
        fs::write(&outside, "x").unwrap();
        let escape = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(
            safe_include_path(dir.path(), &escape).is_err(),
            "export must reject a `..`-escaping stored include entry"
        );
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn safe_include_path_rejects_absolute_and_sentinels() {
        let dir = ws();
        assert!(safe_include_path(dir.path(), "/etc/hostname").is_err());
        fs::write(dir.path().join(INCLUDE_FILE), "{\"files\":[]}").unwrap();
        assert!(safe_include_path(dir.path(), INCLUDE_FILE).is_err());
    }

    #[test]
    fn safe_include_path_rejects_denylisted_entries() {
        // MAJ-7 second line of defense: a restored/hand-edited list naming an
        // in-workspace secret (hermes' workspace IS ~/.hermes, holding .env)
        // must not pack — adapters skip+warn on this error.
        let dir = ws();
        for name in [".env", "server.pem", "signing.key"] {
            fs::write(dir.path().join(name), "secret").unwrap();
            let err = safe_include_path(dir.path(), name)
                .expect_err("a denylisted stored entry must be rejected at export");
            assert!(
                format!("{err:#}").contains("denylist"),
                "the rejection must name the denylist: {err:#}"
            );
        }
        // Benign entries are unaffected.
        fs::write(dir.path().join("notes.md"), "n").unwrap();
        assert!(safe_include_path(dir.path(), "notes.md").is_ok());
    }

    #[test]
    fn safe_include_path_rejects_symlink_escape() {
        let dir = ws();
        let outside = dir.path().parent().unwrap().join("escape-target.txt");
        fs::write(&outside, "x").unwrap();
        // A tracked "safe-looking" name that is actually a symlink out.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.path().join("link.txt")).unwrap();
            assert!(
                safe_include_path(dir.path(), "link.txt").is_err(),
                "a tracked path symlinked outside the workspace must be rejected"
            );
        }
        let _ = fs::remove_file(&outside);
    }

    // -- D3: external-file validation --------------------------------------

    #[test]
    fn validate_external_accepts_blessed_file() {
        let root = ws();
        let f = root.path().join("AGENTS.md");
        fs::write(&f, "# ops").unwrap();
        let roots = vec![root.path().canonicalize().unwrap()];
        let canon = validate_external_source(&f, &roots).unwrap();
        assert!(canon.ends_with("AGENTS.md"));
    }

    #[test]
    fn validate_external_rejects_outside_roots() {
        let root = ws();
        let other = ws();
        let f = other.path().join("AGENTS.md");
        fs::write(&f, "x").unwrap();
        let roots = vec![root.path().canonicalize().unwrap()];
        assert!(validate_external_source(&f, &roots).is_err());
    }

    #[test]
    fn validate_external_rejects_denylisted_filename() {
        let root = ws();
        let env = root.path().join(".env");
        fs::write(&env, "OPENAI_API_KEY=sk").unwrap();
        let roots = vec![root.path().canonicalize().unwrap()];
        assert!(
            validate_external_source(&env, &roots).is_err(),
            ".env under a blessed root must still be denylisted"
        );
        let pem = root.path().join("server.pem");
        fs::write(&pem, "----").unwrap();
        assert!(validate_external_source(&pem, &roots).is_err());
    }

    #[test]
    fn validate_external_rejects_symlink_escape() {
        let root = ws();
        let secret_dir = ws();
        let secret = secret_dir.path().join("secret.txt");
        fs::write(&secret, "x").unwrap();
        let roots = vec![root.path().canonicalize().unwrap()];
        #[cfg(unix)]
        {
            let link = root.path().join("innocent.txt");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            assert!(
                validate_external_source(&link, &roots).is_err(),
                "a symlink under a blessed root pointing outside must be rejected"
            );
        }
    }

    #[test]
    fn sanitized_name_is_stable_and_safe() {
        let p = Path::new("/home/u/proj/AGENTS.md");
        let a = sanitized_external_name(p);
        let b = sanitized_external_name(p);
        assert_eq!(a, b);
        assert!(a.ends_with("-AGENTS.md"));
        assert!(!a.contains('/'));
    }

    #[test]
    fn external_export_skips_inert_and_invalid() {
        let root = ws();
        let good = root.path().join("AGENTS.md");
        fs::write(&good, "ops").unwrap();
        let roots = vec![root.path().canonicalize().unwrap()];

        let mut list = IncludeList::default();
        // Verified + valid → packable.
        list.add_external(
            "aa-AGENTS.md",
            good.canonicalize().unwrap().to_str().unwrap(),
        );
        // Inert (restored, unverified) → skipped.
        list.files.push(IncludeEntry {
            path: "bb-OLD.md".into(),
            added_at: None,
            external: true,
            source: Some("/some/old/OLD.md".into()),
            verified: false,
            extra: HashMap::new(),
        });

        let (packable, skipped) = external_entries_for_export(&list, &roots);
        assert_eq!(packable.len(), 1);
        assert!(packable[0].0.starts_with("external/"));
        assert!(skipped.iter().any(|s| s.contains("inert")));
    }

    #[test]
    fn legacy_include_list_deserializes_without_new_fields() {
        // Back-compat: an old list (no external/verified) still loads.
        let dir = ws();
        fs::write(
            dir.path().join(INCLUDE_FILE),
            r#"{"files":[{"path":"notes.txt","added_at":null}]}"#,
        )
        .unwrap();
        let list = IncludeList::load(dir.path()).unwrap();
        assert_eq!(list.files.len(), 1);
        assert!(!list.files[0].external);
        assert!(
            list.files[0].verified,
            "missing verified must default to true"
        );
    }

    #[test]
    fn save_leaves_no_temp_sibling() {
        let dir = ws();
        let mut list = IncludeList::default();
        list.add("notes.txt");
        list.save(dir.path()).unwrap();
        // The atomic write must leave exactly the include file, no `.tmp.`.
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![INCLUDE_FILE.to_string()]);
        // And the saved list reloads.
        assert_eq!(
            IncludeList::load(dir.path()).unwrap().paths(),
            vec!["notes.txt"]
        );
    }

    #[test]
    fn validate_external_rejects_oversize_file() {
        let root = ws();
        let big = root.path().join("huge.bin");
        // Sparse file: instant to create, reports the oversize length.
        let f = fs::File::create(&big).unwrap();
        f.set_len(crate::MAX_RAW_ENTRY_BYTES + 1).unwrap();
        drop(f);
        let roots = vec![root.path().canonicalize().unwrap()];
        let err = validate_external_source(&big, &roots)
            .expect_err("a file over MAX_RAW_ENTRY_BYTES must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("per-file cap"), "unexpected message: {msg}");
        assert!(
            msg.contains(&crate::MAX_RAW_ENTRY_BYTES.to_string()),
            "message must name the cap: {msg}"
        );
        assert!(
            msg.contains(&(crate::MAX_RAW_ENTRY_BYTES + 1).to_string()),
            "message must name the size: {msg}"
        );

        // Exactly at the cap is still fine.
        let ok = root.path().join("edge.bin");
        let f = fs::File::create(&ok).unwrap();
        f.set_len(crate::MAX_RAW_ENTRY_BYTES).unwrap();
        drop(f);
        assert!(validate_external_source(&ok, &roots).is_ok());
    }

    /// MIN-7: forward compatibility for the include list. `.alf-include.json`
    /// is rewritten in place by routine operations — `mark_external_inert` on
    /// EVERY restore/import (all four adapters) and `prune_and_log_missing` on
    /// every sync — and it travels inside the archive, so a strip propagates to
    /// the cloud and every other machine. Mixed versions are routine here (a
    /// runtime image bakes `alf` at build time and lags the user's laptop), so
    /// an older binary must round-trip fields it does not know: without this,
    /// the older binary silently deletes a newer version's data on a plain
    /// restore. This must ship BEFORE the first version that adds a field —
    /// a deployed binary cannot be taught to preserve what it never knew.
    #[test]
    fn unknown_fields_survive_every_rewrite_path() {
        let dir = ws();
        fs::write(dir.path().join("kept.txt"), "k").unwrap();
        // A list as a FUTURE alf would write it: unknown keys at both the entry
        // and document level.
        fs::write(
            dir.path().join(INCLUDE_FILE),
            r#"{"files":[
                {"path":"kept.txt","added_at":"2026-01-01T00:00:00Z","pinned":true,
                 "future_entry_field":{"nested":1}},
                {"path":"aa-EXT.md","external":true,"source":"/proj/EXT.md",
                 "verified":true,"pinned":false}
            ],"future_doc_field":"kept"}"#,
        )
        .unwrap();

        // Rewrite path 1: every restore/import.
        assert_eq!(mark_external_inert(dir.path()).unwrap(), 1);
        // Rewrite path 2: every sync.
        prune_and_log_missing(dir.path()).unwrap();

        let raw = fs::read_to_string(dir.path().join(INCLUDE_FILE)).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            doc["future_doc_field"], "kept",
            "document-level unknown field stripped: {raw}"
        );
        let kept = &doc["files"][0];
        assert_eq!(kept["pinned"], true, "entry unknown field stripped: {raw}");
        assert_eq!(
            kept["future_entry_field"]["nested"], 1,
            "nested unknown value mangled: {raw}"
        );
        // The known fields still work — the external really was flipped inert.
        let ext = doc["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["path"] == "aa-EXT.md")
            .expect("external entry survived");
        assert_eq!(ext["verified"], false, "known-field behavior regressed");
        assert_eq!(
            ext["pinned"], false,
            "unknown field lost on the flipped entry"
        );
    }

    #[test]
    fn mark_external_inert_flips_only_verified_externals() {
        let dir = ws();
        let mut list = IncludeList::default();
        list.add("in-workspace.txt"); // in-workspace: untouched
        list.files.push(IncludeEntry {
            path: "aa-EXT.md".into(),
            added_at: None,
            external: true,
            source: Some("/proj/EXT.md".into()),
            verified: true, // verified external: flipped
            extra: HashMap::new(),
        });
        list.files.push(IncludeEntry {
            path: "bb-OLD.md".into(),
            added_at: None,
            external: true,
            source: Some("/proj/OLD.md".into()),
            verified: false, // already inert: stays
            extra: HashMap::new(),
        });
        list.save(dir.path()).unwrap();

        assert_eq!(mark_external_inert(dir.path()).unwrap(), 1);
        let after = IncludeList::load(dir.path()).unwrap();
        assert!(
            after.files.iter().all(|e| !e.external || !e.verified),
            "every external entry must be inert"
        );
        let in_ws = after
            .files
            .iter()
            .find(|e| e.path == "in-workspace.txt")
            .unwrap();
        assert!(in_ws.verified, "in-workspace entries must be untouched");

        // Idempotent: a second pass changes nothing.
        assert_eq!(mark_external_inert(dir.path()).unwrap(), 0);

        // Missing include file is a no-op, not an error.
        let empty = ws();
        assert_eq!(mark_external_inert(empty.path()).unwrap(), 0);
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
