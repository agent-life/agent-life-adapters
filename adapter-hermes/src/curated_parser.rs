//! Parse Hermes curated memory (`memories/MEMORY.md`) into ALF records.
//!
//! Hermes's curated store is a tiny, capped, continuously-rewritten file whose
//! entries are separated by `§` (U+00A7), multiline allowed. Each entry becomes
//! one semantic `MemoryRecord` in the `curated` namespace. IDs are
//! **content-derived UUIDv5** (D2): an unchanged entry re-exports with the same
//! id, so `compute_delta` sees no churn, and an edit reads as delete+create.
//!
//! `USER.md` is handled separately as the human principal (Layer 3), not here.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use alf_core::{
    ExtractionMethod, MemoryRecord, MemoryStatus, MemoryType, SourceProvenance, TemporalMetadata,
};

const RUNTIME: &str = "hermes";
const ORIGIN_FILE: &str = "memories/MEMORY.md";

/// Collect curated-memory records from `memories/MEMORY.md` under the home.
///
/// Returns an empty vec when the file is absent or blank.
pub fn collect_curated_memory(
    home: &Path,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<Vec<MemoryRecord>> {
    let path = home.join("memories").join("MEMORY.md");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mtime = file_mtime(&path);

    let entries = split_entries(&content);
    let mut records = Vec::with_capacity(entries.len());
    // Occurrence disambiguates byte-identical entries (WP4.1 §8.5): two
    // identical curated lines would otherwise hash to one content-derived id
    // and `compute_delta`'s map would silently drop one. Occurrence 0 keeps the
    // historical id, so the common (distinct-entry) case never churns.
    let mut occurrences: HashMap<&str, u32> = HashMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        let occ = occurrences
            .entry(entry.as_str())
            .and_modify(|c| *c += 1)
            .or_insert(0);
        records.push(make_record(
            entry,
            idx,
            *occ,
            agent_id,
            runtime_version,
            mtime,
        ));
    }
    Ok(records)
}

/// Split curated content into entries.
///
/// Primary delimiter is `§`. If the file has no `§`, fall back to H2 (`## `)
/// sections; if it has neither, the whole file is one entry. Blank entries are
/// dropped.
fn split_entries(content: &str) -> Vec<String> {
    if content.contains('§') {
        return content
            .split('§')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let h2 = split_h2(content);
    if h2.len() > 1 {
        return h2;
    }
    vec![content.trim().to_string()]
}

/// Split on `## ` H2 headings, keeping the heading with its body.
fn split_h2(content: &str) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in content.lines() {
        if line.trim_start().starts_with("## ") && !current.trim().is_empty() {
            sections.push(current.trim().to_string());
            current = String::new();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current.trim().to_string());
    }
    sections
}

fn make_record(
    entry: &str,
    entry_index: usize,
    occurrence: u32,
    agent_id: Uuid,
    runtime_version: Option<&str>,
    mtime: DateTime<Utc>,
) -> MemoryRecord {
    // Content-derived id (D2): stable across re-exports of an unchanged entry,
    // reorder-proof. Occurrence disambiguates true duplicates (§8.5); the first
    // occurrence keeps the historical id string so distinct entries never churn.
    let id_name = if occurrence == 0 {
        format!("hermes-curated:{entry}")
    } else {
        format!("hermes-curated:{entry}:{occurrence}")
    };
    let id = Uuid::new_v5(&alf_core::ids::ALF_ID_NAMESPACE, id_name.as_bytes());

    MemoryRecord {
        id,
        agent_id,
        content: entry.to_string(),
        memory_type: classify(entry),
        source: SourceProvenance {
            runtime: RUNTIME.to_string(),
            runtime_version: runtime_version.map(str::to_string),
            origin: Some("memory_md".to_string()),
            origin_file: Some(ORIGIN_FILE.to_string()),
            extraction_method: Some(ExtractionMethod::AgentWritten),
            session_id: None,
            interaction_id: None,
            identity_version: None,
            extra: HashMap::new(),
        },
        temporal: TemporalMetadata {
            // Per-entry timestamps are unavailable in the flat file; use the
            // file mtime so unchanged exports stay stable.
            created_at: mtime,
            updated_at: None,
            observed_at: None,
            valid_from: None,
            valid_until: None,
            last_accessed_at: None,
            access_count: None,
            extra: HashMap::new(),
        },
        status: MemoryStatus::Active,
        namespace: "curated".to_string(),
        category: None,
        supersedes: None,
        confidence: None,
        entities: Vec::new(),
        tags: vec![RUNTIME.to_string(), "curated".to_string()],
        embeddings: Vec::new(),
        related_records: Vec::new(),
        raw_source_format: Some(serde_json::json!({
            "store": "memory",
            "entry_index": entry_index,
        })),
        extra: HashMap::new(),
    }
}

