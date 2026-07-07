//! Golden-corpus parity gate for the chunker (WP-M0).
//!
//! Serializes *every* emitted memory record over *every* fixture workspace into
//! a committed golden file, capturing the chunker-controlled, checkout-stable
//! fields: id, memory_type, namespace, sha256(content), `raw_source_format`
//! (line_start, line_end, heading), and `observed_at` (date-derived from the
//! filename for daily journals — deterministic; it is `None` for every other
//! source). Mtime-derived fields (`created_at` on non-daily files,
//! `updated_at`) are deliberately excluded so the corpus is stable across git
//! checkouts.
//!
//! `content_sha256` fingerprints the record's **raw content bytes**. It is NOT
//! the hash inside the record id: `alf_core::ids::memory_record_id` hashes
//! `content.trim_end()`. The `id` column pins id parity independently.
//!
//! This is the parity baseline for promoting the markdown splitter into
//! `alf_core::chunk`: the goldens must be **bit-for-bit unchanged** before and
//! after the move. The existing suites (`export_correctness`, `delta_stability`,
//! `round_trip`) assert invariants, not corpora — they cannot serve as the gate.
//!
//! To regenerate after an *intentional* corpus change, run with
//! `UPDATE_GOLDEN=1 cargo test -p adapter-openclaw --test golden_corpus`.
//! Regeneration deliberately FAILS the test run (so it can never masquerade as
//! a passing verification) — re-run without `UPDATE_GOLDEN` to verify.

use std::path::Path;

use adapter_openclaw::OpenClawAdapter;
use alf_core::ids::sha256_hex;
use alf_core::{Adapter, AlfReader};
use serde::Serialize;

/// One golden row per emitted record — only chunker-controlled, checkout-stable fields.
#[derive(Serialize)]
struct GoldenRecord {
    origin_file: Option<String>,
    id: String,
    memory_type: String,
    namespace: String,
    content_sha256: String,
    line_start: Option<u64>,
    line_end: Option<u64>,
    heading: Option<String>,
    /// Date-derived (filename) for daily journals, `None` otherwise. Never mtime.
    observed_at: Option<String>,
}

/// Export a fixture and reduce its memory records to the stable golden rows,
/// sorted deterministically so ordering never depends on file mtimes.
fn golden_rows(fixture: &str) -> Vec<GoldenRecord> {
    let fixture_dir = Path::new("tests/fixtures").join(fixture);
    let tmp = tempfile::TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    OpenClawAdapter
        .export(&fixture_dir, &alf_path)
        .expect("export failed");

    let file = std::fs::File::open(&alf_path).unwrap();
    let mut reader = AlfReader::new(std::io::BufReader::new(file)).expect("open failed");
    let records = reader.read_all_memory().expect("read memory failed");

    let mut rows: Vec<GoldenRecord> = records
        .into_iter()
        .map(|r| {
            let rsf = r.raw_source_format.unwrap_or(serde_json::Value::Null);
            GoldenRecord {
                origin_file: r.source.origin_file,
                id: r.id.to_string(),
                memory_type: r.memory_type.to_string(),
                namespace: r.namespace,
                content_sha256: sha256_hex(r.content.as_bytes()),
                line_start: rsf.get("line_start").and_then(|v| v.as_u64()),
                line_end: rsf.get("line_end").and_then(|v| v.as_u64()),
                heading: rsf
                    .get("heading")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                observed_at: r.temporal.observed_at.map(|t| t.to_rfc3339()),
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        (&a.origin_file, a.line_start, &a.id).cmp(&(&b.origin_file, b.line_start, &b.id))
    });
    rows
}

fn check_fixture(fixture: &str) {
    let rows = golden_rows(fixture);
    let actual = serde_json::to_string_pretty(&rows).unwrap() + "\n";
    let golden_path = Path::new("tests/fixtures/golden").join(format!("{fixture}.json"));

    // Exact-value gate: only UPDATE_GOLDEN=1 regenerates. `UPDATE_GOLDEN=0`,
    // empty, or any other value must NOT silently rewrite the baseline.
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&golden_path, &actual).expect("write golden");
        panic!(
            "golden regenerated for `{fixture}` at {} — this run is NOT a \
             verification; re-run without UPDATE_GOLDEN to verify",
            golden_path.display()
        );
    }

    let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|_| {
        panic!(
            "missing golden file {}; regenerate with UPDATE_GOLDEN=1",
            golden_path.display()
        )
    });
    pretty_assertions::assert_eq!(
        actual,
        expected,
        "golden corpus drift for fixture `{}` — the chunker changed emitted records",
        fixture
    );
}

#[test]
fn golden_standard() {
    check_fixture("standard");
}

#[test]
fn golden_community_patterns() {
    check_fixture("community-patterns");
}

#[test]
fn golden_minimal() {
    check_fixture("minimal");
}

#[test]
fn golden_empty() {
    check_fixture("empty");
}
