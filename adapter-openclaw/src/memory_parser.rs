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
// Source-handler table (WP2)
// ---------------------------------------------------------------------------
//
// Translating a runtime's on-disk shape into ALF is a *per-file* decision. Rather
// than run one structure-blind heuristic over every Markdown file, each known
// location declares how it maps: which `memory_type`/`namespace` to tag and how to
// chunk it. Files that match no specific row fall to the `memory/*.md` catch-all.
// This table is the pattern future adapters (ZeroClaw, …) copy and re-fill.

/// How a matched source file is turned into records.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ChunkingStrategy {
    /// The whole file is one record (procedures, curated knowledge, active-context).
    OneRecordPerFile,
    /// Split on Markdown ATX headings of `level` (2 == `## `). When `fence_aware`,
    /// headings inside ``` fenced code blocks are ignored.
    SplitByHeading { fence_aware: bool, level: u8 },
    /// Layer 4 — iterate vault entries. Owned by the vault path (WP1); never
    /// dispatched from the memory parser. Present so the strategy vocabulary
    /// matches the architectural framing for future adapters.
    #[allow(dead_code)]
    VaultEntries,
    /// Layer 5 — path listing only, no parse. Owned by `enumerate` (WP3); never
    /// dispatched from the memory parser.
    #[allow(dead_code)]
    FileListingOnly,
}

/// Declarative description of how one on-disk location maps to ALF memory.
pub(crate) struct SourceHandler {
    /// Glob relative to the workspace root. See `path_matches` for the supported forms.
    pub pattern: &'static str,
    /// Cognitive category to tag records with.
    pub memory_type: MemoryType,
    /// Scoping namespace to tag records with.
    pub namespace: &'static str,
    pub chunking: ChunkingStrategy,
}

/// Ordered source-handler table. First match wins, so specific patterns come
/// before the catch-all. Every memory file flows through exactly one row.
static SOURCE_HANDLERS: &[SourceHandler] = &[
    // Root curated knowledge file. Split by heading so each topic is its own record.
    SourceHandler {
        pattern: "MEMORY.md",
        memory_type: MemoryType::Semantic,
        namespace: "curated",
        chunking: ChunkingStrategy::SplitByHeading {
            fence_aware: true,
            level: 2,
        },
    },
    // Daily journals: memory/YYYY-MM-DD.md → one episodic record per `## ` entry.
    SourceHandler {
        pattern: "memory/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].md",
        memory_type: MemoryType::Episodic,
        namespace: "daily",
        chunking: ChunkingStrategy::SplitByHeading {
            fence_aware: true,
            level: 2,
        },
    },
    // Procedures: self-contained, one record per file. (Fixes the shredding bug.)
    SourceHandler {
        pattern: "memory/procedures/*.md",
        memory_type: MemoryType::Procedural,
        namespace: "procedural",
        chunking: ChunkingStrategy::OneRecordPerFile,
    },
    // Curated knowledge documents: one record per file.
    SourceHandler {
        pattern: "memory/curated/*.md",
        memory_type: MemoryType::Semantic,
        namespace: "curated",
        chunking: ChunkingStrategy::OneRecordPerFile,
    },
    // Active-context working memory: the whole file is one rolling summary record.
    SourceHandler {
        pattern: "memory/active-context.md",
        memory_type: MemoryType::Summary,
        namespace: "active-context",
        chunking: ChunkingStrategy::OneRecordPerFile,
    },
    // Legacy: gating policies are procedural. Kept multi-record for compatibility
    // with existing agents' records (split by heading as before).
    SourceHandler {
        pattern: "memory/gating-policies.md",
        memory_type: MemoryType::Procedural,
        namespace: "procedural",
        chunking: ChunkingStrategy::SplitByHeading {
            fence_aware: true,
            level: 2,
        },
    },
    // Legacy: per-project memory files. Kept multi-record for compatibility.
    SourceHandler {
        pattern: "memory/project-*.md",
        memory_type: MemoryType::Semantic,
        namespace: "project",
        chunking: ChunkingStrategy::SplitByHeading {
            fence_aware: true,
            level: 2,
        },
    },
    // Catch-all for anything else under memory/: one semantic record per file.
    SourceHandler {
        pattern: "memory/*.md",
        memory_type: MemoryType::Semantic,
        namespace: "workspace",
        chunking: ChunkingStrategy::OneRecordPerFile,
    },
];