/// Best-effort: a rule-like entry is `procedural`, otherwise `semantic`.
fn classify(entry: &str) -> MemoryType {
    let head = entry
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', '-', '*', ' '])
        .to_ascii_lowercase();
    const RULE_STARTS: &[&str] = &[
        "always ", "never ", "don't ", "do not ", "avoid ", "prefer ", "use ",
    ];
    if RULE_STARTS.iter().any(|p| head.starts_with(p)) || head.contains(" must ") {
        MemoryType::Procedural
    } else {
        MemoryType::Semantic
    }
}

fn file_mtime(path: &Path) -> DateTime<Utc> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_memory(home: &Path, body: &str) {
        let mem = home.join("memories");
        fs::create_dir_all(&mem).unwrap();
        fs::write(mem.join("MEMORY.md"), body).unwrap();
    }

    #[test]
    fn splits_on_section_sign() {
        let tmp = tempfile::tempdir().unwrap();
        write_memory(
            tmp.path(),
            "User prefers Rust.\n§\nAlways run cargo fmt before commit.\n§\nThe staging URL is x.",
        );
        let recs = collect_curated_memory(tmp.path(), Uuid::new_v4(), None).unwrap();
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].namespace, "curated");
        assert_eq!(recs[0].memory_type, MemoryType::Semantic);
        // "Always run ..." → procedural.
        assert_eq!(recs[1].memory_type, MemoryType::Procedural);
        assert!(recs[0].embeddings.is_empty());
    }

    #[test]
    fn ids_are_content_stable() {
        let tmp = tempfile::tempdir().unwrap();
        write_memory(tmp.path(), "A fact.\n§\nAnother fact.");
        let a = collect_curated_memory(tmp.path(), Uuid::new_v4(), None).unwrap();
        let b = collect_curated_memory(tmp.path(), Uuid::new_v4(), None).unwrap();
        assert_eq!(a[0].id, b[0].id, "same content → same id (no delta churn)");
        assert_ne!(a[0].id, a[1].id);
    }

    #[test]
    fn duplicate_entries_get_distinct_ids() {
        // Two byte-identical curated entries must not collide to one id (§8.5),
        // and the first occurrence must keep the historical content-only id so
        // distinct entries never churn on upgrade.
        let tmp = tempfile::tempdir().unwrap();
        write_memory(tmp.path(), "Same note.\n§\nSame note.");
        let recs = collect_curated_memory(tmp.path(), Uuid::new_v4(), None).unwrap();
        assert_eq!(recs.len(), 2);
        assert_ne!(
            recs[0].id, recs[1].id,
            "occurrence disambiguates duplicates"
        );
        // First occurrence == the historical content-only id.
        let legacy = Uuid::new_v5(
            &alf_core::ids::ALF_ID_NAMESPACE,
            b"hermes-curated:Same note.",
        );
        assert_eq!(
            recs[0].id, legacy,
            "occurrence 0 keeps the old id (no churn)"
        );
    }

    #[test]
    fn h2_fallback_when_no_section_sign() {
        let tmp = tempfile::tempdir().unwrap();
        write_memory(tmp.path(), "## Env\n\nuses nix\n\n## Style\n\ntabs\n");
        let recs = collect_curated_memory(tmp.path(), Uuid::new_v4(), None).unwrap();
        assert_eq!(recs.len(), 2);
    }

    #[test]
    fn whole_file_when_no_delimiters() {
        let tmp = tempfile::tempdir().unwrap();
        write_memory(tmp.path(), "Just one blob of notes with no markers.");
        let recs = collect_curated_memory(tmp.path(), Uuid::new_v4(), None).unwrap();
        assert_eq!(recs.len(), 1);
    }

    #[test]
    fn missing_file_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(collect_curated_memory(tmp.path(), Uuid::new_v4(), None)
            .unwrap()
            .is_empty());
    }
}
