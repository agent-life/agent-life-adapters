//! Parse ZeroClaw Markdown backend files into ALF `MemoryRecord` values.
//!
//! Handles daily files (`YYYY-MM-DD.md`), session files (`session_*.md`),
//! and archived files (`archive/`). Uses H2-heading splitting for record
//! boundaries and UUID v5 for deterministic IDs.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use regex::Regex;
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::{
    ExtractionMethod, MemoryRecord, MemoryStatus, MemoryType, SourceProvenance, TemporalMetadata,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// UUID v5 namespace for generating deterministic record IDs.
/// Distinct from OpenClaw's namespace to avoid ID collisions.
const ZEROCLAW_NS: Uuid = Uuid::from_bytes([
    0x7a, 0x65, 0x72, 0x6f, 0x63, 0x6c, 0x61, 0x77, // "zeroclaw"
    0x2d, 0x61, 0x6c, 0x66, 0x2d, 0x6e, 0x73, 0x31, // "-alf-ns1"
]);

const RUNTIME: &str = "zeroclaw";

// ---------------------------------------------------------------------------
// Section splitting (shared logic with OpenClaw adapter)
// ---------------------------------------------------------------------------

/// A section extracted from a Markdown file by splitting on H2 headings.
#[derive(Debug, Clone)]
struct MarkdownSection {
    heading: Option<String>,
    content: String,
    line_start: usize,
    line_end: usize,
}

/// Split Markdown content on `## ` (H2) headings.
fn split_markdown_sections(content: &str) -> Vec<MarkdownSection> {
    let lines: Vec<&str> = content.lines().collect();
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut section_start: usize = 1;

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        if line.starts_with("## ") {
            if !current_lines.is_empty() {
                let text = current_lines.join("\n");
                if !text.trim().is_empty() {
                    sections.push(MarkdownSection {
                        heading: current_heading.take(),
                        content: text,
                        line_start: section_start,
                        line_end: lineno - 1,
                    });
                }
            }
            current_heading = Some(line.trim_start_matches("## ").trim().to_string());
            current_lines = vec![line];
            section_start = lineno;
        } else {
            current_lines.push(line);
        }
    }

    // Flush last section
    if !current_lines.is_empty() {
        let text = current_lines.join("\n");
        if !text.trim().is_empty() {
            sections.push(MarkdownSection {
                heading: current_heading,
                content: text,
                line_start: section_start,
                line_end: lines.len(),
            });
        }
    }

    sections
}

// ---------------------------------------------------------------------------
// File classification
// ---------------------------------------------------------------------------

/// Classify a markdown file's purpose based on its path.
struct FileClassification {
    memory_type: MemoryType,
    namespace: String,
    status: MemoryStatus,
    observed_at: Option<DateTime<Utc>>,
    extraction_method: ExtractionMethod,
}

fn classify_file(relative_path: &str) -> FileClassification {
    let is_archived = relative_path.contains("archive/");

    let status = if is_archived {
        MemoryStatus::Archived
    } else {
        MemoryStatus::Active
    };

    // Strip "memory/" and "archive/" prefixes for name matching
    let filename = relative_path
        .trim_start_matches("memory/")
        .trim_start_matches("archive/");

    // Daily file: YYYY-MM-DD.md
    let daily_re = Regex::new(r"^(\d{4}-\d{2}-\d{2})\.md$").unwrap();
    if let Some(caps) = daily_re.captures(filename) {
        let date_str = &caps[1];
        let observed = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_time(NaiveTime::from_hms_opt(0, 0, 0)?).and_utc().into());

        return FileClassification {
            memory_type: MemoryType::Episodic,
            namespace: "daily".into(),
            status,
            observed_at: observed,
            extraction_method: ExtractionMethod::AgentWritten,
        };
    }

    // Session file: session_*.md
    if filename.starts_with("session_") {
        return FileClassification {
            memory_type: MemoryType::Episodic,
            namespace: "session".into(),
            status,
            observed_at: None,
            extraction_method: ExtractionMethod::AgentWritten,
        };
    }

    // Fallback: generic memory file
    FileClassification {
        memory_type: MemoryType::Semantic,
        namespace: "memory".into(),
        status,
        observed_at: None,
        extraction_method: ExtractionMethod::AgentWritten,
    }
}

// ---------------------------------------------------------------------------
// Record generation
// ---------------------------------------------------------------------------

/// Generate a deterministic UUID v5 for a record.
fn record_id(relative_path: &str, section_index: usize) -> Uuid {
    let name = format!("{relative_path}:{section_index}");
    Uuid::new_v5(&ZEROCLAW_NS, name.as_bytes())
}

