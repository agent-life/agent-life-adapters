//! Export a ZeroClaw workspace to an `.alf` archive.
//!
//! Orchestrates: detect backend from `config.toml` → extract memory (SQLite
//! or Markdown) → build identity/principals/credentials → write archive.

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

use crate::config_parser::{self, MemoryBackend, ZeroClawConfig};
use crate::identity_parser;
use crate::markdown_parser;
use crate::principals_parser;
use crate::sqlite_extractor;
use crate::ExportReport;

// ---------------------------------------------------------------------------
// Agent ID persistence
// ---------------------------------------------------------------------------

/// UUID v5 namespace for deriving agent IDs from workspace paths.
const AGENT_ID_NS: Uuid = Uuid::from_bytes([
    0x61, 0x6c, 0x66, 0x2d, 0x7a, 0x63, 0x6c, 0x77, // "alf-zclw"
    0x2d, 0x61, 0x67, 0x65, 0x6e, 0x74, 0x2d, 0x31, // "-agent-1"
]);

/// Resolve the agent UUID without writing anything.
///
/// Reads `{workspace}/.alf-agent-id` if present, otherwise derives a
/// deterministic UUID v5 from the canonical workspace path. A freshly-derived
/// id is **not** persisted — the read-only path used by `export --dry-run`.
fn resolve_agent_id_readonly(workspace: &Path) -> Result<Uuid> {
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

/// Read or generate the agent UUID, persisting a freshly-derived id.
fn resolve_agent_id(workspace: &Path) -> Result<Uuid> {
    let id = resolve_agent_id_readonly(workspace)?;
    let id_file = workspace.join(".alf-agent-id");
    if !id_file.is_file() {
        let _ = fs::write(&id_file, id.to_string());
    }
    Ok(id)
}

// ---------------------------------------------------------------------------
// ZeroClaw directory detection
// ---------------------------------------------------------------------------

/// Locate the ZeroClaw home directory.
///
/// The `workspace` argument is typically `~/.zeroclaw/workspace/`, so the
/// ZeroClaw home is its parent. Falls back to `~/.zeroclaw` if not a child.
fn zeroclaw_home(workspace: &Path) -> std::path::PathBuf {
    if let Some(parent) = workspace.parent() {
        if parent.join("config.toml").is_file() || parent.join("memory.db").is_file() {
            return parent.to_path_buf();
        }
    }
    // Fallback: check if workspace itself contains config
    if workspace.join("config.toml").is_file() {
        return workspace.to_path_buf();
    }
    // Best guess
    workspace.parent().unwrap_or(workspace).to_path_buf()
}

/// Try to detect ZeroClaw version from workspace files or environment.
fn detect_zeroclaw_version(zc_home: &Path) -> Option<String> {
    // Check config.toml for a version field
    let config_path = zc_home.join("config.toml");
    if let Ok(content) = fs::read_to_string(&config_path) {
        if let Ok(val) = content.parse::<toml::Value>() {
            if let Some(v) = val.get("version").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Raw source collection
// ---------------------------------------------------------------------------

/// Root-level workspace files to preserve.
const ROOT_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "TOOLS.md",
    "HEARTBEAT.md",
];

/// The set of files an export would archive, before their contents are read.
pub struct EnumerationResult {
    pub files: Vec<FileEntry>,
    pub excluded_by_alfignore: u32,
    pub alfignore_warnings: Vec<String>,
    /// Paths in the agent's `alf add` include list that no longer exist on disk.
    pub missing_includes: Vec<String>,
}

/// Where a raw-source entry's bytes come from.
enum RawContent {
    /// Read verbatim from this absolute path.
    Disk(PathBuf),
    /// Synthesized bytes — ZeroClaw's redacted `config.toml`.
    Inline(Vec<u8>),
}

/// What `enumerate_raw` returns: the raw entries (each paired with where its
/// bytes come from), the `.alfignore` exclusion count, any warnings, and the
/// tracked-but-missing include paths.
type RawEnumeration = (Vec<(FileEntry, RawContent)>, u32, Vec<String>, Vec<String>);

/// Load `<workspace>/.alfignore` into a gitignore matcher.
///
/// A missing file yields an empty matcher (nothing excluded) and no warning.
/// A malformed file also yields an empty matcher plus a warning — filtering is
/// skipped rather than failing the export.
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
fn is_alfignored(matcher: &Gitignore, rel: &str) -> bool {
    matcher
        .matched_path_or_any_parents(rel, /* is_dir = */ false)
        .is_ignore()
}

/// Enumerate every raw-source entry alongside the location of its bytes.
///
/// Two buckets, deliberately treated differently by `.alfignore`:
/// - **Filterable** — the workspace-relative [`ROOT_FILES`] and `memory/**`.
///   `.alfignore` patterns (which are workspace-relative) apply to these.
/// - **Unfilterable** — the synthesized, redacted `config.toml` and the AIEOS
///   `identity.json` (which may live outside the workspace entirely). A
///   workspace-relative `.alfignore` pattern cannot meaningfully address these,
///   so they are always included, never matched, and never counted.
fn enumerate_raw(workspace: &Path, config: &ZeroClawConfig) -> RawEnumeration {
    let (matcher, mut warnings) = load_alfignore(workspace);
    let mut entries: Vec<(FileEntry, RawContent)> = Vec::new();
    let mut excluded: u32 = 0;
    let mut missing_includes: Vec<String> = Vec::new();

    // config.toml — synthesized & redacted; unfilterable.
    let redacted = config_parser::redact_secrets(&config.raw_toml).into_bytes();
    entries.push((
        FileEntry {
            path: "config.toml".to_string(),
            size: redacted.len() as u64,
        },
        RawContent::Inline(redacted),
    ));

    // Root-level workspace files — `.alfignore` applies.
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
        entries.push((
            FileEntry {
                path: name.to_string(),
                size,
            },
            RawContent::Disk(path),
        ));
    }

    // identity.json (AIEOS) — may be absolute / outside the workspace;
    // unfilterable.
    if let Some(ref aieos_path) = config.aieos_path {
        let path = if Path::new(aieos_path).is_absolute() {
            Path::new(aieos_path).to_path_buf()
        } else {
            workspace.join(aieos_path)
        };
        if path.is_file() {
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            entries.push((
                FileEntry {
                    path: "identity.json".to_string(),
                    size,
                },
                RawContent::Disk(path),
            ));
        }
    }

    // memory/ directory — collected sorted; `.alfignore` applies.
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
        for (rel, abs) in walked {
            if is_alfignored(&matcher, &rel) {
                excluded += 1;
                continue;
            }
            let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            entries.push((FileEntry { path: rel, size }, RawContent::Disk(abs)));
        }
    }

    // Agent-managed include list — arbitrary files the agent opted into via
    // `alf add` (runtime-agnostic; shared with OpenClaw via alf_core::include).
    // ALF never auto-discovers; only explicitly-tracked files are added (raw
    // only — no semantic parse). A malformed list degrades to empty with a
    // warning rather than blocking the backup (same posture as `.alfignore`).
    let mut seen: HashSet<String> = entries.iter().map(|(fe, _)| fe.path.clone()).collect();
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
        if is_alfignored(&matcher, &rel) {
            excluded += 1;
            continue;
        }
        let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
        entries.push((FileEntry { path: rel.clone(), size }, RawContent::Disk(abs)));
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
            entries.push((
                FileEntry {
                    path: sentinel.to_string(),
                    size,
                },
                RawContent::Disk(abs),
            ));
            seen.insert(sentinel.to_string());
        }
    }

    (entries, excluded, warnings, missing_includes)
}

