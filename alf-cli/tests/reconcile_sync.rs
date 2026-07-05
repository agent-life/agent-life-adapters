//! WP4.1 integration pins: the sync front-half (export → reconcile → archive
//! rewrite → diff) through the real OpenClaw adapter, no network.
//!
//! These pin the two invariants `execute_delta` relies on:
//! 1. an in-place curation of MEMORY.md reconciles to exactly one Update that
//!    keeps the record's identity, and the rewritten archive carries the
//!    reconciled records while every other layer (notably the raw tree) stays
//!    byte-identical to the fresh export;
//! 2. the rewritten archive is a fixed point — re-exporting an unchanged
//!    workspace and reconciling against it yields an empty delta.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use adapter_openclaw::OpenClawAdapter;
use alf_core::delta::compute_delta;
use alf_core::manifest::DeltaOperation;
use alf_core::memory::MemoryRecord;
use alf_core::reconcile::reconcile;
use alf_core::{replace_memory_records, Adapter, AlfReader};
use tempfile::TempDir;

const MEMORY_V1: &str = "\
## Identity

Name: Atlas. Reference code: ATLAS-SEM1-7F3A.

## Preferences

Terse answers. Metric units.
";

const MEMORY_V2_CURATED: &str = "\
## Preferences

Terse answers. Metric units.

## Identity

Name: Atlas. Reference code: ATLAS-SEM2-9E4C.
";

fn export_bytes(workspace: &Path) -> Vec<u8> {
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    OpenClawAdapter.export(workspace, &alf_path).unwrap();
    fs::read(&alf_path).unwrap()
}

fn read_memory(bytes: &[u8]) -> Vec<MemoryRecord> {
    let mut reader = AlfReader::new(Cursor::new(bytes)).unwrap();
    reader.read_all_memory().unwrap()
}

/// Raw tree as a sorted list — the rewritten archive stores entries in
/// BTreeMap order while the adapter writes them in walk order; only the
/// path→bytes mapping matters.
fn raw_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut reader = AlfReader::new(Cursor::new(bytes)).unwrap();
    let names: Vec<String> = reader
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with("raw/") && !n.ends_with('/'))
        .collect();
    let mut entries: Vec<(String, Vec<u8>)> = names
        .into_iter()
        .map(|n| {
            let data = reader.read_raw_entry(&n).unwrap();
            (n, data)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn make_workspace(dir: &Path) -> PathBuf {
    let ws = dir.join("workspace");
    fs::create_dir_all(ws.join("memory")).unwrap();
    fs::write(ws.join("SOUL.md"), "# Identity\nA test agent.").unwrap();
    fs::write(ws.join("MEMORY.md"), MEMORY_V1).unwrap();
    ws
}

#[test]
fn curated_edit_yields_single_update_and_rewritten_base() {
    let tmp = TempDir::new().unwrap();
    let ws = make_workspace(tmp.path());

    // Sync 1's base: the v1 export.
    let base_bytes = export_bytes(&ws);
    let prev_records = read_memory(&base_bytes);

    // The agent curates: re-ranks sections AND overwrites the reference code
    // in place (the WP4.1 §1a shape).
    fs::write(ws.join("MEMORY.md"), MEMORY_V2_CURATED).unwrap();
    let fresh_bytes = export_bytes(&ws);
    let exported = read_memory(&fresh_bytes);

    let outcome = reconcile(&prev_records, exported);
    assert!(outcome.rewritten);

    // Exactly one Update, carrying the base record's identity and anchor.
    let delta = compute_delta(&prev_records, &outcome.records);
    assert_eq!(delta.len(), 1, "delta: {delta:?}");
    assert_eq!(delta[0].operation, DeltaOperation::Update);
    assert!(delta[0].record.content.contains("ATLAS-SEM2-9E4C"));
    let base_identity = prev_records
        .iter()
        .find(|r| r.content.contains("ATLAS-SEM1-7F3A"))
        .unwrap();
    assert_eq!(delta[0].record.id, base_identity.id);
    assert_eq!(
        delta[0].record.temporal.created_at,
        base_identity.temporal.created_at
    );

    // The rewritten archive carries the reconciled records...
    let rewritten = replace_memory_records(&fresh_bytes, &outcome.records).unwrap();
    assert_eq!(read_memory(&rewritten), outcome.records);
    // ...and the raw tree stays byte-identical to the fresh export (the live
    // curated MEMORY.md, not the base one).
    assert_eq!(raw_entries(&rewritten), raw_entries(&fresh_bytes));
    let raw_memory = raw_entries(&rewritten)
        .into_iter()
        .find(|(n, _)| n == "raw/openclaw/MEMORY.md")
        .expect("raw MEMORY.md present")
        .1;
    assert_eq!(raw_memory, MEMORY_V2_CURATED.as_bytes());
}

#[test]
fn second_sync_after_rewrite_is_no_changes() {
    let tmp = TempDir::new().unwrap();
    let ws = make_workspace(tmp.path());

    let base_bytes = export_bytes(&ws);
    let prev_records = read_memory(&base_bytes);

    fs::write(ws.join("MEMORY.md"), MEMORY_V2_CURATED).unwrap();
    let outcome = reconcile(&prev_records, read_memory(&export_bytes(&ws)));
    let new_base = outcome.records;

    // Sync 2: the workspace is untouched; a fresh export reconciled against
    // the rewritten base must be a fixed point with an empty delta.
    let second = reconcile(&new_base, read_memory(&export_bytes(&ws)));
    assert!(compute_delta(&new_base, &second.records).is_empty());
    assert_eq!(second.records, new_base);
}