/// Find the first handler whose pattern matches a workspace-relative path.
fn dispatch(relative_path: &str) -> Option<&'static SourceHandler> {
    let norm = relative_path.replace('\\', "/");
    SOURCE_HANDLERS
        .iter()
        .find(|h| path_matches(h.pattern, &norm))
}

/// Minimal glob matcher for the `SOURCE_HANDLERS` patterns. Supports exactly:
/// - literal bytes,
/// - a single `*` matching a run of non-`/` characters (one path segment), and
/// - `[0-9]` digit classes (used by the daily-date pattern).
///
/// Because `*` never crosses `/`, `memory/*.md` does not match a path under
/// `memory/procedures/`. Matched against the full workspace-relative path.
fn path_matches(pattern: &str, path: &str) -> bool {
    glob_match(pattern.as_bytes(), path.as_bytes())
}

fn glob_match(pattern: &[u8], path: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    match pattern[0] {
        b'*' => {
            let rest = &pattern[1..];
            // `*` matches zero or more non-`/` characters.
            if glob_match(rest, path) {
                return true;
            }
            let mut i = 0;
            while i < path.len() && path[i] != b'/' {
                i += 1;
                if glob_match(rest, &path[i..]) {
                    return true;
                }
            }
            false
        }
        b'[' if pattern.starts_with(b"[0-9]") => {
            if path.first().is_some_and(u8::is_ascii_digit) {
                glob_match(&pattern[5..], &path[1..])
            } else {
                false
            }
        }
        c => path.first() == Some(&c) && glob_match(&pattern[1..], &path[1..]),
    }
}

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

/// Split Markdown content on ATX headings of `level` (2 == `## `).
///
/// Content before the first heading becomes section 0 with `heading = None`.
/// When `fence_aware`, heading-looking lines inside ``` fenced code blocks are
/// treated as ordinary content, not split points. A section is emitted only if
/// its *body* (content minus its leading heading line(s)) is non-empty — see
/// `flush_section`.
pub(crate) fn split_markdown_sections(
    content: &str,
    level: u8,
    fence_aware: bool,
) -> Vec<MarkdownSection> {
    let marker = format!("{} ", "#".repeat(level as usize));
    let lines: Vec<&str> = content.lines().collect();
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut section_start: usize = 1; // 1-based
    let mut in_fence = false;

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1; // 1-based

        // A line opening or closing a fenced code block toggles fence state and
        // is never itself a heading.
        if fence_aware && line.starts_with("```") {
            in_fence = !in_fence;
            current_lines.push(line);
            continue;
        }

        if !in_fence && line.starts_with(marker.as_str()) {
            flush_section(
                &mut sections,
                current_heading.take(),
                &current_lines,
                section_start,
                lineno - 1,
            );
            current_heading = Some(line[marker.len()..].trim().to_string());
            current_lines = vec![line];
            section_start = lineno;
        } else {
            current_lines.push(line);
        }
    }

    flush_section(
        &mut sections,
        current_heading.take(),
        &current_lines,
        section_start,
        lines.len(),
    );

    sections
}

/// Push a section onto `sections` only if its *body* is non-empty after trimming,
/// where the body is the section's lines minus any leading heading line(s).
///
/// This single rule drops two kinds of noise: an empty `## ` section (heading
/// with no content), and a preamble that is only an H1 date header — the source
/// of the daily-journal over-chunking bug where `# Saturday, May 23rd, 2026`
/// became its own record. A preamble that carries real intro text is kept.
fn flush_section(
    sections: &mut Vec<MarkdownSection>,
    heading: Option<String>,
    lines: &[&str],
    line_start: usize,
    line_end: usize,
) {
    if lines.is_empty() {
        return;
    }
    // A `## ` section has exactly one leading heading line. A preamble has none,
    // but may open with an H1/run of ATX headings we also strip before judging.
    let body_start = if heading.is_some() {
        1
    } else {
        lines
            .iter()
            .position(|l| !is_heading_or_blank(l))
            .unwrap_or(lines.len())
    };
    if lines[body_start..].iter().all(|l| l.trim().is_empty()) {
        return;
    }
    sections.push(MarkdownSection {
        heading,
        content: lines.join("\n"),
        line_start,
        line_end,
    });
}

