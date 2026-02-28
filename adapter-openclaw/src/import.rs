//! Import an `.alf` archive into an OpenClaw workspace.
//!
//! Two paths:
//! 1. **Raw source restore** (preferred): if `raw/openclaw/` entries exist in
//!    the archive, extract them directly. This is the lossless path for
//!    OpenClaw-to-OpenClaw restores.
//! 2. **Cross-runtime migration**: reconstruct workspace files from ALF
//!    structured data (identity prose, principals prose, memory records).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use alf_core::AlfReader;

use crate::ImportReport;

// ---------------------------------------------------------------------------
// Import entry point
// ---------------------------------------------------------------------------

/// Import an `.alf` archive into an OpenClaw workspace.
///
/// Creates the workspace directory if it doesn't exist. Prefers raw source
/// files when available (lossless restore). Falls back to reconstructing
/// workspace files from structured ALF data (cross-runtime migration).
pub fn import(alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
    let file = std::fs::File::open(alf_file)
        .with_context(|| format!("Failed to open ALF file: {}", alf_file.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader)?;

    let manifest = alf.manifest().clone();
    let agent_name = manifest.agent.name.clone();
    let agent_id = manifest.agent.id;

    let mut warnings = Vec::new();

    // Ensure workspace directory exists
    fs::create_dir_all(workspace)?;
    fs::create_dir_all(workspace.join("memory"))?;

    // Check if raw/openclaw/ sources are available
    let file_names = alf.file_names();
    let raw_prefix = "raw/openclaw/";
    let has_raw = file_names.iter().any(|f| f.starts_with(raw_prefix));

    if has_raw {
        // Path 1: Raw source restore (lossless)
        restore_raw_sources(&mut alf, workspace, raw_prefix, &file_names)?;
    } else {
        // Path 2: Cross-runtime migration
        warnings.push(
            "No raw/openclaw/ sources in archive — reconstructing from structured data."
                .to_string(),
        );
        reconstruct_from_structured(&mut alf, workspace, &mut warnings)?;
    }

    // Write the agent ID for future exports
    let id_file = workspace.join(".alf-agent-id");
    fs::write(&id_file, agent_id.to_string())?;

    // Credentials warning
    let credentials_count = manifest
        .layers
        .credentials
        .as_ref()
        .map(|c| c.count)
        .unwrap_or(0);
    if credentials_count > 0 {
        warnings.push(format!(
            "{credentials_count} credential(s) found in archive (metadata only). \
             Re-authenticate in OpenClaw to restore access."
        ));
    }

    let identity_imported = manifest.layers.identity.is_some();
    let principals_count = manifest
        .layers
        .principals
        .as_ref()
        .map(|p| p.count)
        .unwrap_or(0);
    let memory_records = manifest
        .layers
        .memory
        .as_ref()
        .map(|m| m.record_count)
        .unwrap_or(0);

    Ok(ImportReport {
        agent_name,
        memory_records,
        identity_imported,
        principals_count,
        credentials_count,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Path 1: Raw source restore
// ---------------------------------------------------------------------------

/// Extract raw/openclaw/ files directly into the workspace.
fn restore_raw_sources<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    prefix: &str,
    file_names: &[String],
) -> Result<()> {
    for name in file_names {
        if !name.starts_with(prefix) {
            continue;
        }
        let relative = &name[prefix.len()..];
        if relative.is_empty() {
            continue;
        }

        let data = alf.read_raw_entry(name)?;
        let target = workspace.join(relative);

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

/// Reconstruct OpenClaw workspace files from structured ALF data.
fn reconstruct_from_structured<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    // Identity → SOUL.md
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
            // Synthesize a minimal SOUL.md from structured data
            let name = structured
                .names
                .as_ref()
                .map(|n| n.primary.as_str())
                .unwrap_or("Agent");
            let role = structured.role.as_deref().unwrap_or("AI Assistant");
            let soul = format!("# {name}\n\n{role}\n");
            fs::write(workspace.join("SOUL.md"), soul)?;
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
                // Synthesize minimal USER.md
                let name = structured.name.as_deref().unwrap_or("User");
                let mut content = format!("# {name}\n");
                if let Some(ref tz) = structured.timezone {
                    content.push_str(&format!("\n## Timezone\n\n{tz}\n"));
                }
                fs::write(workspace.join("USER.md"), content)?;
            }
        }
    }

    // Memory records → MEMORY.md + memory/YYYY-MM-DD.md
    let all_records = alf.read_all_memory()?;
    if all_records.is_empty() {
        return Ok(());
    }

    // Separate by namespace
    let mut curated_sections: Vec<String> = Vec::new();
    let mut daily_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut other_files: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for record in &all_records {
        let origin_file = record
            .source
            .origin_file
            .as_deref()
            .unwrap_or("");

        match record.namespace.as_str() {
            "curated" => {
                curated_sections.push(record.content.clone());
            }
            "daily" => {
                // Group by origin file or by observed_at date
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else if let Some(observed) = record.temporal.observed_at {
                    format!("memory/{}.md", observed.format("%Y-%m-%d"))
                } else {
                    format!("memory/{}.md", record.temporal.created_at.format("%Y-%m-%d"))
                };
                daily_groups
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
            _ => {
                // Use origin_file if available, otherwise namespace-based path
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else {
                    format!("memory/{}.md", record.namespace)
                };
                other_files
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
        }
    }

    // Write MEMORY.md
    if !curated_sections.is_empty() {
        let content = curated_sections.join("\n\n");
        fs::write(workspace.join("MEMORY.md"), content)?;
    }

    // Write daily log files
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

    if all_records.len() > 0 {
        warnings.push(format!(
            "Reconstructed {} memory record(s) from structured data.",
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
    fn round_trip_with_raw_sources() {
        // Create a workspace, export, then import into a fresh directory
        let ws = create_workspace(&[
            ("SOUL.md", "# Clawd\n\nA helpful lobster."),
            ("USER.md", "# Alice\n\n## Timezone\n\nAmerica/New_York\n"),
            ("MEMORY.md", "## Preferences\n\nLikes Rust."),
            (
                "memory/2026-01-15.md",
                "## Morning\n\nBuilt the adapter.",
            ),
        ]);
        let alf_file = ws.path().join("export.alf");

        // Export
        let report = export::export(ws.path(), &alf_file).unwrap();
        assert!(report.memory_records > 0);

        // Import into fresh workspace
        let target = TempDir::new().unwrap();
        let import_report = import(&alf_file, target.path()).unwrap();

        assert_eq!(import_report.agent_name, "Clawd");
        assert!(import_report.identity_imported);
        assert_eq!(import_report.principals_count, 1);
        assert_eq!(import_report.memory_records, report.memory_records);

        // Raw source files should be restored
        let soul = fs::read_to_string(target.path().join("SOUL.md")).unwrap();
        assert!(soul.contains("Clawd"));
        assert!(soul.contains("helpful lobster"));

        let user = fs::read_to_string(target.path().join("USER.md")).unwrap();
        assert!(user.contains("Alice"));

        let memory = fs::read_to_string(target.path().join("MEMORY.md")).unwrap();
        assert!(memory.contains("Likes Rust"));

        let daily = fs::read_to_string(target.path().join("memory/2026-01-15.md")).unwrap();
        assert!(daily.contains("adapter"));

        // Agent ID file should exist
        assert!(target.path().join(".alf-agent-id").is_file());
    }

    #[test]
    fn import_creates_workspace_dirs() {
        let ws = create_workspace(&[("SOUL.md", "# Bot\n\nTest.")]);
        let alf_file = ws.path().join("export.alf");
        export::export(ws.path(), &alf_file).unwrap();

        let target = TempDir::new().unwrap();
        let deep_path = target.path().join("deep/nested/workspace");
        let report = import(&alf_file, &deep_path).unwrap();
        assert_eq!(report.agent_name, "Bot");
        assert!(deep_path.is_dir());
    }
}