//! Export an OpenClaw workspace to an `.alf` archive.
//!
//! Orchestrates the parsers (memory, identity, principals, credentials),
//! groups memory records into time-based partitions, preserves raw source
//! files, and writes the archive using `AlfWriter`.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::{
    AgentMetadata, AlfWriter, CredentialsLayerInfo, FileEntry, IdentityLayerInfo, LayerInventory,
    Manifest, MemoryInventory, MemoryPartitionInfo, PartitionAssigner, PrincipalsLayerInfo,
    WorkspaceEnumeration,
};

use crate::identity_parser;
use crate::memory_parser;
use crate::principals_parser;
use crate::ExportReport;

// ---------------------------------------------------------------------------
// Agent ID persistence
// ---------------------------------------------------------------------------

/// UUID v5 namespace for deriving agent IDs from workspace paths.
const AGENT_ID_NS: Uuid = Uuid::from_bytes([
    0x61, 0x6c, 0x66, 0x2d, 0x61, 0x67, 0x65, 0x6e, // "alf-agen"
    0x74, 0x2d, 0x69, 0x64, 0x2d, 0x6e, 0x73, 0x31, // "t-id-ns1"
]);

/// Resolve the agent UUID without writing anything.
///
/// If `{workspace}/.alf-agent-id` exists, read it. Otherwise derive a
/// deterministic UUID v5 from the canonical workspace path. Unlike
/// [`resolve_agent_id`], a freshly-derived id is **not** persisted — this is
/// the read-only path used by `export --dry-run` and the adapter's
/// `Adapter::resolve_agent_id` (WP0 selector/discovery).
pub(crate) fn resolve_agent_id_readonly(workspace: &Path) -> Result<Uuid> {
    let id_file = workspace.join(".alf-agent-id");
    if id_file.is_file() {
        let raw = fs::read_to_string(&id_file).context("Failed to read .alf-agent-id")?;
        let id = Uuid::parse_str(raw.trim()).context("Invalid UUID in .alf-agent-id")?;
        return Ok(id);
    }
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    Ok(Uuid::new_v5(
        &AGENT_ID_NS,
        canonical.to_string_lossy().as_bytes(),
    ))
}

/// Read or generate the agent UUID.
///
/// If `{workspace}/.alf-agent-id` exists, read it. Otherwise generate a
/// deterministic UUID v5 from the canonical workspace path and persist it.
fn resolve_agent_id(workspace: &Path) -> Result<Uuid> {
    let id = resolve_agent_id_readonly(workspace)?;
    let id_file = workspace.join(".alf-agent-id");
    if !id_file.is_file() {
        // Persist for stability across future exports.
        let _ = fs::write(&id_file, id.to_string());
    }
    Ok(id)
}

// ---------------------------------------------------------------------------
// OpenClaw version detection
// ---------------------------------------------------------------------------

/// Try to detect the OpenClaw gateway version from `~/.openclaw/openclaw.json`.
fn detect_openclaw_version() -> Option<String> {
    let home = dirs_home()?;
    let config_path = home.join(".openclaw").join("openclaw.json");
    let content = fs::read_to_string(config_path).ok()?;
    // Look for meta.lastTouchedVersion in the JSON
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("meta")?
        .get("lastTouchedVersion")?
        .as_str()
        .map(|s| s.to_string())
}

/// Best-effort home directory (honors `ALF_HOME`).
fn dirs_home() -> Option<std::path::PathBuf> {
    alf_core::home_dir()
}

// ---------------------------------------------------------------------------
// Raw source collection
// ---------------------------------------------------------------------------

/// Files at the workspace root that should be preserved as raw sources.
const ROOT_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "TOOLS.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
    "MEMORY.md",
];

/// The set of files an export would archive, before their contents are read.
pub struct EnumerationResult {
    pub files: Vec<FileEntry>,
    pub excluded_by_alfignore: u32,
    pub alfignore_warnings: Vec<String>,
    /// Paths in the agent's include list (`alf add`) that no longer exist on
    /// disk. Reported, not pruned — `alf sync` prunes them and logs the removal.
    pub missing_includes: Vec<String>,
}

