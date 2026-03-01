//! Export a ZeroClaw workspace to an `.alf` archive.
//!
//! Orchestrates: detect backend from `config.toml` → extract memory (SQLite
//! or Markdown) → build identity/principals/credentials → write archive.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::{
    AgentMetadata, AlfWriter, CredentialsLayerInfo, IdentityLayerInfo, LayerInventory, Manifest,
    MemoryInventory, MemoryPartitionInfo, PartitionAssigner, PrincipalsLayerInfo,
};

use crate::config_parser::{self, MemoryBackend, ZeroClawConfig};
use crate::credential_map;
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

/// Read or generate the agent UUID.
fn resolve_agent_id(workspace: &Path) -> Result<Uuid> {
    let id_file = workspace.join(".alf-agent-id");
    if id_file.is_file() {
        let raw = fs::read_to_string(&id_file)
            .context("Failed to read .alf-agent-id")?;
        let id = Uuid::parse_str(raw.trim())
            .context("Invalid UUID in .alf-agent-id")?;
        return Ok(id);
    }

    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let id = Uuid::new_v5(&AGENT_ID_NS, canonical.to_string_lossy().as_bytes());
    let _ = fs::write(&id_file, id.to_string());
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
    workspace
        .parent()
        .unwrap_or(workspace)
        .to_path_buf()
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

/// Collect all raw source files from a ZeroClaw workspace.
/// Returns `(relative_path, data)` pairs.
fn collect_raw_sources(workspace: &Path, _zc_home: &Path, config: &ZeroClawConfig) -> Vec<(String, Vec<u8>)> {
    let mut sources = Vec::new();

    // config.toml (redacted)
    let redacted = config_parser::redact_secrets(&config.raw_toml);
    sources.push(("config.toml".to_string(), redacted.into_bytes()));

    // Root-level workspace files
    for name in ROOT_FILES {
        let path = workspace.join(name);
        if path.is_file() {
            if let Ok(data) = fs::read(&path) {
                sources.push((name.to_string(), data));
            }
        }
    }

    // identity.json (AIEOS)
    if let Some(ref aieos_path) = config.aieos_path {
        let path = if Path::new(aieos_path).is_absolute() {
            Path::new(aieos_path).to_path_buf()
        } else {
            workspace.join(aieos_path)
        };
        if path.is_file() {
            if let Ok(data) = fs::read(&path) {
                sources.push(("identity.json".to_string(), data));
            }
        }
    }

    // memory/ directory (Markdown backend files)
    let memory_dir = workspace.join("memory");
    if memory_dir.is_dir() {
        for entry in WalkDir::new(&memory_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.path().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(workspace)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            if let Ok(data) = fs::read(entry.path()) {
                sources.push((relative, data));
            }
        }
    }

    sources
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
// Export entry point
// ---------------------------------------------------------------------------

/// Export a ZeroClaw workspace to an `.alf` archive.
pub fn export(workspace: &Path, output: &Path) -> Result<ExportReport> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    let zc_home = zeroclaw_home(workspace);

    // 1. Parse config
    let config_path = zc_home.join("config.toml");
    let config = config_parser::parse_config(&config_path)?
        .unwrap_or_else(|| {
            // No config.toml — use defaults with heuristic backend detection
            let backend = config_parser::detect_backend_heuristic(&zc_home);
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
        });

    // 2. Agent ID + name
    let agent_id = resolve_agent_id(workspace)?;
    let agent_name = identity_parser::detect_agent_name(workspace, &config);
    let runtime_version = detect_zeroclaw_version(&zc_home);

    // 3. Extract memory records (based on backend)
    let records = match config.memory_backend {
        MemoryBackend::Sqlite => {
            let db_path = zc_home.join("memory.db");
            if db_path.is_file() {
                sqlite_extractor::extract_from_sqlite(
                    &db_path,
                    &config,
                    agent_id,
                    runtime_version.as_deref(),
                )?
            } else {
                // SQLite configured but file missing — try markdown fallback
                markdown_parser::collect_markdown_memory(
                    workspace,
                    agent_id,
                    runtime_version.as_deref(),
                )?
            }
        }
        MemoryBackend::Markdown => {
            markdown_parser::collect_markdown_memory(
                workspace,
                agent_id,
                runtime_version.as_deref(),
            )?
        }
        MemoryBackend::None | MemoryBackend::Unsupported => Vec::new(),
    };
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
            (quarter_start(year, quarter), Some(quarter_end(year, quarter)))
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
    let credentials = credential_map::build_credentials(&config, agent_id)?;

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
                partitions: partition_infos.iter().map(|(info, _)| info.clone()).collect(),
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

    // Raw sources
    let raw_sources = collect_raw_sources(workspace, &zc_home, &config);
    let raw_source_names: Vec<String> = raw_sources.iter().map(|(n, _)| n.clone()).collect();
    for (relative_path, data) in &raw_sources {
        alf_writer.add_raw_source("zeroclaw", relative_path, data)?;
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
        ).unwrap();
        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "a1b2c3d4-0000-0000-0000-000000000001",
                "user_pref",
                "User prefers Rust over Go",
                "core",
                "2026-01-15T10:00:00Z",
            ],
        ).unwrap();
        conn.execute(
            "INSERT INTO memories VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "a1b2c3d4-0000-0000-0000-000000000002",
                "daily_log",
                "Reviewed migration plan",
                "daily",
                "2026-02-20T14:00:00Z",
            ],
        ).unwrap();
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
        let (dir, ws) = create_zeroclaw_home(config, &[
            ("SOUL.md", "# ZCAgent\n\nA test ZeroClaw agent.\n"),
        ]);
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
        let (dir, ws) = create_zeroclaw_home(config, &[
            ("SOUL.md", "# MdAgent\n\nMarkdown backend.\n"),
            ("memory/2026-02-15.md", "## Morning\n\nDid stuff.\n\n## Evening\n\nMore stuff.\n"),
        ]);

        let output = dir.path().join("test.alf");
        let report = export(&ws, &output).unwrap();

        assert_eq!(report.agent_name, "MdAgent");
        assert_eq!(report.memory_records, 2);
    }

    #[test]
    fn agent_id_stability() {
        let config = "[memory]\nbackend = \"sqlite\"";
        let (dir, ws) = create_zeroclaw_home(config, &[
            ("SOUL.md", "# Stable\n\nTest.\n"),
        ]);
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
}