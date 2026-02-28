//! Parse OpenClaw workspace Markdown files into ALF `MemoryRecord` values.
//!
//! This is the heart of the adapter. OpenClaw stores memory as plain Markdown
//! files — the adapter must define record boundaries, classify types, and
//! generate stable IDs so that delta computation works across exports.

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
/// Generated once, never changes. Ensures the same workspace file + section
/// always produces the same record UUID.
const OPENCLAW_NS: Uuid = Uuid::from_bytes([
    0x6f, 0x70, 0x65, 0x6e, 0x63, 0x6c, 0x61, 0x77, // "openclaw"
    0x2d, 0x61, 0x6c, 0x66, 0x2d, 0x6e, 0x73, 0x31, // "-alf-ns1"
]);

const RUNTIME: &str = "openclaw";

// ---------------------------------------------------------------------------
// Section splitting
// ---------------------------------------------------------------------------

/// A section extracted from a Markdown file by splitting on H2 headings.
#[derive(Debug, Clone)]
pub(crate) struct MarkdownSection {
    /// H2 heading text (without the `## ` prefix), or `None` for content
    /// before the first heading.
    pub heading: Option<String>,
    /// Full section text including the heading line.
    pub content: String,
    /// 1-based start line in the original file.
    pub line_start: usize,
    /// 1-based end line (inclusive).
    pub line_end: usize,
}