/// Load `<workspace>/.alfignore` into a gitignore matcher.
///
/// A missing file yields an empty matcher (nothing excluded) and no warning.
/// A malformed file also yields an empty matcher plus a warning — filtering is
/// skipped rather than failing the export, so a broken `.alfignore` can never
/// silently drop files or block a backup.
fn load_alfignore(workspace: &Path) -> (Gitignore, Vec<String>) {
    let path = workspace.join(".alfignore");
    if !path.is_file() {
        return (Gitignore::empty(), Vec::new());
    }
    let mut warnings = Vec::new();
    let mut builder = GitignoreBuilder::new(workspace);
    if let Some(err) = builder.add(&path) {
        warnings.push(format!(
            ".alfignore could not be read ({err}); continuing without filtering"
        ));
        return (Gitignore::empty(), warnings);
    }
    match builder.build() {
        Ok(matcher) => (matcher, warnings),
        Err(err) => {
            warnings.push(format!(
                ".alfignore is unparseable ({err}); continuing without filtering"
            ));
            (Gitignore::empty(), warnings)
        }
    }
}

/// Whether a workspace-relative path is excluded by the `.alfignore` matcher.
///
/// `matched_path_or_any_parents` is required so a directory pattern such as
/// `memory/` excludes everything beneath it, and so negations re-include.
fn is_alfignored(matcher: &Gitignore, rel: &str) -> bool {
    matcher
        .matched_path_or_any_parents(rel, /* is_dir = */ false)
        .is_ignore()
}

