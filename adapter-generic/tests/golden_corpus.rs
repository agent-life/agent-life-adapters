//! Golden-corpus parity gate for the generic adapter (WP-M1).
//!
//! Same mechanism as `adapter-openclaw`'s golden gate: serialize *every* emitted
//! memory record over the toy fixture into a committed golden, capturing only
//! the chunker-controlled, checkout-stable fields (id, memory_type, namespace,
//! sha256(content), raw_source_format line_start/line_end/heading, and the
//! date-derived observed_at). Mtime-derived fields are excluded so the corpus is
//! stable across checkouts (the fixture pins `.alf-agent-id` so ids are stable
//! too).
//!
//! This is the concrete reproduction of the design §9 worked example — see
//! `worked_example.rs` for the value-by-value assertion of that example.
//!
//! Regenerate intentionally with
//! `UPDATE_GOLDEN=1 cargo test -p adapter-generic --test golden_corpus`.
//! Regeneration deliberately FAILS the run — re-run without `UPDATE_GOLDEN` to
//! verify.

use std::path::Path;

use adapter_generic::GenericAdapter;
use alf_core::ids::sha256_hex;
use alf_core::{Adapter, AlfReader};
use serde::Serialize;

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
    tags: Vec<String>,
    observed_at: Option<String>,
}

fn golden_rows(fixture: &str) -> Vec<GoldenRecord> {
    // Isolate the vault read path (the golden captures only memory records, but
    // keep the export hermetic regardless of the developer's ~/.alf).
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", dir.path());
        std::env::set_var("HOME", dir.path());
        dir
    });
    let fixture_dir = Path::new("tests/fixtures").join(fixture);
    let tmp = tempfile::TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    GenericAdapter
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
                tags: r.tags,
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
        "golden corpus drift for fixture `{}` — the generic extractor changed emitted records",
        fixture
    );
}

#[test]
fn golden_toy() {
    check_fixture("toy");
}