/// Parse a single markdown file into memory records.
fn parse_memory_file(
    file_path: &Path,
    relative_path: &str,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<Vec<MemoryRecord>> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;

    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let classification = classify_file(relative_path);
    let sections = split_markdown_sections(&content);

    let file_mtime = fs::metadata(file_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| DateTime::<Utc>::from(t))
        .unwrap_or_else(Utc::now);

    let mut records = Vec::with_capacity(sections.len());

    for (idx, section) in sections.iter().enumerate() {
        let id = record_id(relative_path, idx);
        let created_at = classification.observed_at.unwrap_or(file_mtime);

        let raw_source = serde_json::json!({
            "line_start": section.line_start,
            "line_end": section.line_end,
            "heading": section.heading,
        });

        let mut tags = vec![classification.namespace.clone(), RUNTIME.to_string()];
        if classification.status == MemoryStatus::Archived {
            tags.push("archived".to_string());
        }

        records.push(MemoryRecord {
            id,
            agent_id,
            content: section.content.clone(),
            memory_type: classification.memory_type.clone(),
            source: SourceProvenance {
                runtime: RUNTIME.to_string(),
                runtime_version: runtime_version.map(|s| s.to_string()),
                origin: Some("workspace".to_string()),
                origin_file: Some(relative_path.to_string()),
                extraction_method: Some(classification.extraction_method.clone()),
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at,
                updated_at: None,
                observed_at: classification.observed_at,
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: classification.status.clone(),
            namespace: classification.namespace.clone(),
            category: Some(classification.namespace.clone()),
            supersedes: None,
            confidence: None,
            entities: Vec::new(),
            tags,
            embeddings: Vec::new(),
            related_records: Vec::new(),
            raw_source_format: Some(raw_source),
            extra: HashMap::new(),
        });
    }

    Ok(records)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Collect all memory records from the ZeroClaw markdown backend.
///
/// Walks `{workspace}/memory/` (including `archive/`) and parses all `.md`
/// files. Returns records sorted by `created_at`.
pub fn collect_markdown_memory(
    workspace: &Path,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<Vec<MemoryRecord>> {
    let memory_dir = workspace.join("memory");
    if !memory_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut all_records = Vec::new();

    for entry in WalkDir::new(&memory_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let relative = path
            .strip_prefix(workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        let records = parse_memory_file(path, &relative, agent_id, runtime_version)?;
        all_records.extend(records);
    }

    all_records.sort_by(|a, b| a.temporal.created_at.cmp(&b.temporal.created_at));

    Ok(all_records)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_workspace(dir: &Path) -> std::path::PathBuf {
        let ws = dir.join("workspace");
        fs::create_dir_all(ws.join("memory").join("archive")).unwrap();
        ws
    }

    #[test]
    fn daily_file_split_on_h2() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = setup_workspace(tmp.path());
        let agent_id = Uuid::new_v4();

        fs::write(
            ws.join("memory/2026-01-15.md"),
            "## Session — 10:30 AM\n\nReviewed the plan.\n\n## Session — 2:15 PM\n\nShipped v2.\n",
        ).unwrap();

        let records = collect_markdown_memory(&ws, agent_id, None).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].memory_type, MemoryType::Episodic);
        assert_eq!(records[0].namespace, "daily");
        assert!(records[0].content.contains("10:30 AM"));
        assert!(records[1].content.contains("2:15 PM"));
    }

    #[test]
    fn session_file_classified() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = setup_workspace(tmp.path());
        let agent_id = Uuid::new_v4();

        fs::write(
            ws.join("memory/session_abc123.md"),
            "## Turn 1\n\nUser asked about weather.\n",
        ).unwrap();

        let records = collect_markdown_memory(&ws, agent_id, None).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].namespace, "session");
        assert_eq!(
            records[0].source.extraction_method,
            Some(ExtractionMethod::AgentWritten)
        );
    }

    #[test]
    fn archived_file_gets_archived_status() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = setup_workspace(tmp.path());
        let agent_id = Uuid::new_v4();

        fs::write(
            ws.join("memory/archive/2026-01-08.md"),
            "## Old session\n\nArchived content.\n",
        ).unwrap();

        let records = collect_markdown_memory(&ws, agent_id, None).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, MemoryStatus::Archived);
        assert!(records[0].tags.contains(&"archived".to_string()));
    }

    #[test]
    fn deterministic_ids() {
        let id1 = record_id("memory/2026-01-15.md", 0);
        let id2 = record_id("memory/2026-01-15.md", 0);
        let id3 = record_id("memory/2026-01-15.md", 1);
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn daily_file_observed_at() {
        let classification = classify_file("memory/2026-02-20.md");
        assert!(classification.observed_at.is_some());
        let obs = classification.observed_at.unwrap();
        assert_eq!(obs.format("%Y-%m-%d").to_string(), "2026-02-20");
    }

    #[test]
    fn empty_memory_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = setup_workspace(tmp.path());
        let records = collect_markdown_memory(&ws, Uuid::new_v4(), None).unwrap();
        assert!(records.is_empty());
    }
}