/// Enumerate the workspace files an export would preserve as raw sources.
///
/// This is the single source of truth for the export file list — both the
/// real `export` and `export --dry-run` go through it. The candidate set is
/// the [`ROOT_FILES`] plus everything under `memory/`, filtered through
/// `<workspace>/.alfignore` if that file exists. The memory walk is sorted so
/// the file list is deterministic across platforms.
///
/// `.alfignore` itself is never a candidate (it is neither a root file nor
/// under `memory/`), so it can never appear in the result.
pub fn enumerate(workspace: &Path) -> Result<EnumerationResult> {
    let (matcher, mut warnings) = load_alfignore(workspace);
    let mut files = Vec::new();
    let mut excluded: u32 = 0;

    // Root-level files.
    for name in ROOT_FILES {
        let path = workspace.join(name);
        if !path.is_file() {
            continue;
        }
        if is_alfignored(&matcher, name) {
            excluded += 1;
            warnings.push(format!(".alfignore excludes the structural file {name}"));
            continue;
        }
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        files.push(FileEntry {
            path: name.to_string(),
            size,
        });
    }

    // memory/ directory — collected and sorted for a deterministic file list.
    let memory_dir = workspace.join("memory");
    if memory_dir.is_dir() {
        let mut walked: Vec<(String, PathBuf)> = WalkDir::new(&memory_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| {
                let rel = e
                    .path()
                    .strip_prefix(workspace)
                    .unwrap_or(e.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                (rel, e.path().to_path_buf())
            })
            .collect();
        walked.sort();
        for (rel, abs) in &walked {
            if is_alfignored(&matcher, rel) {
                excluded += 1;
                continue;
            }
            let size = fs::metadata(abs).map(|m| m.len()).unwrap_or(0);
            files.push(FileEntry {
                path: rel.clone(),
                size,
            });
        }
    }

    // Agent-managed include list — arbitrary files the agent opted into via
    // `alf add`. ALF never auto-discovers; only explicitly-tracked files are
    // added (raw only — no semantic parse). A malformed list degrades to empty
    // with a warning rather than blocking the backup (same posture as
    // `.alfignore`).
    let mut missing_includes = Vec::new();
    let mut seen: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
    let include = match alf_core::include::IncludeList::load(workspace) {
        Ok(list) => list,
        Err(err) => {
            warnings.push(format!(
                "{} could not be read ({err}); tracked files not synced this run",
                alf_core::include::INCLUDE_FILE
            ));
            alf_core::include::IncludeList::default()
        }
    };
    for rel in include.paths() {
        if seen.contains(&rel) {
            continue; // already captured (e.g. a ROOT_FILE or memory/ file)
        }
        let abs = workspace.join(&rel);
        if !abs.is_file() {
            missing_includes.push(rel);
            continue;
        }
        // A4.2: re-validate that a (possibly restored/edited) stored entry still
        // resolves inside the workspace before packing it.
        if let Err(e) = alf_core::include::safe_include_path(workspace, &rel) {
            warnings.push(format!("ignoring tracked path {rel}: {e}"));
            continue;
        }
        if is_alfignored(&matcher, &rel) {
            excluded += 1;
            continue;
        }
        let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
        files.push(FileEntry {
            path: rel.clone(),
            size,
        });
        seen.insert(rel);
    }

    // The include list and sync log themselves travel as raw so the agent's
    // sync config and removal history persist across machines on restore.
    for sentinel in [
        alf_core::include::INCLUDE_FILE,
        alf_core::include::SYNC_LOG_FILE,
    ] {
        if seen.contains(sentinel) {
            continue;
        }
        let abs = workspace.join(sentinel);
        if abs.is_file() {
            let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            files.push(FileEntry {
                path: sentinel.to_string(),
                size,
            });
            seen.insert(sentinel.to_string());
        }
    }

    Ok(EnumerationResult {
        files,
        excluded_by_alfignore: excluded,
        alfignore_warnings: warnings,
        missing_includes,
    })
}

/// Build the `export --dry-run` preview: the enumerated file list plus the
/// agent name and memory-record count.
///
/// Strictly read-only — writes no archive and, unlike a real export, never
/// persists `.alf-agent-id`.
pub fn enumerate_workspace(workspace: &Path) -> Result<WorkspaceEnumeration> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    let enumeration = enumerate(workspace)?;
    let total_size = enumeration.files.iter().map(|f| f.size).sum();

    let agent_name = identity_parser::resolve_agent_display_name(workspace);
    let agent_id = resolve_agent_id_readonly(workspace)?;
    let memory_records = memory_parser::collect_all_memory(workspace, agent_id)?.len() as u64;

    // Surface (but do not prune — this is read-only) tracked files that have
    // gone missing, so a dry-run preview shows what `alf sync` would drop.
    let mut warnings = enumeration.alfignore_warnings;
    for rel in &enumeration.missing_includes {
        warnings.push(format!(
            "tracked file {rel} no longer exists (will be removed from sync on next `alf sync`)"
        ));
    }

    Ok(WorkspaceEnumeration {
        agent_name,
        memory_records,
        files: enumeration.files,
        excluded_by_alfignore: enumeration.excluded_by_alfignore,
        total_size,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Partition grouping
// ---------------------------------------------------------------------------

/// Quarter start date for a given partition label.
fn quarter_start(year: i32, quarter: u32) -> NaiveDate {
    let month = (quarter - 1) * 3 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).unwrap()
}

/// Quarter end date (inclusive) for a given partition label.
fn quarter_end(year: i32, quarter: u32) -> NaiveDate {
    let end_month = quarter * 3;
    let last_day = match end_month {
        3 => 31,
        6 => 30,
        9 => 30,
        12 => 31,
        _ => 30,
    };
    NaiveDate::from_ymd_opt(year, end_month, last_day).unwrap()
}

// ---------------------------------------------------------------------------
// Agent vault (Layer 4)
// ---------------------------------------------------------------------------

/// Load the agent-managed ALF vault — the `CredentialsDocument` the agent
/// builds explicitly with `alf vault add`.
///
/// This is the ONLY source of the archive's Layer 4. ALF deliberately does
/// not capture a runtime's own keystore (e.g. OpenClaw `auth-profiles.json`):
/// the agent chooses what to back up. Vault records are already AEAD-encrypted,
/// so they enter the archive verbatim. Returns `None` when the vault file is
/// missing, unreadable, or has no records — the graceful-degradation contract
/// export relies on for every optional layer.
fn load_agent_vault(vault_path: Option<&Path>) -> Result<Option<alf_core::CredentialsDocument>> {
    let path = match vault_path {
        Some(p) if p.is_file() => p,
        _ => return Ok(None),
    };
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    match serde_json::from_str::<alf_core::CredentialsDocument>(&content) {
        Ok(doc) if doc.credentials.is_empty() => Ok(None),
        Ok(doc) => Ok(Some(doc)),
        Err(_) => Ok(None), // graceful degradation
    }
}

// ---------------------------------------------------------------------------
// Export entry point
// ---------------------------------------------------------------------------

/// Export an OpenClaw workspace to an `.alf` archive.
///
/// Layer 4 (credentials) is the agent's explicit ALF vault — see
/// [`load_agent_vault`]. Its records are already AEAD-encrypted; export never
/// touches a vault key.
pub fn export(workspace: &Path, output: &Path) -> Result<ExportReport> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    // 1. Agent ID
    let agent_id = resolve_agent_id(workspace)?;
    let agent_name = identity_parser::resolve_agent_display_name(workspace);
    let runtime_version = detect_openclaw_version();

    // 2. Collect memory records
    let records = memory_parser::collect_all_memory(workspace, agent_id)?;
    let total_records = records.len() as u64;

    // 3. Group records into partitions
    let mut partition_groups: BTreeMap<String, Vec<alf_core::MemoryRecord>> = BTreeMap::new();
    for record in records {
        let label = PartitionAssigner::partition_for_record(&record);
        partition_groups.entry(label).or_default().push(record);
    }

    // 4. Build partition infos
    let mut partition_infos: Vec<(MemoryPartitionInfo, Vec<alf_core::MemoryRecord>)> = Vec::new();
    for (file_path, group_records) in &partition_groups {
        // Parse the label to get year/quarter for from/to dates
        // file_path is like "memory/2026-Q1.jsonl"
        let label = file_path
            .trim_start_matches("memory/")
            .trim_end_matches(".jsonl");
        let parts: Vec<&str> = label.split("-Q").collect();
        let (from, to) = if parts.len() == 2 {
            let year: i32 = parts[0].parse().unwrap_or(2026);
            let quarter: u32 = parts[1].parse().unwrap_or(1);
            (
                quarter_start(year, quarter),
                Some(quarter_end(year, quarter)),
            )
        } else {
            (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), None)
        };

        let info = MemoryPartitionInfo {
            file: file_path.clone(),
            from,
            to,
            record_count: group_records.len() as u64,
            sealed: false, // current export is never sealed
            extra: std::collections::HashMap::new(),
        };
        partition_infos.push((info, group_records.clone()));
    }

    // 5. Build other layers
    let identity = identity_parser::build_identity(workspace, agent_id)?;
    let principals = principals_parser::build_principals(workspace, agent_id)?;

    // Layer 4 = the agent's explicit ALF vault ONLY. ALF never captures a
    // runtime's own keystore (e.g. OpenClaw `auth-profiles.json`) — the agent
    // chooses what to back up via `alf vault add`. Vault records are already
    // AEAD-encrypted, so they enter the archive verbatim. Per-agent path
    // (WP1): the CLI migrates any legacy install-scoped vault before export,
    // so there is deliberately no legacy fallback here.
    let vault_path = dirs_home().map(|h| alf_core::agent_vault_path(&h, agent_id));
    let credentials = load_agent_vault(vault_path.as_deref())?;

    // 6. Build manifest
    let has_identity = identity.is_some();
    let identity_version = identity.as_ref().map(|i| i.version);
    let principals_count = principals
        .as_ref()
        .map(|p| p.principals.len() as u32)
        .unwrap_or(0);
    let credentials_count = credentials
        .as_ref()
        .map(|c| c.credentials.len() as u32)
        .unwrap_or(0);

    let manifest = Manifest {
        alf_version: "1.0.0".to_string(),
        created_at: Utc::now(),
        agent: AgentMetadata {
            id: agent_id,
            name: agent_name.clone(),
            source_runtime: "openclaw".to_string(),
            source_runtime_version: runtime_version,
            extra: std::collections::HashMap::new(),
        },
        layers: LayerInventory {
            identity: if has_identity {
                Some(IdentityLayerInfo {
                    version: identity_version.unwrap_or(1),
                    file: "identity/identity.json".to_string(),
                    extra: std::collections::HashMap::new(),
                })
            } else {
                None
            },
            principals: if principals_count > 0 {
                Some(PrincipalsLayerInfo {
                    count: principals_count,
                    file: "principals/principals.json".to_string(),
                    extra: std::collections::HashMap::new(),
                })
            } else {
                None
            },
            credentials: if credentials_count > 0 {
                Some(CredentialsLayerInfo {
                    count: credentials_count,
                    file: "credentials/credentials.json".to_string(),
                    extra: std::collections::HashMap::new(),
                })
            } else {
                None
            },
            memory: Some(MemoryInventory {
                record_count: total_records,
                index_file: "memory/index.json".to_string(),
                partitions: partition_infos
                    .iter()
                    .map(|(info, _)| info.clone())
                    .collect(),
                has_embeddings: Some(false),
                has_raw_source: Some(true),
                extra: std::collections::HashMap::new(),
            }),
            attachments: None,
            extra: std::collections::HashMap::new(),
        },
        runtime_hints: None,
        sync: None,
        raw_sources: vec!["openclaw".to_string()],
        checksum: None,
        extra: std::collections::HashMap::new(),
    };

    // 7. Write archive
    let file = File::create(output)
        .with_context(|| format!("Failed to create output file: {}", output.display()))?;
    let writer = BufWriter::new(file);
    let mut alf_writer = AlfWriter::new(writer, manifest)?;

    // Identity
    if let Some(ref id) = identity {
        alf_writer.set_identity(id)?;
    }

    // Principals
    if let Some(ref p) = principals {
        alf_writer.set_principals(p)?;
    }

    // Credentials
    if let Some(ref c) = credentials {
        alf_writer.set_credentials(c)?;
    }

    // Memory partitions
    for (info, group_records) in &partition_infos {
        alf_writer.add_memory_partition(info.clone(), group_records)?;
    }

    // Raw sources — `enumerate` is the single source of truth for the file set.
    let enumeration = enumerate(workspace)?;
    let excluded_by_alfignore = enumeration.excluded_by_alfignore;
    let missing_includes = enumeration.missing_includes.clone();
    let mut raw_source_names = Vec::with_capacity(enumeration.files.len());
    for entry in &enumeration.files {
        let data = fs::read(workspace.join(&entry.path))
            .with_context(|| format!("Failed to read raw source {}", entry.path))?;
        alf_writer.add_raw_source("openclaw", &entry.path, &data)?;
        raw_source_names.push(entry.path.clone());
    }

    let inner = alf_writer.finish()?;
    drop(inner); // flush and close

    // Get output file size
    let output_size = fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(ExportReport {
        agent_name,
        alf_version: "1.0.0".to_string(),
        memory_records: total_records,
        identity_version,
        principals_count,
        credentials_count,
        attachments_count: 0,
        raw_sources: raw_source_names,
        output_path: output.to_string_lossy().to_string(),
        output_size_bytes: output_size,
        excluded_by_alfignore,
        missing_includes,
        warnings: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_named_workspace(name: &str, files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join(name);
        fs::create_dir_all(&workspace).unwrap();
        for (name, content) in files {
            let path = workspace.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        (dir, workspace)
    }

    fn create_workspace(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        create_named_workspace("workspace", files)
    }

    #[test]
    fn export_uses_workspace_basename_when_identity_name_is_missing() {
        let (_dir, workspace) = create_named_workspace(
            "test-agent",
            &[
                ("SOUL.md", "# TestAgent\n\nA test agent."),
                ("MEMORY.md", "## Facts\n\nThe sky is blue."),
            ],
        );
        let output = workspace.join("test.alf");

        let report = export(&workspace, &output).unwrap();
        assert_eq!(report.agent_name, "test-agent");
        assert_eq!(report.memory_records, 1);
        assert!(report.identity_version.is_some());
        assert!(output.is_file());
        assert!(report.output_size_bytes > 0);
    }

    #[test]
    fn export_uses_identity_name_field_when_present() {
        let (_dir, workspace) = create_workspace(&[
            ("SOUL.md", "# SOUL.md - Who You Are\n\nTemplate soul text."),
            (
                "IDENTITY.md",
                "# IDENTITY.md - Who Am I?\n\n- **Name:** Kleo\n- **Creature:** Cyberhuman",
            ),
            ("MEMORY.md", "## Facts\n\nThe sky is blue."),
        ]);
        let output = workspace.join("test.alf");

        let report = export(&workspace, &output).unwrap();
        assert_eq!(report.agent_name, "Kleo");
    }

    #[test]
    fn export_with_daily_logs() {
        let (_dir, workspace) = create_workspace(&[
            ("SOUL.md", "# Agent\n\nHello."),
            (
                "memory/2026-01-15.md",
                "## Morning\n\nDid stuff.\n\n## Afternoon\n\nMore stuff.",
            ),
            ("memory/2026-01-16.md", "## All day\n\nBusy day."),
        ]);
        let output = workspace.join("test.alf");

        let report = export(&workspace, &output).unwrap();
        // 2 sections from Jan 15 + 1 from Jan 16
        assert_eq!(report.memory_records, 3);
    }

    #[test]
    fn export_preserves_raw_sources() {
        let (_dir, workspace) = create_workspace(&[
            ("SOUL.md", "# Bot\n\nSoul."),
            ("USER.md", "# Alice\n\nProfile."),
            ("TOOLS.md", "Tool notes."),
        ]);
        let output = workspace.join("test.alf");

        let report = export(&workspace, &output).unwrap();
        assert!(report.raw_sources.contains(&"SOUL.md".to_string()));
        assert!(report.raw_sources.contains(&"USER.md".to_string()));
        assert!(report.raw_sources.contains(&"TOOLS.md".to_string()));
    }

    #[test]
    fn agent_id_is_stable() {
        let (_dir, workspace) = create_workspace(&[("SOUL.md", "# X\n\nTest.")]);
        let output1 = workspace.join("test1.alf");
        let output2 = workspace.join("test2.alf");

        export(&workspace, &output1).unwrap();
        export(&workspace, &output2).unwrap();

        // .alf-agent-id should have been written
        let id_file = workspace.join(".alf-agent-id");
        assert!(id_file.is_file());
    }

    #[test]
    fn export_nonexistent_workspace() {
        let result = export(Path::new("/nonexistent/path"), Path::new("/tmp/out.alf"));
        assert!(result.is_err());
    }

    #[test]
    fn load_agent_vault_reads_credentials_json() {
        use alf_core::{CredentialRecord, CredentialType, CredentialsDocument, EncryptionMetadata};
        use std::collections::HashMap;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("credentials.json");
        let doc = CredentialsDocument {
            credentials: vec![CredentialRecord {
                id: Uuid::nil(),
                agent_id: Uuid::nil(),
                service: "email".into(),
                credential_type: CredentialType::Account,
                encrypted_payload: "ZmFrZQ==".into(),
                encryption: EncryptionMetadata {
                    algorithm: "xchacha20-poly1305".into(),
                    nonce: "bm9uY2U=".into(),
                    kdf: None,
                    kdf_params: None,
                    extra: HashMap::new(),
                },
                created_at: Utc::now(),
                label: Some("me@example.com".into()),
                description: None,
                capabilities_granted: vec![],
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags: vec!["alf-vault".into()],
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        };
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let loaded = load_agent_vault(Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.credentials.len(), 1);
        assert_eq!(loaded.credentials[0].service, "email");
    }

    #[test]
    fn load_agent_vault_missing_or_empty_returns_none() {
        use alf_core::CredentialsDocument;
        use std::collections::HashMap;

        let dir = TempDir::new().unwrap();
        assert!(load_agent_vault(None).unwrap().is_none());
        assert!(load_agent_vault(Some(&dir.path().join("nope.json")))
            .unwrap()
            .is_none());

        let empty = dir.path().join("empty.json");
        let doc = CredentialsDocument {
            credentials: vec![],
            extra: HashMap::new(),
        };
        fs::write(&empty, serde_json::to_string(&doc).unwrap()).unwrap();
        assert!(load_agent_vault(Some(&empty)).unwrap().is_none());
    }

    // --- .alfignore enumeration (EX-1..7) ------------------------------------

    fn enumerated_paths(result: &EnumerationResult) -> Vec<String> {
        result.files.iter().map(|f| f.path.clone()).collect()
    }

    /// EX-1: with no `.alfignore`, `enumerate` lists every candidate file.
    #[test]
    fn ex1_enumerate_without_alfignore_lists_all_candidates() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("MEMORY.md", "mem"),
            ("memory/2026-01-15.md", "a"),
            ("memory/2026-01-16.md", "b"),
        ]);
        let result = enumerate(&ws).unwrap();
        assert_eq!(
            enumerated_paths(&result),
            vec![
                "SOUL.md",
                "MEMORY.md",
                "memory/2026-01-15.md",
                "memory/2026-01-16.md",
            ]
        );
        assert_eq!(result.excluded_by_alfignore, 0);
        assert!(result.alfignore_warnings.is_empty());
    }

    /// EX-2: a `.alfignore` naming one file excludes only that file.
    #[test]
    fn ex2_alfignore_excludes_a_single_file() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("memory/2026-01-15.md", "a"),
            ("memory/2026-01-16.md", "b"),
            (".alfignore", "memory/2026-01-15.md\n"),
        ]);
        let result = enumerate(&ws).unwrap();
        assert_eq!(result.excluded_by_alfignore, 1);
        assert_eq!(
            enumerated_paths(&result),
            vec!["SOUL.md", "memory/2026-01-16.md"]
        );
    }

    /// EX-3: a `memory/` directory pattern excludes every memory file.
    #[test]
    fn ex3_alfignore_excludes_entire_memory_dir() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("memory/2026-01-15.md", "a"),
            ("memory/2026-01-16.md", "b"),
            (".alfignore", "memory/\n"),
        ]);
        let result = enumerate(&ws).unwrap();
        assert_eq!(result.excluded_by_alfignore, 2);
        assert_eq!(enumerated_paths(&result), vec!["SOUL.md"]);
    }

    /// EX-4: a negation re-includes one file from an excluded directory.
    #[test]
    fn ex4_alfignore_negation_reincludes_a_file() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("memory/2026-01-15.md", "a"),
            ("memory/2026-01-16.md", "b"),
            (".alfignore", "memory/\n!memory/2026-01-15.md\n"),
        ]);
        let result = enumerate(&ws).unwrap();
        assert_eq!(result.excluded_by_alfignore, 1);
        assert_eq!(
            enumerated_paths(&result),
            vec!["SOUL.md", "memory/2026-01-15.md"]
        );
    }

    /// EX-5: `.alfignore` itself never appears in the file list.
    #[test]
    fn ex5_alfignore_file_is_never_listed() {
        let (_dir, ws) =
            create_workspace(&[("SOUL.md", "soul"), (".alfignore", "# excludes nothing\n")]);
        let result = enumerate(&ws).unwrap();
        assert!(!enumerated_paths(&result).contains(&".alfignore".to_string()));
    }

    /// EX-6: excluding a structural root file emits a warning naming it.
    #[test]
    fn ex6_warns_when_root_file_excluded() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("MEMORY.md", "mem"),
            (".alfignore", "SOUL.md\n"),
        ]);
        let result = enumerate(&ws).unwrap();
        assert!(!enumerated_paths(&result).contains(&"SOUL.md".to_string()));
        assert_eq!(result.excluded_by_alfignore, 1);
        assert!(result
            .alfignore_warnings
            .iter()
            .any(|w| w.contains("SOUL.md")));
    }

    /// EX-7: a malformed `.alfignore` never crashes enumeration or silently
    /// over-excludes — filtering is skipped, the candidate set stays intact.
    #[test]
    fn ex7_malformed_alfignore_does_not_over_exclude() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("memory/2026-01-15.md", "a"),
            (".alfignore", "[unterminated\n"),
        ]);
        let result = enumerate(&ws).unwrap();
        assert!(enumerated_paths(&result).contains(&"SOUL.md".to_string()));
        assert!(enumerated_paths(&result).contains(&"memory/2026-01-15.md".to_string()));
        assert_eq!(result.excluded_by_alfignore, 0);
    }

    // -- Include list (`alf add`) -----------------------------------------

    use alf_core::include::{IncludeList, INCLUDE_FILE, SYNC_LOG_FILE};

    /// A tracked arbitrary file (root + nested) is enumerated into raw, and the
    /// include list itself travels.
    #[test]
    fn include_tracks_arbitrary_files() {
        let (_dir, ws) = create_workspace(&[
            ("SOUL.md", "soul"),
            ("notes.txt", "hello"),
            ("my-project/data.csv", "a,b\n1,2\n"),
            ("untracked.txt", "should not sync"),
        ]);
        let mut list = IncludeList::default();
        list.add("notes.txt");
        list.add("my-project/data.csv");
        list.save(&ws).unwrap();

        let paths = enumerated_paths(&enumerate(&ws).unwrap());
        assert!(paths.contains(&"notes.txt".to_string()));
        assert!(paths.contains(&"my-project/data.csv".to_string()));
        // The include list itself is preserved so it travels on restore.
        assert!(paths.contains(&INCLUDE_FILE.to_string()));
        // Arbitrary files NOT opted in are never auto-discovered.
        assert!(!paths.contains(&"untracked.txt".to_string()));
    }

    /// A listed-but-missing tracked file is reported (not enumerated, not an error).
    #[test]
    fn include_missing_file_is_reported_not_enumerated() {
        let (_dir, ws) = create_workspace(&[("SOUL.md", "soul"), ("kept.txt", "k")]);
        let mut list = IncludeList::default();
        list.add("kept.txt");
        list.add("gone.txt"); // never created
        list.save(&ws).unwrap();

        let result = enumerate(&ws).unwrap();
        let paths = enumerated_paths(&result);
        assert!(paths.contains(&"kept.txt".to_string()));
        assert!(!paths.contains(&"gone.txt".to_string()));
        assert_eq!(result.missing_includes, vec!["gone.txt".to_string()]);
    }

    /// An empty tracked file is enumerated (zero-byte is valid).
    #[test]
    fn include_empty_file_is_enumerated() {
        let (_dir, ws) = create_workspace(&[("SOUL.md", "soul"), ("empty.txt", "")]);
        let mut list = IncludeList::default();
        list.add("empty.txt");
        list.save(&ws).unwrap();

        let result = enumerate(&ws).unwrap();
        let entry = result
            .files
            .iter()
            .find(|f| f.path == "empty.txt")
            .expect("empty.txt enumerated");
        assert_eq!(entry.size, 0);
    }

    /// Tracking a path already captured by ROOT_FILES does not duplicate it.
    #[test]
    fn include_does_not_duplicate_known_file() {
        let (_dir, ws) = create_workspace(&[("SOUL.md", "soul")]);
        let mut list = IncludeList::default();
        list.add("SOUL.md");
        list.save(&ws).unwrap();

        let paths = enumerated_paths(&enumerate(&ws).unwrap());
        assert_eq!(paths.iter().filter(|p| *p == "SOUL.md").count(), 1);
    }

    /// The sync log, when present, travels as raw.
    #[test]
    fn include_sync_log_travels() {
        let (_dir, ws) = create_workspace(&[("SOUL.md", "soul")]);
        fs::write(ws.join(SYNC_LOG_FILE), "- 2026-01-01: removed x\n").unwrap();

        let paths = enumerated_paths(&enumerate(&ws).unwrap());
        assert!(paths.contains(&SYNC_LOG_FILE.to_string()));
    }

    /// A malformed include list degrades to empty + warning (never blocks backup).
    #[test]
    fn include_malformed_degrades_with_warning() {
        let (_dir, ws) = create_workspace(&[("SOUL.md", "soul")]);
        fs::write(ws.join(INCLUDE_FILE), "{ not json").unwrap();

        let result = enumerate(&ws).unwrap();
        // SOUL.md still enumerated despite the broken include list.
        assert!(enumerated_paths(&result).contains(&"SOUL.md".to_string()));
        assert!(result
            .alfignore_warnings
            .iter()
            .any(|w| w.contains(INCLUDE_FILE)));
    }
}