/// Enumerate the workspace files an export would preserve as raw sources.
///
/// The single source of truth for the export file list — both the real
/// `export` and `export --dry-run` go through it.
pub fn enumerate(workspace: &Path) -> Result<EnumerationResult> {
    let zc_home = zeroclaw_home(workspace);
    let config = load_config(&zc_home)?;
    let (entries, excluded, warnings, missing_includes) = enumerate_raw(workspace, &config);
    Ok(EnumerationResult {
        files: entries.into_iter().map(|(fe, _)| fe).collect(),
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

    let zc_home = zeroclaw_home(workspace);
    let config = load_config(&zc_home)?;
    let (entries, excluded, mut warnings, missing_includes) = enumerate_raw(workspace, &config);
    let files: Vec<FileEntry> = entries.into_iter().map(|(fe, _)| fe).collect();
    let total_size = files.iter().map(|f| f.size).sum();

    let agent_id = resolve_agent_id_readonly(workspace)?;
    let agent_name = identity_parser::detect_agent_name(workspace, &config);
    let runtime_version = detect_zeroclaw_version(&zc_home);
    let records = extract_memory_records(
        workspace,
        &zc_home,
        &config,
        agent_id,
        runtime_version.as_deref(),
    )?;

    // Surface (but do not prune — this is read-only) tracked files that have
    // gone missing, so a dry-run preview shows what `alf sync` would drop.
    for rel in &missing_includes {
        warnings.push(format!(
            "tracked file {rel} no longer exists (will be removed from sync on next `alf sync`)"
        ));
    }

    Ok(WorkspaceEnumeration {
        agent_name,
        memory_records: records.len() as u64,
        files,
        excluded_by_alfignore: excluded,
        total_size,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Partition helpers
// ---------------------------------------------------------------------------

fn quarter_start(year: i32, quarter: u32) -> NaiveDate {
    let month = (quarter - 1) * 3 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).unwrap()
}

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

/// Best-effort home directory (honors `ALF_HOME`).
fn dirs_home() -> Option<std::path::PathBuf> {
    alf_core::home_dir()
}

/// Load the agent-managed ALF vault — the `CredentialsDocument` the agent
/// builds explicitly with `alf vault add`.
///
/// This is the ONLY source of the archive's Layer 4. ALF deliberately does
/// not capture a runtime's own keystore (e.g. ZeroClaw `config.toml`
/// `[secrets]`): the agent chooses what to back up. Vault records are already
/// AEAD-encrypted, so they enter the archive verbatim. Returns `None` when the
/// vault file is missing, unreadable, or has no records.
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
// Config + memory helpers (shared by export and dry-run enumeration)
// ---------------------------------------------------------------------------

/// Parse `config.toml` from the ZeroClaw home, falling back to defaults with
/// heuristic backend detection when no config file exists.
fn load_config(zc_home: &Path) -> Result<ZeroClawConfig> {
    let config_path = zc_home.join("config.toml");
    Ok(config_parser::parse_config(&config_path)?.unwrap_or_else(|| {
        let backend = config_parser::detect_backend_heuristic(zc_home);
        ZeroClawConfig {
            memory_backend: backend,
            auto_save: true,
            embedding_provider: "none".into(),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            identity_format: config_parser::IdentityFormat::OpenClaw,
            aieos_path: None,
            aieos_inline: None,
            secrets_encrypt: true,
            credential_hints: Vec::new(),
            raw_toml: String::new(),
        }
    }))
}

/// Extract memory records for the configured backend (SQLite or Markdown).
fn extract_memory_records(
    workspace: &Path,
    zc_home: &Path,
    config: &ZeroClawConfig,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<Vec<alf_core::MemoryRecord>> {
    let records = match config.memory_backend {
        MemoryBackend::Sqlite => {
            let db_path = zc_home.join("memory.db");
            if db_path.is_file() {
                sqlite_extractor::extract_from_sqlite(&db_path, config, agent_id, runtime_version)?
            } else {
                // SQLite configured but file missing — try markdown fallback.
                markdown_parser::collect_markdown_memory(workspace, agent_id, runtime_version)?
            }
        }
        MemoryBackend::Markdown => {
            markdown_parser::collect_markdown_memory(workspace, agent_id, runtime_version)?
        }
        MemoryBackend::None | MemoryBackend::Unsupported => Vec::new(),
    };
    Ok(records)
}

// ---------------------------------------------------------------------------
// Export entry point
// ---------------------------------------------------------------------------

/// Export a ZeroClaw workspace to an `.alf` archive.
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

    let zc_home = zeroclaw_home(workspace);

    // 1. Parse config
    let config = load_config(&zc_home)?;

    // 2. Agent ID + name
    let agent_id = resolve_agent_id(workspace)?;
    let agent_name = identity_parser::detect_agent_name(workspace, &config);
    let runtime_version = detect_zeroclaw_version(&zc_home);

    // 3. Extract memory records (based on backend)
    let records = extract_memory_records(
        workspace,
        &zc_home,
        &config,
        agent_id,
        runtime_version.as_deref(),
    )?;
    let total_records = records.len() as u64;

    // Check for embeddings in the record set
    let has_embeddings = records.iter().any(|r| !r.embeddings.is_empty());

    // 4. Group records into partitions
    let mut partition_groups: BTreeMap<String, Vec<alf_core::MemoryRecord>> = BTreeMap::new();
    for record in records {
        let label = PartitionAssigner::partition_for_record(&record);
        partition_groups.entry(label).or_default().push(record);
    }

    let mut partition_infos: Vec<(MemoryPartitionInfo, Vec<alf_core::MemoryRecord>)> = Vec::new();
    for (file_path, group_records) in &partition_groups {
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
            sealed: false,
            extra: std::collections::HashMap::new(),
        };
        partition_infos.push((info, group_records.clone()));
    }

    // 5. Build other layers
    let identity = identity_parser::parse_identity(workspace, &config, agent_id)?;
    let principals = principals_parser::parse_principals(workspace, agent_id)?;

    // Layer 4 = the agent's explicit ALF vault ONLY. ALF never captures a
    // runtime's own keystore (e.g. ZeroClaw `config.toml [secrets]`) — the
    // agent chooses what to back up via `alf vault add`. Vault records are
    // already AEAD-encrypted, so they enter the archive verbatim.
    let vault_path = dirs_home().map(|h| h.join(".alf").join("vault").join("credentials.json"));
    let credentials = load_agent_vault(vault_path.as_deref())?;

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

    // 6. Build manifest
    let manifest = Manifest {
        alf_version: "1.0.0".to_string(),
        created_at: Utc::now(),
        agent: AgentMetadata {
            id: agent_id,
            name: agent_name.clone(),
            source_runtime: "zeroclaw".to_string(),
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
                has_embeddings: Some(has_embeddings),
                has_raw_source: Some(true),
                extra: std::collections::HashMap::new(),
            }),
            attachments: None,
            extra: std::collections::HashMap::new(),
        },
        runtime_hints: None,
        sync: None,
        raw_sources: vec!["zeroclaw".to_string()],
        checksum: None,
        extra: std::collections::HashMap::new(),
    };

    // 7. Write archive
    let file = File::create(output)
        .with_context(|| format!("Failed to create output file: {}", output.display()))?;
    let writer = BufWriter::new(file);
    let mut alf_writer = AlfWriter::new(writer, manifest)?;

    if let Some(ref id) = identity {
        alf_writer.set_identity(id)?;
    }
    if let Some(ref p) = principals {
        alf_writer.set_principals(p)?;
    }
    if let Some(ref c) = credentials {
        alf_writer.set_credentials(c)?;
    }

    for (info, group_records) in &partition_infos {
        alf_writer.add_memory_partition(info.clone(), group_records)?;
    }

    // Raw sources — `enumerate_raw` is the single source of truth for the set.
    let (raw_entries, excluded_by_alfignore, _warnings, missing_includes) =
        enumerate_raw(workspace, &config);
    let mut raw_source_names = Vec::with_capacity(raw_entries.len());
    for (entry, content) in raw_entries {
        let data = match content {
            RawContent::Inline(bytes) => bytes,
            RawContent::Disk(path) => fs::read(&path)
                .with_context(|| format!("Failed to read raw source {}", path.display()))?,
        };
        alf_writer.add_raw_source("zeroclaw", &entry.path, &data)?;
        raw_source_names.push(entry.path);
    }

    let inner = alf_writer.finish()?;
    drop(inner);

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
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    /// Create a ZeroClaw-style directory structure:
    /// `{dir}/config.toml`, `{dir}/workspace/SOUL.md`, etc.
    fn create_zeroclaw_home(
        config_toml: &str,
        workspace_files: &[(&str, &str)],
    ) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let zc_home = dir.path().to_path_buf();
        let ws = zc_home.join("workspace");
        fs::create_dir_all(&ws).unwrap();

        fs::write(zc_home.join("config.toml"), config_toml).unwrap();

        for (name, content) in workspace_files {
            let path = ws.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        (dir, ws)
    }

    fn create_test_db(zc_home: &Path) {
        let db_path = zc_home.join("memory.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                key TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                embedding BLOB
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "a1b2c3d4-0000-0000-0000-000000000001",
                "user_pref",
                "User prefers Rust over Go",
                "core",
                "2026-01-15T10:00:00Z",
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "a1b2c3d4-0000-0000-0000-000000000002",
                "daily_log",
                "Reviewed migration plan",
                "daily",
                "2026-02-20T14:00:00Z",
            ],
        )
        .unwrap();
    }

    #[test]
    fn export_sqlite_workspace() {
        let config = r#"
[memory]
backend = "sqlite"
embedding_provider = "none"

[identity]
format = "openclaw"
"#;
        let (dir, ws) = create_zeroclaw_home(
            config,
            &[("SOUL.md", "# ZCAgent\n\nA test ZeroClaw agent.\n")],
        );
        create_test_db(dir.path());

        let output = dir.path().join("test.alf");
        let report = export(&ws, &output).unwrap();

        assert_eq!(report.agent_name, "ZCAgent");
        assert_eq!(report.memory_records, 2);
        assert!(report.identity_version.is_some());
        assert!(output.is_file());
        assert!(report.output_size_bytes > 0);
        assert!(report.raw_sources.contains(&"config.toml".to_string()));
    }

    #[test]
    fn export_markdown_workspace() {
        let config = r#"
[memory]
backend = "markdown"
"#;
        let (dir, ws) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# MdAgent\n\nMarkdown backend.\n"),
                (
                    "memory/2026-02-15.md",
                    "## Morning\n\nDid stuff.\n\n## Evening\n\nMore stuff.\n",
                ),
            ],
        );

        let output = dir.path().join("test.alf");
        let report = export(&ws, &output).unwrap();

        assert_eq!(report.agent_name, "MdAgent");
        assert_eq!(report.memory_records, 2);
    }

    #[test]
    fn agent_id_stability() {
        let config = "[memory]\nbackend = \"sqlite\"";
        let (dir, ws) = create_zeroclaw_home(config, &[("SOUL.md", "# Stable\n\nTest.\n")]);
        create_test_db(dir.path());

        let out1 = dir.path().join("out1.alf");
        let out2 = dir.path().join("out2.alf");
        export(&ws, &out1).unwrap();
        export(&ws, &out2).unwrap();

        assert!(ws.join(".alf-agent-id").is_file());
    }

    #[test]
    fn export_nonexistent_workspace() {
        let result = export(Path::new("/nonexistent"), Path::new("/tmp/out.alf"));
        assert!(result.is_err());
    }

    #[test]
    fn load_agent_vault_reads_and_handles_missing() {
        use alf_core::{CredentialRecord, CredentialType, CredentialsDocument, EncryptionMetadata};
        use std::collections::HashMap;

        let dir = TempDir::new().unwrap();
        assert!(load_agent_vault(None).unwrap().is_none());
        assert!(load_agent_vault(Some(&dir.path().join("nope.json")))
            .unwrap()
            .is_none());

        let path = dir.path().join("credentials.json");
        let doc = CredentialsDocument {
            credentials: vec![CredentialRecord {
                id: Uuid::nil(),
                agent_id: Uuid::nil(),
                service: "telegram".into(),
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
                label: Some("mybot".into()),
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
        assert_eq!(loaded.credentials[0].service, "telegram");
    }

    // --- .alfignore enumeration (ZeroClaw parity) ----------------------------

    /// `.alfignore` filters workspace files but never the synthesized
    /// `config.toml`, which is not a workspace path.
    #[test]
    fn enumerate_alfignore_filters_workspace_not_config() {
        let config = "[memory]\nbackend = \"markdown\"\n";
        let (_dir, ws) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# Agent\n\nHello.\n"),
                ("memory/2026-02-15.md", "## A\n\nlog\n"),
                ("memory/2026-02-16.md", "## B\n\nlog\n"),
                (".alfignore", "memory/2026-02-15.md\n"),
            ],
        );

        let result = enumerate(&ws).unwrap();
        let paths: Vec<String> = result.files.iter().map(|f| f.path.clone()).collect();

        // config.toml is synthesized and unfilterable — always present.
        assert!(paths.contains(&"config.toml".to_string()));
        assert!(paths.contains(&"SOUL.md".to_string()));
        // The .alfignore'd memory file is gone; the other remains.
        assert!(!paths.contains(&"memory/2026-02-15.md".to_string()));
        assert!(paths.contains(&"memory/2026-02-16.md".to_string()));
        assert_eq!(result.excluded_by_alfignore, 1);
    }

    /// `enumerate` is the single source of truth: its file list equals the
    /// real export's `raw/zeroclaw/` entries.
    #[test]
    fn enumerate_matches_export_raw_sources() {
        let config = "[memory]\nbackend = \"markdown\"\n";
        let (dir, ws) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# Agent\n\nHello.\n"),
                ("memory/2026-02-15.md", "## A\n\nlog\n"),
            ],
        );

        let enumerated: std::collections::BTreeSet<String> =
            enumerate(&ws).unwrap().files.into_iter().map(|f| f.path).collect();

        let output = dir.path().join("out.alf");
        let report = export(&ws, &output).unwrap();
        let exported: std::collections::BTreeSet<String> =
            report.raw_sources.into_iter().collect();

        assert_eq!(enumerated, exported);
    }
}