/// Split Markdown content on `## ` (H2) headings.
///
/// Content before the first H2 becomes section 0 with `heading = None` (if
/// non-empty after trimming). Each H2 and its body becomes one section.
pub(crate) fn split_markdown_sections(content: &str) -> Vec<MarkdownSection> {
    let lines: Vec<&str> = content.lines().collect();
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut section_start: usize = 1; // 1-based

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1; // 1-based
        if line.starts_with("## ") {
            // Flush previous section
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
// Classification helpers
// ---------------------------------------------------------------------------

/// Generate a deterministic record ID from file path and section index.
fn record_id(relative_path: &str, section_index: usize) -> Uuid {
    let name = format!("{relative_path}:{section_index}");
    Uuid::new_v5(&OPENCLAW_NS, name.as_bytes())
}

/// Classify a workspace-relative file path into a `MemoryType`.
fn classify_memory_type(relative_path: &str) -> MemoryType {
    let lower = relative_path.to_lowercase();
    if is_daily_log(&lower) {
        MemoryType::Episodic
    } else if lower == "memory.md" {
        MemoryType::Semantic
    } else if lower.contains("active-context") {
        MemoryType::Summary
    } else if lower.contains("gating-policies") || lower.contains("gating_policies") {
        MemoryType::Procedural
    } else if lower.starts_with("memory/project-") {
        MemoryType::Semantic
    } else {
        MemoryType::Semantic
    }
}

/// Classify a workspace-relative file path into a namespace string.
fn classify_namespace(relative_path: &str) -> String {
    let lower = relative_path.to_lowercase();
    if is_daily_log(&lower) {
        "daily".to_string()
    } else if lower == "memory.md" {
        "curated".to_string()
    } else if lower.contains("active-context") {
        "active-context".to_string()
    } else if lower.starts_with("memory/project-") {
        "project".to_string()
    } else if lower.contains("gating-policies") || lower.contains("gating_policies") {
        "procedural".to_string()
    } else {
        "workspace".to_string()
    }
}

/// Determine the extraction method based on file path.
fn classify_extraction_method(relative_path: &str) -> ExtractionMethod {
    let lower = relative_path.to_lowercase();
    // MEMORY.md and gating-policies are typically user-curated
    if lower == "memory.md" || lower.contains("gating-policies") {
        ExtractionMethod::UserAuthored
    } else {
        ExtractionMethod::AgentWritten
    }
}

/// Check if a lowercased relative path matches the daily log pattern.
fn is_daily_log(lower_path: &str) -> bool {
    lower_path.starts_with("memory/") && parse_daily_date_inner(lower_path).is_some()
}

/// Try to parse a date from a daily log filename.
/// Accepts `memory/YYYY-MM-DD.md` (case-insensitive on the path).
pub(crate) fn parse_daily_date(relative_path: &str) -> Option<NaiveDate> {
    parse_daily_date_inner(&relative_path.to_lowercase())
}

fn parse_daily_date_inner(lower_path: &str) -> Option<NaiveDate> {
    // Extract the filename stem
    let filename = lower_path.strip_prefix("memory/")?;
    let stem = filename.strip_suffix(".md")?;
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

// ---------------------------------------------------------------------------
// Tag and importance extraction
// ---------------------------------------------------------------------------

/// Extract an importance tag from a line matching `[tag|i=N.N]`.
/// Returns `(tag_name, importance_score)`.
pub(crate) fn extract_importance_tag(line: &str) -> Option<(String, f64)> {
    // Pattern: [word|i=float]
    // Lazy-init the regex (runs once).
    lazy_static_regex(line)
}

fn lazy_static_regex(line: &str) -> Option<(String, f64)> {
    let re = Regex::new(r"\[(\w+)\|i=([\d.]+)\]").ok()?;
    let caps = re.captures(line)?;
    let tag = caps.get(1)?.as_str().to_string();
    let score: f64 = caps.get(2)?.as_str().parse().ok()?;
    Some((tag, score))
}

/// Scan section content for importance tags, #hashtags, and the file category tag.
fn extract_tags_and_confidence(
    content: &str,
    file_category: &str,
) -> (Vec<String>, Option<f64>, Option<String>) {
    let mut tags = vec![file_category.to_string()];
    let mut confidence: Option<f64> = None;
    let mut category: Option<String> = None;

    for line in content.lines() {
        // Importance tags
        if let Some((tag_name, score)) = extract_importance_tag(line) {
            if category.is_none() {
                category = Some(tag_name.clone());
            }
            if confidence.is_none() || score > confidence.unwrap_or(0.0) {
                confidence = Some(score);
            }
            if !tags.contains(&tag_name) {
                tags.push(tag_name);
            }
        }

        // #hashtags
        for word in line.split_whitespace() {
            if word.starts_with('#') && word.len() > 1 {
                let hashtag = word
                    .trim_start_matches('#')
                    .trim_end_matches(|c: char| !c.is_alphanumeric())
                    .to_string();
                if !hashtag.is_empty() && !tags.contains(&hashtag) {
                    tags.push(hashtag);
                }
            }
        }
    }

    (tags, confidence, category)
}

// ---------------------------------------------------------------------------
// File → MemoryRecord conversion
// ---------------------------------------------------------------------------

/// Parse a single memory file into `MemoryRecord` values.
///
/// `relative_path`: workspace-relative (e.g., `"memory/2026-01-15.md"`)
/// `content`: file contents
/// `file_mtime`: last modification time of the file
/// `agent_id`: the agent's UUID
pub(crate) fn parse_memory_file(
    relative_path: &str,
    content: &str,
    file_mtime: DateTime<Utc>,
    agent_id: Uuid,
) -> Vec<MemoryRecord> {
    let memory_type = classify_memory_type(relative_path);
    let namespace = classify_namespace(relative_path);
    let extraction_method = classify_extraction_method(relative_path);
    let daily_date = parse_daily_date(relative_path);

    // Determine splitting strategy
    let is_whole_file = relative_path.to_lowercase().contains("active-context");
    let sections = if is_whole_file {
        // Entire file as one section
        if content.trim().is_empty() {
            return Vec::new();
        }
        let lines: Vec<&str> = content.lines().collect();
        vec![MarkdownSection {
            heading: None,
            content: content.to_string(),
            line_start: 1,
            line_end: lines.len().max(1),
        }]
    } else {
        split_markdown_sections(content)
    };

    let file_category = match namespace.as_str() {
        "daily" => "daily",
        "curated" => "curated",
        "active-context" => "active-context",
        "project" => "project",
        "procedural" => "procedural",
        _ => "workspace",
    };

    sections
        .into_iter()
        .enumerate()
        .map(|(idx, section)| {
            let id = record_id(relative_path, idx);

            // Determine created_at: for daily logs use midnight of filename date,
            // otherwise fall back to file mtime.
            let created_at = if let Some(date) = daily_date {
                date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                    .and_utc()
            } else {
                file_mtime
            };

            // observed_at: only for daily logs
            let observed_at = daily_date.map(|date| {
                date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                    .and_utc()
            });

            let (tags, confidence, category) =
                extract_tags_and_confidence(&section.content, file_category);

            // Build raw_source_format metadata for precise re-import
            let raw_source_format = serde_json::json!({
                "line_start": section.line_start,
                "line_end": section.line_end,
                "heading": section.heading,
            });

            MemoryRecord {
                id,
                agent_id,
                content: section.content,
                memory_type: memory_type.clone(),
                source: SourceProvenance {
                    runtime: RUNTIME.to_string(),
                    runtime_version: None,
                    origin: Some("workspace".to_string()),
                    origin_file: Some(relative_path.to_string()),
                    extraction_method: Some(extraction_method.clone()),
                    session_id: None,
                    interaction_id: None,
                    identity_version: None,
                    extra: HashMap::new(),
                },
                temporal: TemporalMetadata {
                    created_at,
                    updated_at: Some(file_mtime),
                    observed_at,
                    valid_from: None,
                    valid_until: None,
                    last_accessed_at: None,
                    access_count: None,
                    extra: HashMap::new(),
                },
                status: MemoryStatus::Active,
                namespace: namespace.clone(),
                category,
                supersedes: None,
                confidence,
                entities: Vec::new(),
                tags,
                embeddings: Vec::new(),
                related_records: Vec::new(),
                raw_source_format: Some(raw_source_format),
                extra: HashMap::new(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Workspace walker
// ---------------------------------------------------------------------------

/// Walk the workspace and collect all memory records, sorted by `created_at`.
///
/// Reads `MEMORY.md` (at workspace root) and all `*.md` files under `memory/`.
pub fn collect_all_memory(workspace: &Path, agent_id: Uuid) -> Result<Vec<MemoryRecord>> {
    let mut records = Vec::new();

    // 1. MEMORY.md at workspace root
    let memory_md = workspace.join("MEMORY.md");
    if memory_md.is_file() {
        let content =
            fs::read_to_string(&memory_md).context("Failed to read MEMORY.md")?;
        let mtime = file_mtime(&memory_md);
        records.extend(parse_memory_file("MEMORY.md", &content, mtime, agent_id));
    }

    // 2. memory/ directory
    let memory_dir = workspace.join("memory");
    if memory_dir.is_dir() {
        for entry in WalkDir::new(&memory_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "md" {
                continue;
            }
            let relative = path
                .strip_prefix(workspace)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/"); // normalize Windows paths

            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read {relative}"))?;
            let mtime = file_mtime(path);
            records.extend(parse_memory_file(&relative, &content, mtime, agent_id));
        }
    }

    // Sort by created_at ascending
    records.sort_by_key(|r| r.temporal.created_at);

    Ok(records)
}

/// Get the last-modified time of a file, falling back to now on error.
fn file_mtime(path: &Path) -> DateTime<Utc> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sections_multiple_h2() {
        let md = "\
# Title

Intro text.

## First Section

Content one.

## Second Section

Content two.
";
        let sections = split_markdown_sections(md);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].heading, None);
        assert!(sections[0].content.contains("Title"));
        assert!(sections[0].content.contains("Intro text."));
        assert_eq!(sections[0].line_start, 1);

        assert_eq!(sections[1].heading, Some("First Section".to_string()));
        assert!(sections[1].content.contains("Content one."));

        assert_eq!(sections[2].heading, Some("Second Section".to_string()));
        assert!(sections[2].content.contains("Content two."));
    }

    #[test]
    fn split_sections_no_headings() {
        let md = "Just some text\nwith multiple lines.";
        let sections = split_markdown_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, None);
        assert_eq!(sections[0].line_start, 1);
        assert_eq!(sections[0].line_end, 2);
    }

    #[test]
    fn split_sections_empty_file() {
        let sections = split_markdown_sections("");
        assert!(sections.is_empty());
    }

    #[test]
    fn split_sections_h3_not_boundary() {
        let md = "\
## Section A

### Subsection

Text.
";
        let sections = split_markdown_sections(md);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].content.contains("### Subsection"));
    }

    #[test]
    fn split_sections_only_whitespace_before_first_h2() {
        let md = "\n\n\n## Real Section\n\nContent.";
        let sections = split_markdown_sections(md);
        // Whitespace-only preamble should be dropped
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, Some("Real Section".to_string()));
    }

    #[test]
    fn record_id_is_deterministic() {
        let id1 = record_id("memory/2026-01-15.md", 0);
        let id2 = record_id("memory/2026-01-15.md", 0);
        assert_eq!(id1, id2);
    }

    #[test]
    fn record_id_differs_for_different_inputs() {
        let id1 = record_id("memory/2026-01-15.md", 0);
        let id2 = record_id("memory/2026-01-15.md", 1);
        let id3 = record_id("memory/2026-01-16.md", 0);
        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn classify_daily_log() {
        assert_eq!(
            classify_memory_type("memory/2026-01-15.md"),
            MemoryType::Episodic
        );
        assert_eq!(classify_namespace("memory/2026-01-15.md"), "daily");
    }

    #[test]
    fn classify_curated() {
        assert_eq!(classify_memory_type("MEMORY.md"), MemoryType::Semantic);
        assert_eq!(classify_namespace("MEMORY.md"), "curated");
    }

    #[test]
    fn classify_active_context() {
        assert_eq!(
            classify_memory_type("memory/active-context.md"),
            MemoryType::Summary
        );
        assert_eq!(classify_namespace("memory/active-context.md"), "active-context");
    }

    #[test]
    fn classify_gating_policies() {
        assert_eq!(
            classify_memory_type("memory/gating-policies.md"),
            MemoryType::Procedural
        );
        assert_eq!(classify_namespace("memory/gating-policies.md"), "procedural");
    }

    #[test]
    fn classify_project_memory() {
        assert_eq!(
            classify_memory_type("memory/project-clawsmith.md"),
            MemoryType::Semantic
        );
        assert_eq!(classify_namespace("memory/project-clawsmith.md"), "project");
    }

    #[test]
    fn parse_daily_date_valid() {
        assert_eq!(
            parse_daily_date("memory/2026-01-15.md"),
            Some(NaiveDate::from_ymd_opt(2026, 1, 15).unwrap())
        );
    }

    #[test]
    fn parse_daily_date_invalid() {
        assert_eq!(parse_daily_date("memory/active-context.md"), None);
        assert_eq!(parse_daily_date("MEMORY.md"), None);
        assert_eq!(parse_daily_date("memory/project-foo.md"), None);
    }

    #[test]
    fn extract_importance_tag_valid() {
        let result = extract_importance_tag("- [decision|i=0.9] Switched to SQLite");
        assert_eq!(result, Some(("decision".to_string(), 0.9)));
    }

    #[test]
    fn extract_importance_tag_missing() {
        assert_eq!(extract_importance_tag("Just a normal line"), None);
    }

    #[test]
    fn extract_importance_tag_milestone() {
        let result = extract_importance_tag("[milestone|i=0.85] Shipped v2.0");
        assert_eq!(result, Some(("milestone".to_string(), 0.85)));
    }

    #[test]
    fn parse_memory_file_daily_log() {
        let content = "\
## Session — 10:30 AM

Reviewed the migration plan.

## Session — 2:15 PM

Shipped v2.0 of the memory architecture.
";
        let agent_id = Uuid::nil();
        let mtime = Utc::now();
        let records = parse_memory_file("memory/2026-01-15.md", content, mtime, agent_id);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].memory_type, MemoryType::Episodic);
        assert_eq!(records[0].namespace, "daily");
        assert!(records[0].content.contains("migration plan"));
        assert_eq!(
            records[0].source.origin_file.as_deref(),
            Some("memory/2026-01-15.md")
        );
        assert_eq!(
            records[0].source.extraction_method,
            Some(ExtractionMethod::AgentWritten)
        );
        // observed_at should be the date from filename
        let expected_date = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        assert_eq!(
            records[0].temporal.observed_at,
            Some(
                expected_date
                    .and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                    .and_utc()
            )
        );
    }

    #[test]
    fn parse_memory_file_curated() {
        let content = "\
## Conventions

Use SQLite for structured facts.

## Architecture

Modular with clear boundaries.
";
        let agent_id = Uuid::nil();
        let mtime = Utc::now();
        let records = parse_memory_file("MEMORY.md", content, mtime, agent_id);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].memory_type, MemoryType::Semantic);
        assert_eq!(records[0].namespace, "curated");
        assert_eq!(
            records[0].source.extraction_method,
            Some(ExtractionMethod::UserAuthored)
        );
    }

    #[test]
    fn parse_memory_file_with_importance_tags() {
        let content = "\
## Today

- [decision|i=0.9] Switched from PostgreSQL to SQLite
- [context|i=0.3] Ran routine maintenance #ops
";
        let agent_id = Uuid::nil();
        let mtime = Utc::now();
        let records = parse_memory_file("memory/2026-02-10.md", content, mtime, agent_id);

        assert_eq!(records.len(), 1);
        // Highest confidence from the section
        assert_eq!(records[0].confidence, Some(0.9));
        assert_eq!(records[0].category, Some("decision".to_string()));
        assert!(records[0].tags.contains(&"daily".to_string()));
        assert!(records[0].tags.contains(&"decision".to_string()));
        assert!(records[0].tags.contains(&"ops".to_string()));
    }

    #[test]
    fn parse_active_context_is_single_record() {
        let content = "\
# Current Focus

Working on the adapter implementation.

## Next Steps

Build the memory parser.
";
        let agent_id = Uuid::nil();
        let mtime = Utc::now();
        let records = parse_memory_file("memory/active-context.md", content, mtime, agent_id);

        // Should be ONE record despite having a ## heading
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].memory_type, MemoryType::Summary);
        assert!(records[0].content.contains("Current Focus"));
        assert!(records[0].content.contains("Next Steps"));
    }

    #[test]
    fn parse_empty_file_produces_no_records() {
        let records = parse_memory_file("MEMORY.md", "", Utc::now(), Uuid::nil());
        assert!(records.is_empty());
    }

    #[test]
    fn record_ids_stable_across_calls() {
        let content = "## Section A\n\nContent.";
        let agent_id = Uuid::nil();
        let mtime = Utc::now();
        let r1 = parse_memory_file("MEMORY.md", content, mtime, agent_id);
        let r2 = parse_memory_file("MEMORY.md", content, mtime, agent_id);
        assert_eq!(r1[0].id, r2[0].id);
    }
}