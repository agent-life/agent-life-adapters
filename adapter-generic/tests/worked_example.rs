//! Value-by-value assertion of the design §9 worked example.
//!
//! The golden corpus (`golden_corpus.rs`) pins the whole toy fixture; this test
//! asserts the two journal records exactly as the design §9 table specifies, so
//! a reviewer can read the DoD numbers straight off the assertions:
//! headings, line ranges 3-6 / 8-9, tags `["daily"]` / `["daily","planning"]`,
//! and `observed_at` = 2026-07-04T00:00:00Z (filename_date).

use std::path::Path;

use adapter_generic::GenericAdapter;
use alf_core::{Adapter, AlfReader, MemoryRecord, MemoryType};
use chrono::{TimeZone, Utc};

fn journal_records() -> Vec<MemoryRecord> {
    // Isolate the vault read path from the developer's real ~/.alf.
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", dir.path());
        std::env::set_var("HOME", dir.path());
        dir
    });
    let tmp = tempfile::TempDir::new().unwrap();
    let alf = tmp.path().join("out.alf");
    GenericAdapter
        .export(Path::new("tests/fixtures/toy"), &alf)
        .expect("export failed");
    let mut reader = AlfReader::new(std::io::BufReader::new(std::fs::File::open(&alf).unwrap()))
        .expect("open failed");
    let mut records: Vec<MemoryRecord> = reader
        .read_all_memory()
        .unwrap()
        .into_iter()
        .filter(|r| r.source.origin_file.as_deref() == Some("memories/2026-07-04.md"))
        .collect();
    records.sort_by_key(line_start);
    records
}

fn line_start(r: &MemoryRecord) -> u64 {
    r.raw_source_format
        .as_ref()
        .and_then(|v| v.get("line_start"))
        .and_then(|v| v.as_u64())
        .unwrap()
}

fn line_end(r: &MemoryRecord) -> u64 {
    r.raw_source_format
        .as_ref()
        .and_then(|v| v.get("line_end"))
        .and_then(|v| v.as_u64())
        .unwrap()
}

fn heading(r: &MemoryRecord) -> String {
    r.raw_source_format
        .as_ref()
        .and_then(|v| v.get("heading"))
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string()
}

#[test]
fn design_section_9_worked_example_is_exact() {
    let records = journal_records();

    // Exactly two records: the H1 preamble and the empty `## Blocked` heading
    // are dropped; the fenced `## this is not a heading` did not split.
    assert_eq!(records.len(), 2, "expected exactly two journal records");

    let midnight = Utc.with_ymd_and_hms(2026, 7, 4, 0, 0, 0).unwrap();

    // Section A.
    let a = &records[0];
    assert_eq!(a.memory_type, MemoryType::Episodic);
    assert_eq!(a.namespace, "daily");
    assert_eq!(heading(a), "Fixed the deploy pipeline");
    assert_eq!((line_start(a), line_end(a)), (3, 6));
    assert_eq!(a.tags, vec!["daily"]);
    assert_eq!(a.temporal.observed_at, Some(midnight));
    assert_eq!(a.temporal.created_at, midnight);
    // The fenced heading-looking line travels inside A's content verbatim.
    assert!(a.content.contains("## this is not a heading"));

    // Section B.
    let b = &records[1];
    assert_eq!(b.memory_type, MemoryType::Episodic);
    assert_eq!(b.namespace, "daily");
    assert_eq!(heading(b), "Standup notes");
    assert_eq!((line_start(b), line_end(b)), (8, 9));
    assert_eq!(b.tags, vec!["daily", "planning"]);
    assert_eq!(b.temporal.observed_at, Some(midnight));
    assert_eq!(b.temporal.created_at, midnight);

    // Distinct birth ids.
    assert_ne!(a.id, b.id);
}
