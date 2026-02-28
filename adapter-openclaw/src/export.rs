//! Export an OpenClaw workspace to an `.alf` archive.
//!
//! Orchestrates the parsers (memory, identity, principals, credentials),
//! groups memory records into time-based partitions, preserves raw source
//! files, and writes the archive using `AlfWriter`.

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

use crate::credential_map;
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

/// Read or generate the agent UUID.
///
/// If `{workspace}/.alf-agent-id` exists, read it. Otherwise generate a
/// deterministic UUID v5 from the canonical workspace path and persist it.
fn resolve_agent_id(workspace: &Path) -> Result<Uuid> {
    let id_file = workspace.join(".alf-agent-id");
    if id_file.is_file() {
        let raw = fs::read_to_string(&id_file)
            .context("Failed to read .alf-agent-id")?;
        let id = Uuid::parse_str(raw.trim())
            .context("Invalid UUID in .alf-agent-id")?;
        return Ok(id);
    }

    // Derive from canonical workspace path
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let id = Uuid::new_v5(&AGENT_ID_NS, canonical.to_string_lossy().as_bytes());

    // Persist for stability across future exports
    let _ = fs::write(&id_file, id.to_string());

    Ok(id)
}

// ---------------------------------------------------------------------------
// Agent name detection
// ---------------------------------------------------------------------------

/// Try to detect the agent's display name from workspace files.
fn detect_agent_name(workspace: &Path) -> String {
    // Try SOUL.md first
    if let Ok(content) = fs::read_to_string(workspace.join("SOUL.md")) {
        if let Some(name) = extract_h1(&content) {
            return name;
        }
    }
    // Then IDENTITY.md
    if let Ok(content) = fs::read_to_string(workspace.join("IDENTITY.md")) {
        if let Some(name) = extract_h1(&content) {
            return name;
        }
    }
    // Fall back to directory name
    workspace
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn extract_h1(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("# ") && !t.starts_with("## ") {
            return Some(t.trim_start_matches("# ").trim().to_string());
        }
    }
    None
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

/// Best-effort home directory.
fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
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

/// Collect all raw source files to preserve in the archive.
/// Returns `(workspace-relative path, file contents)` pairs.
fn collect_raw_sources(workspace: &Path) -> Vec<(String, Vec<u8>)> {
    let mut sources = Vec::new();

    // Root-level files
    for name in ROOT_FILES {
        let path = workspace.join(name);
        if path.is_file() {
            if let Ok(data) = fs::read(&path) {
                sources.push((name.to_string(), data));
            }
        }
    }

    // memory/ directory
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
// Export entry point
// ---------------------------------------------------------------------------

/// Export an OpenClaw workspace to an `.alf` archive.
pub fn export(workspace: &Path, output: &Path) -> Result<ExportReport> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    // 1. Agent ID
    let agent_id = resolve_agent_id(workspace)?;
    let agent_name = detect_agent_name(workspace);
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
            (quarter_start(year, quarter), Some(quarter_end(year, quarter)))
        } else {
            (
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                None,
            )
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

    // Try to find state dir for credentials
    let state_dir = dirs_home().map(|h| h.join(".openclaw"));
    let credentials = credential_map::build_credentials(
        state_dir.as_deref(),
        "main", // default agent ID string
        agent_id,
    )?;

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
                partitions: partition_infos.iter().map(|(info, _)| info.clone()).collect(),
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

    // Raw sources
    let raw_sources = collect_raw_sources(workspace);
    let raw_source_names: Vec<String> = raw_sources.iter().map(|(n, _)| n.clone()).collect();
    for (relative_path, data) in &raw_sources {
        alf_writer.add_raw_source("openclaw", relative_path, data)?;
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
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_workspace(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        dir
    }

    #[test]
    fn export_minimal_workspace() {
        let ws = create_workspace(&[
            ("SOUL.md", "# TestAgent\n\nA test agent."),
            ("MEMORY.md", "## Facts\n\nThe sky is blue."),
        ]);
        let output = ws.path().join("test.alf");

        let report = export(ws.path(), &output).unwrap();
        assert_eq!(report.agent_name, "TestAgent");
        assert_eq!(report.memory_records, 1);
        assert!(report.identity_version.is_some());
        assert!(output.is_file());
        assert!(report.output_size_bytes > 0);
    }

    #[test]
    fn export_with_daily_logs() {
        let ws = create_workspace(&[
            ("SOUL.md", "# Agent\n\nHello."),
            (
                "memory/2026-01-15.md",
                "## Morning\n\nDid stuff.\n\n## Afternoon\n\nMore stuff.",
            ),
            ("memory/2026-01-16.md", "## All day\n\nBusy day."),
        ]);
        let output = ws.path().join("test.alf");

        let report = export(ws.path(), &output).unwrap();
        // 2 sections from Jan 15 + 1 from Jan 16
        assert_eq!(report.memory_records, 3);
    }

    #[test]
    fn export_preserves_raw_sources() {
        let ws = create_workspace(&[
            ("SOUL.md", "# Bot\n\nSoul."),
            ("USER.md", "# Alice\n\nProfile."),
            ("TOOLS.md", "Tool notes."),
        ]);
        let output = ws.path().join("test.alf");

        let report = export(ws.path(), &output).unwrap();
        assert!(report.raw_sources.contains(&"SOUL.md".to_string()));
        assert!(report.raw_sources.contains(&"USER.md".to_string()));
        assert!(report.raw_sources.contains(&"TOOLS.md".to_string()));
    }

    #[test]
    fn agent_id_is_stable() {
        let ws = create_workspace(&[("SOUL.md", "# X\n\nTest.")]);
        let output1 = ws.path().join("test1.alf");
        let output2 = ws.path().join("test2.alf");

        export(ws.path(), &output1).unwrap();
        export(ws.path(), &output2).unwrap();

        // .alf-agent-id should have been written
        let id_file = ws.path().join(".alf-agent-id");
        assert!(id_file.is_file());
    }

    #[test]
    fn export_nonexistent_workspace() {
        let result = export(Path::new("/nonexistent/path"), Path::new("/tmp/out.alf"));
        assert!(result.is_err());
    }
}