/// True for a blank line or any ATX heading line (`#`, `##`, …).
fn is_heading_or_blank(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

// ---------------------------------------------------------------------------
// Classification helpers
// ---------------------------------------------------------------------------

/// Generate a deterministic record ID from file path and section index.
///
/// NOTE: the ID is positional — it hashes `path:index`, so inserting or removing
/// a section mid-file renumbers every later section and changes its ID, inflating
/// the next delta. Acceptable today (procedures/curated are one record at index 0);
/// a content-addressed scheme is deferred to a future "stable memory IDs" package.
fn record_id(relative_path: &str, section_index: usize) -> Uuid {
    let name = format!("{relative_path}:{section_index}");
    Uuid::new_v5(&OPENCLAW_NS, name.as_bytes())
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
    let Some(handler) = dispatch(relative_path) else {
        // No handler matches this location → no structured record.
        return Vec::new();
    };

    let memory_type = handler.memory_type.clone();
    let namespace = handler.namespace.to_string();
    let extraction_method = classify_extraction_method(relative_path);
    let daily_date = parse_daily_date(relative_path);

    let sections = match handler.chunking {
        ChunkingStrategy::OneRecordPerFile => {
            // Entire file as one section.
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
        }
        ChunkingStrategy::SplitByHeading { fence_aware, level } => {
            split_markdown_sections(content, level, fence_aware)
        }
        ChunkingStrategy::VaultEntries => {
            unreachable!("VaultEntries is owned by the vault path (WP1), not the memory parser")
        }
        ChunkingStrategy::FileListingOnly => {
            unreachable!("FileListingOnly is owned by enumerate (WP3), not the memory parser")
        }
    };

    let file_category = namespace.as_str();

    sections
        .into_iter()
        .enumerate()
        .map(|(idx, section)| {
            let id = record_id(relative_path, idx);

            // Determine created_at: for daily logs use midnight of filename date,
            // otherwise fall back to file mtime.
            let created_at = if let Some(date) = daily_date {
                date.and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("midnight is valid"))
                    .and_utc()
            } else {
                file_mtime
            };

            // observed_at: only for daily logs
            let observed_at = daily_date.map(|date| {
                date.and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("midnight is valid"))
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
        let content = fs::read_to_string(&memory_md).context("Failed to read MEMORY.md")?;
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

            let content =
                fs::read_to_string(path).with_context(|| format!("Failed to read {relative}"))?;
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
        let sections = split_markdown_sections(md, 2, false);
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
        let sections = split_markdown_sections(md, 2, false);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, None);
        assert_eq!(sections[0].line_start, 1);
        assert_eq!(sections[0].line_end, 2);
    }

    #[test]
    fn split_sections_empty_file() {
        let sections = split_markdown_sections("", 2, false);
        assert!(sections.is_empty());
    }

    #[test]
    fn split_sections_h3_not_boundary() {
        let md = "\
## Section A

### Subsection

Text.
";
        let sections = split_markdown_sections(md, 2, false);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].content.contains("### Subsection"));
    }

    #[test]
    fn split_sections_only_whitespace_before_first_h2() {
        let md = "\n\n\n## Real Section\n\nContent.";
        let sections = split_markdown_sections(md, 2, false);
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

    // -- Source-handler dispatch + matcher (WP2) -------------------------------

    #[test]
    fn path_matches_supported_forms() {
        assert!(path_matches("MEMORY.md", "MEMORY.md"));
        assert!(!path_matches("MEMORY.md", "memory.md"));
        assert!(path_matches("memory/*.md", "memory/notes.md"));
        // `*` is segment-bounded: it must not cross `/`.
        assert!(!path_matches("memory/*.md", "memory/procedures/x.md"));
        assert!(path_matches(
            "memory/procedures/*.md",
            "memory/procedures/x.md"
        ));
        assert!(path_matches(
            "memory/project-*.md",
            "memory/project-clawsmith.md"
        ));

        let daily = "memory/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].md";
        assert!(path_matches(daily, "memory/2026-05-21.md"));
        assert!(!path_matches(daily, "memory/2026-5-21.md")); // wrong digit count
        assert!(!path_matches(daily, "memory/active-context.md"));
    }

    #[test]
    fn dispatch_routes_known_locations() {
        assert_eq!(
            dispatch("memory/procedures/x.md").unwrap().namespace,
            "procedural"
        );
        assert_eq!(
            dispatch("memory/curated/x.md").unwrap().namespace,
            "curated"
        );
        assert_eq!(
            dispatch("memory/2026-05-21.md").unwrap().memory_type,
            MemoryType::Episodic
        );
        // Anything else under memory/ falls to the catch-all.
        assert_eq!(dispatch("memory/random.md").unwrap().namespace, "workspace");
    }

    // -- Chunking-strategy behavior (WP2 acceptance cases) ---------------------

    #[test]
    fn parse_procedure_one_record() {
        let content = "\
# Morning Standup Procedure

## Steps

1. Check overnight alerts.
2. Post the summary.

```
## Standup YYYY-MM-DD
- summary template
```

## Notes

Keep it under five minutes.
";
        let records = parse_memory_file(
            "memory/procedures/morning-standup.md",
            content,
            Utc::now(),
            Uuid::nil(),
        );
        assert_eq!(records.len(), 1, "a procedure is a single record");
        assert_eq!(records[0].memory_type, MemoryType::Procedural);
        assert_eq!(records[0].namespace, "procedural");
        assert_eq!(
            records[0].content, content,
            "content is the whole file verbatim"
        );
        let raw = records[0].raw_source_format.as_ref().unwrap();
        assert_eq!(raw["line_start"].as_u64().unwrap(), 1);
        assert_eq!(
            raw["line_end"].as_u64().unwrap(),
            content.lines().count() as u64
        );
    }

    #[test]
    fn parse_curated_one_record() {
        let content = "\
# Postgres RLS

## Policy

Always enable RLS.

## Gotcha

Run inside a transaction.
";
        let records = parse_memory_file(
            "memory/curated/postgres-rls.md",
            content,
            Utc::now(),
            Uuid::nil(),
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].memory_type, MemoryType::Semantic);
        assert_eq!(records[0].namespace, "curated");
        assert_eq!(records[0].content, content);
    }

    #[test]
    fn parse_daily_three_sections() {
        let content = "\
## 09:00 Standup

Did the standup.

## 12:00 Lunch

Ate lunch.

## 17:00 Wrap

Wrapped up.
";
        let records = parse_memory_file("memory/2026-05-21.md", content, Utc::now(), Uuid::nil());
        assert_eq!(records.len(), 3);
        assert!(records
            .iter()
            .all(|r| r.memory_type == MemoryType::Episodic));
        assert!(records.iter().all(|r| r.namespace == "daily"));
    }

    #[test]
    fn parse_fence_aware_heading_not_split() {
        let content = "\
## Real Section

Text.

```
## Standup 2026-05-21
templated heading, not real
```

## Second Section

More.
";
        let records = parse_memory_file("memory/2026-05-21.md", content, Utc::now(), Uuid::nil());
        // The `## ` inside the fence must NOT start a third record.
        assert_eq!(records.len(), 2);
        assert!(records
            .iter()
            .any(|r| r.content.contains("## Standup 2026-05-21")));
    }

    #[test]
    fn parse_daily_no_headings_one_record() {
        let content = "Just a free-form note for the day.\nNo headings here.\n";
        let records = parse_memory_file("memory/2026-05-21.md", content, Utc::now(), Uuid::nil());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].memory_type, MemoryType::Episodic);
    }

    #[test]
    fn parse_empty_daily_zero_records() {
        let records = parse_memory_file("memory/2026-05-21.md", "", Utc::now(), Uuid::nil());
        assert!(records.is_empty());
    }

    #[test]
    fn parse_heading_with_empty_body_dropped() {
        // `## Foo` has no body and must be dropped; `## Bar` is kept.
        let content = "## Foo\n\n## Bar\n\nReal content.\n";
        let records = parse_memory_file("memory/2026-05-21.md", content, Utc::now(), Uuid::nil());
        assert_eq!(records.len(), 1);
        assert!(records[0].content.contains("Bar"));
        assert!(records[0].content.contains("Real content."));
    }

    #[test]
    fn parse_daily_drops_h1_date_preamble() {
        // Screenshot bug: the H1 date header must not become its own record.
        let content = "\
# Saturday, May 23rd, 2026

## 09:05 Reddit Watchdog

Recovered the watchdog.

## 11:00 Standup

Synced with the team.
";
        let records = parse_memory_file("memory/2026-05-23.md", content, Utc::now(), Uuid::nil());
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|r| r.content.contains("Watchdog") || r.content.contains("Standup")),
            "no record should be the bare date header"
        );
    }

    #[test]
    fn collect_skips_non_markdown_files() {
        use std::fs;
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path();
        fs::create_dir(ws.join("memory")).unwrap();
        fs::write(ws.join("memory/notes.txt"), "plain text, not markdown").unwrap();
        fs::write(ws.join("memory/2026-05-21.md"), "## Entry\n\nReal.\n").unwrap();
        let records = collect_all_memory(ws, Uuid::nil()).unwrap();
        // Only the .md file is parsed; notes.txt is ignored by the parser.
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].source.origin_file.as_deref(),
            Some("memory/2026-05-21.md")
        );
    }
}
