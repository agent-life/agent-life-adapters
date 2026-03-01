//! Import an `.alf` archive into a ZeroClaw workspace.
//!
//! Two paths:
//! 1. **Raw source restore** (preferred): if `raw/zeroclaw/` entries exist,
//!    extract them directly. This is the lossless path for ZeroClaw-to-ZeroClaw
//!    restores.
//! 2. **Cross-runtime migration**: reconstruct workspace files from ALF
//!    structured data (identity, principals, memory records). Writes Markdown
//!    files for the memory layer — does NOT populate SQLite (the user must run
//!    `zeroclaw` to ingest the Markdown files).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use alf_core::AlfReader;

use crate::ImportReport;

// ---------------------------------------------------------------------------
// Import entry point
// ---------------------------------------------------------------------------

/// Import an `.alf` archive into a ZeroClaw workspace.
///
/// Creates workspace directories if they don't exist. Prefers raw source files
/// when available. Falls back to reconstructing workspace from ALF data.
pub fn import(alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader)?;

    let manifest = alf.manifest().clone();
    let agent_name = manifest.agent.name.clone();
    let agent_id = manifest.agent.id;

    let mut warnings = Vec::new();

    // Ensure workspace directory structure
    fs::create_dir_all(workspace)?;
    fs::create_dir_all(workspace.join("memory"))?;

    // Check for raw/zeroclaw/ sources
    let file_names = alf.file_names();
    let raw_prefix = "raw/zeroclaw/";
    let has_raw = file_names.iter().any(|f| f.starts_with(raw_prefix));

    if has_raw {
        restore_raw_sources(&mut alf, workspace, raw_prefix, &file_names)?;
    } else {
        warnings.push(
            "No raw/zeroclaw/ sources in archive — reconstructing from structured data."
                .to_string(),
        );
        reconstruct_from_structured(&mut alf, workspace, &mut warnings)?;
    }

    // Write agent ID file
    let id_file = workspace.join(".alf-agent-id");
    let _ = fs::write(&id_file, agent_id.to_string());

    // Count what we imported
    let identity = alf.read_identity()?;
    let principals = alf.read_principals()?;
    let credentials = alf.read_credentials()?;
    let all_memory = alf.read_all_memory()?;

    Ok(ImportReport {
        agent_name,
        memory_records: all_memory.len() as u64,
        identity_imported: identity.is_some(),
        principals_count: principals
            .as_ref()
            .map(|p| p.principals.len() as u32)
            .unwrap_or(0),
        credentials_count: credentials
            .as_ref()
            .map(|c| c.credentials.len() as u32)
            .unwrap_or(0),
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Path 1: Raw source restore (lossless)
// ---------------------------------------------------------------------------

fn restore_raw_sources<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    prefix: &str,
    file_names: &[String],
) -> Result<()> {
    // Determine the ZeroClaw home (parent of workspace)
    let zc_home = workspace.parent().unwrap_or(workspace);

    for name in file_names {
        if !name.starts_with(prefix) {
            continue;
        }
        let relative = &name[prefix.len()..];
        if relative.is_empty() {
            continue;
        }

        let data = alf.read_raw_entry(name)?;

        // Decide where to place the file:
        // - config.toml → zc_home/config.toml
        // - identity.json → workspace/identity.json
        // - memory/* → workspace/memory/*
        // - Everything else → workspace/{relative}
        let target = if relative == "config.toml" {
            zc_home.join(relative)
        } else {
            workspace.join(relative)
        };

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, &data)
            .with_context(|| format!("Failed to write {}", target.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Path 2: Cross-runtime reconstruction
// ---------------------------------------------------------------------------

fn reconstruct_from_structured<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    // Identity → SOUL.md (+ IDENTITY.md, AGENTS.md)
    if let Some(identity) = alf.read_identity()? {
        if let Some(ref prose) = identity.prose {
            if let Some(ref soul) = prose.soul {
                fs::write(workspace.join("SOUL.md"), soul)?;
            }
            if let Some(ref profile) = prose.identity_profile {
                fs::write(workspace.join("IDENTITY.md"), profile)?;
            }
            if let Some(ref instructions) = prose.operating_instructions {
                fs::write(workspace.join("AGENTS.md"), instructions)?;
            }
        } else if let Some(ref structured) = identity.structured {
            // Synthesize a minimal SOUL.md
            let name = structured
                .names
                .as_ref()
                .map(|n| n.primary.as_str())
                .unwrap_or("Agent");
            let role = structured.role.as_deref().unwrap_or("AI Assistant");
            let soul = format!("# {name}\n\n{role}\n");
            fs::write(workspace.join("SOUL.md"), soul)?;
        }

        // If AIEOS raw source is present, write identity.json
        if identity.source_format.as_deref() == Some("aieos") {
            if let Some(ref raw) = identity.raw_source {
                let json = serde_json::to_string_pretty(raw)?;
                fs::write(workspace.join("identity.json"), json)?;
            }
        }
    }

    // Principals → USER.md
    if let Some(principals) = alf.read_principals()? {
        if let Some(principal) = principals.principals.first() {
            if let Some(ref prose) = principal.profile.prose {
                if let Some(ref user_profile) = prose.user_profile {
                    fs::write(workspace.join("USER.md"), user_profile)?;
                }
            } else if let Some(ref structured) = principal.profile.structured {
                let name = structured.name.as_deref().unwrap_or("User");
                let mut content = format!("# {name}\n");
                if let Some(ref tz) = structured.timezone {
                    content.push_str(&format!("\n## Timezone\n\n{tz}\n"));
                }
                fs::write(workspace.join("USER.md"), content)?;
            }
        }
    }

    // Memory records → memory/YYYY-MM-DD.md files
    let all_records = alf.read_all_memory()?;
    if all_records.is_empty() {
        return Ok(());
    }

    // Group by namespace/origin for reconstruction
    let mut core_sections: Vec<String> = Vec::new();
    let mut daily_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut other_files: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for record in &all_records {
        let origin_file = record.source.origin_file.as_deref().unwrap_or("");

        match record.namespace.as_str() {
            "core" => {
                core_sections.push(record.content.clone());
            }
            "daily" => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else if let Some(observed) = record.temporal.observed_at {
                    format!("memory/{}.md", observed.format("%Y-%m-%d"))
                } else {
                    format!("memory/{}.md", record.temporal.created_at.format("%Y-%m-%d"))
                };
                daily_groups.entry(key).or_default().push(record.content.clone());
            }
            "conversation" => {
                // Conversation records grouped by date
                let key = format!(
                    "memory/{}.md",
                    record.temporal.created_at.format("%Y-%m-%d")
                );
                daily_groups.entry(key).or_default().push(record.content.clone());
            }
            "session" => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else {
                    let short_id = &record.id.to_string()[..8];
                    format!("memory/session_{short_id}.md")
                };
                other_files.entry(key).or_default().push(record.content.clone());
            }
            _ => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else {
                    format!("memory/{}.md", record.namespace)
                };
                other_files.entry(key).or_default().push(record.content.clone());
            }
        }
    }

    // Write core sections as a single file (ZeroClaw's memory_store Core)
    if !core_sections.is_empty() {
        let content = core_sections.join("\n\n");
        let target = workspace.join("memory").join("core.md");
        fs::write(&target, content)?;
    }

    // Write daily files
    for (file_path, sections) in &daily_groups {
        let content = sections.join("\n\n");
        let target = workspace.join(file_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, content)?;
    }

    // Write other memory files
    for (file_path, sections) in &other_files {
        let content = sections.join("\n\n");
        let target = workspace.join(file_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, content)?;
    }

    if !all_records.is_empty() {
        warnings.push(format!(
            "Reconstructed {} memory record(s) as Markdown files. \
             Run `zeroclaw` to ingest into SQLite if desired.",
            all_records.len()
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

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
                "00000000-0000-0000-0000-000000000001",
                "pref_lang",
                "## Language Preference\n\nUser prefers Rust.",
                "core",
                "2026-01-15T10:00:00Z",
            ],
        ).unwrap();
    }

    #[test]
    fn round_trip_sqlite_workspace() {
        let config = "[memory]\nbackend = \"sqlite\"\nembedding_provider = \"none\"";
        let (dir, ws) = create_zeroclaw_home(config, &[
            ("SOUL.md", "# ZCBot\n\nA ZeroClaw assistant.\n"),
            ("USER.md", "# Alice\n\n## Timezone\n\nAmerica/New_York\n"),
        ]);
        create_test_db(dir.path());

        // Export
        let alf_file = dir.path().join("export.alf");
        let export_report = export::export(&ws, &alf_file).unwrap();
        assert!(export_report.memory_records > 0);

        // Import into fresh workspace
        let target_dir = TempDir::new().unwrap();
        let target_ws = target_dir.path().join("workspace");
        fs::create_dir_all(&target_ws).unwrap();

        let import_report = import(&alf_file, &target_ws).unwrap();
        assert_eq!(import_report.agent_name, "ZCBot");
        assert!(import_report.identity_imported);
        assert_eq!(import_report.principals_count, 1);

        // Raw sources should be restored
        let soul = fs::read_to_string(target_ws.join("SOUL.md")).unwrap();
        assert!(soul.contains("ZCBot"));

        // config.toml goes to parent (zc_home level)
        // Note: in our test the target_ws has no parent with special meaning,
        // so config.toml ends up at target_ws's parent
        let config_path = target_dir.path().join("config.toml");
        assert!(config_path.is_file());
    }

    #[test]
    fn import_creates_workspace_dirs() {
        let config = "[memory]\nbackend = \"markdown\"";
        let (dir, ws) = create_zeroclaw_home(config, &[
            ("SOUL.md", "# DirTest\n\nTest.\n"),
            ("memory/2026-01-15.md", "## Entry\n\nContent.\n"),
        ]);

        let alf_file = dir.path().join("export.alf");
        export::export(&ws, &alf_file).unwrap();

        let target = TempDir::new().unwrap();
        let deep = target.path().join("deep/nested/workspace");
        let report = import(&alf_file, &deep).unwrap();
        assert_eq!(report.agent_name, "DirTest");
        assert!(deep.is_dir());
    }
}