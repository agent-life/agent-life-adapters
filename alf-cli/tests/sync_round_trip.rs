//! Round-trip integration tests for the sync pipeline.
//!
//! These tests exercise the full data path that `alf sync` + `alf restore` use:
//!   export workspace → compute delta → rebuild snapshot → verify records match
//!
//! No live service is needed — the tests operate entirely on in-memory archives.

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;

use adapter_openclaw::OpenClawAdapter;
use alf_core::archive::{AlfReader, DeltaMemoryEntry, DeltaWriter};
use alf_core::delta::compute_delta;
use alf_core::manifest::{ChangeInventory, DeltaAgentRef, DeltaManifest, DeltaSyncCursor};
use alf_core::memory::MemoryRecord;
use alf_core::rebuild::rebuild_snapshot;
use alf_core::Adapter;
use chrono::Utc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a minimal OpenClaw workspace and return its path inside the TempDir.
fn create_workspace(
    tmp: &TempDir,
    name: &str,
    soul: &str,
    memory_md: &str,
    daily_logs: &[(&str, &str)],
) -> std::path::PathBuf {
    let ws = tmp.path().join(name);
    fs::create_dir_all(ws.join("memory")).unwrap();
    fs::write(ws.join("SOUL.md"), format!("# {soul}\n\nYou are {soul}.")).unwrap();
    fs::write(
        ws.join("IDENTITY.md"),
        format!("# Identity\nRole: Assistant\nName: {soul}"),
    )
    .unwrap();
    if !memory_md.is_empty() {
        fs::write(ws.join("MEMORY.md"), memory_md).unwrap();
    }
    for (filename, content) in daily_logs {
        fs::write(ws.join("memory").join(filename), content).unwrap();
    }
    ws
}

/// Export a workspace to an .alf archive via the OpenClaw adapter.
fn export_workspace(workspace: &std::path::Path, output: &std::path::Path) -> Vec<u8> {
    let adapter = OpenClawAdapter;
    adapter.export(workspace, output).expect("export failed");
    fs::read(output).expect("failed to read exported .alf")
}

/// Read all memory records from a .alf archive.
fn read_records(alf_bytes: &[u8]) -> Vec<MemoryRecord> {
    let mut reader = AlfReader::new(Cursor::new(alf_bytes)).expect("failed to open .alf");
    reader.read_all_memory().expect("failed to read memory")
}

/// Read the identity version from a .alf archive.
fn read_identity_version(alf_bytes: &[u8]) -> Option<u32> {
    let mut reader = AlfReader::new(Cursor::new(alf_bytes)).expect("failed to open .alf");
    reader
        .read_identity()
        .expect("failed to read identity")
        .map(|id| id.version)
}

/// Build a delta archive from delta entries with the given base_sequence.
fn build_delta(
    agent_id: uuid::Uuid,
    base_sequence: u64,
    entries: &[DeltaMemoryEntry],
) -> Vec<u8> {
    let manifest = DeltaManifest {
        alf_version: "1.0.0".into(),
        created_at: Utc::now(),
        agent: DeltaAgentRef {
            id: agent_id,
            source_runtime: Some("openclaw".into()),
            extra: HashMap::new(),
        },
        sync: DeltaSyncCursor {
            base_sequence,
            new_sequence: base_sequence + 1,
            base_timestamp: None,
            new_timestamp: None,
            extra: HashMap::new(),
        },
        changes: ChangeInventory {
            identity: None,
            principals: None,
            credentials: None,
            memory: None,
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    };

    let buf = Cursor::new(Vec::new());
    let mut writer = DeltaWriter::new(buf, manifest).expect("failed to create delta writer");
    if !entries.is_empty() {
        writer
            .add_memory_deltas(entries)
            .expect("failed to add memory deltas");
    }
    writer.finish().expect("failed to finish delta").into_inner()
}

/// Compare two sets of memory records by (id → content), order-independent.
/// Returns a descriptive error message on mismatch, or None if they match.
fn compare_records(expected: &[MemoryRecord], actual: &[MemoryRecord]) -> Option<String> {
    if expected.len() != actual.len() {
        return Some(format!(
            "record count mismatch: expected {}, got {}",
            expected.len(),
            actual.len()
        ));
    }

    let expected_map: HashMap<uuid::Uuid, &str> =
        expected.iter().map(|r| (r.id, r.content.as_str())).collect();
    let actual_map: HashMap<uuid::Uuid, &str> =
        actual.iter().map(|r| (r.id, r.content.as_str())).collect();

    for (id, expected_content) in &expected_map {
        match actual_map.get(id) {
            None => {
                return Some(format!(
                    "record {} missing in actual (content: {:?})",
                    id,
                    &expected_content[..expected_content.len().min(60)]
                ))
            }
            Some(actual_content) => {
                if expected_content != actual_content {
                    return Some(format!(
                        "record {} content mismatch:\n  expected: {:?}\n  actual:   {:?}",
                        id,
                        &expected_content[..expected_content.len().min(80)],
                        &actual_content[..actual_content.len().min(80)],
                    ));
                }
            }
        }
    }

    for id in actual_map.keys() {
        if !expected_map.contains_key(id) {
            return Some(format!("unexpected record {} in actual", id));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Export → read back → verify records and identity survive the round trip.
#[test]
fn snapshot_round_trip() {
    let tmp = TempDir::new().unwrap();

    let ws = create_workspace(
        &tmp,
        "workspace",
        "Atlas",
        "## Fact 1\n\nThe sky is blue.\n\n## Fact 2\n\nWater is wet.",
        &[
            (
                "2026-01-15.md",
                "## Morning\n\nDiscussed plans.\n\n## Afternoon\n\nShipped the feature.",
            ),
            ("2026-01-16.md", "## All day\n\nDebugging session."),
        ],
    );

    let alf_path = tmp.path().join("snapshot.alf");
    let alf_bytes = export_workspace(&ws, &alf_path);

    let records = read_records(&alf_bytes);
    // 2 from MEMORY.md + 2 from Jan 15 + 1 from Jan 16 = 5
    assert_eq!(records.len(), 5, "expected 5 records from baseline workspace");

    let identity_version = read_identity_version(&alf_bytes);
    assert!(
        identity_version.is_some(),
        "identity should be present in exported snapshot"
    );

    // Import into a fresh workspace and re-export — records should be identical
    let adapter = OpenClawAdapter;
    let restored_ws = tmp.path().join("restored");
    adapter
        .import(&alf_path, &restored_ws)
        .expect("import failed");

    let re_exported_path = tmp.path().join("re-exported.alf");
    let re_exported_bytes = export_workspace(&restored_ws, &re_exported_path);
    let re_exported_records = read_records(&re_exported_bytes);

    assert_eq!(
        records.len(),
        re_exported_records.len(),
        "re-exported record count should match original"
    );
}

/// Export baseline → mutate → export again → compute delta → rebuild → verify
/// the rebuilt snapshot matches the mutated export.
#[test]
fn snapshot_plus_delta_round_trip() {
    let tmp = TempDir::new().unwrap();

    // ── Baseline ──────────────────────────────────────────────────
    let ws = create_workspace(
        &tmp,
        "workspace",
        "Atlas",
        "## Architecture\n\nEvent-sourced with Postgres.\n\n## Conventions\n\nTwo approvals required.",
        &[
            (
                "2026-01-15.md",
                "## Standup\n\nDiscussed Redis migration.\n\n## Review\n\nReviewed CDC pipeline.",
            ),
        ],
    );

    let base_path = tmp.path().join("base.alf");
    let base_bytes = export_workspace(&ws, &base_path);
    let base_records = read_records(&base_bytes);
    assert_eq!(base_records.len(), 4, "baseline: 2 MEMORY.md + 2 daily");

    let agent_id = {
        let reader = AlfReader::new(Cursor::new(&base_bytes)).unwrap();
        reader.manifest().agent.id
    };

    // ── Mutate workspace ──────────────────────────────────────────

    // Add a new daily log
    fs::write(
        ws.join("memory/2026-01-16.md"),
        "## Migration prep\n\nWrote the migration runbook.\n\n## Load testing\n\nRan traffic replay.",
    )
    .unwrap();

    // Modify MEMORY.md — add a third section
    fs::write(
        ws.join("MEMORY.md"),
        "## Architecture\n\nEvent-sourced with Postgres.\n\n## Conventions\n\nTwo approvals required.\n\n## Connection pooling\n\nUse PgBouncer in transaction mode.",
    )
    .unwrap();

    // ── Export mutated state ──────────────────────────────────────
    let mutated_path = tmp.path().join("mutated.alf");
    let mutated_bytes = export_workspace(&ws, &mutated_path);
    let mutated_records = read_records(&mutated_bytes);
    // 3 MEMORY.md sections + 2 from Jan 15 + 2 from Jan 16 = 7
    assert_eq!(mutated_records.len(), 7, "mutated: 3 MEMORY.md + 2 daily-15 + 2 daily-16");

    // ── Compute delta ─────────────────────────────────────────────
    let delta_entries = compute_delta(&base_records, &mutated_records);
    assert!(
        !delta_entries.is_empty(),
        "delta should not be empty after mutation"
    );

    // ── Build delta archive ───────────────────────────────────────
    let delta_bytes = build_delta(agent_id, 0, &delta_entries);

    // ── Rebuild ───────────────────────────────────────────────────
    let rebuilt_bytes = rebuild_snapshot(&base_bytes, &[delta_bytes.as_slice()])
        .expect("rebuild failed");

    let rebuilt_records = read_records(&rebuilt_bytes);

    // ── Verify ────────────────────────────────────────────────────
    if let Some(err) = compare_records(&mutated_records, &rebuilt_records) {
        panic!("rebuilt snapshot doesn't match mutated export: {}", err);
    }
}

/// Baseline → mutate 1 → delta 1 → mutate 2 → delta 2 → rebuild(base + [d1, d2])
/// → verify matches final state.
#[test]
fn multi_delta_chain() {
    let tmp = TempDir::new().unwrap();

    // ── Baseline ──────────────────────────────────────────────────
    let ws = create_workspace(
        &tmp,
        "workspace",
        "Meridian",
        "## Stack\n\nNext.js 14 with TypeScript.",
        &[(
            "2026-01-15.md",
            "## Session 1\n\nFixed hydration bug.\n\n## Session 2\n\nAdded SSE notifications.",
        )],
    );

    let base_path = tmp.path().join("base.alf");
    let base_bytes = export_workspace(&ws, &base_path);
    let base_records = read_records(&base_bytes);
    assert_eq!(base_records.len(), 3, "baseline: 1 MEMORY.md + 2 daily");

    let agent_id = {
        let reader = AlfReader::new(Cursor::new(&base_bytes)).unwrap();
        reader.manifest().agent.id
    };

    // ── Mutation 1: add a daily log ───────────────────────────────
    fs::write(
        ws.join("memory/2026-01-16.md"),
        "## Refactor\n\nMigrated to Server Components.",
    )
    .unwrap();

    let m1_path = tmp.path().join("m1.alf");
    let m1_bytes = export_workspace(&ws, &m1_path);
    let m1_records = read_records(&m1_bytes);
    assert_eq!(m1_records.len(), 4);

    let delta1_entries = compute_delta(&base_records, &m1_records);
    let delta1_bytes = build_delta(agent_id, 0, &delta1_entries);

    // ── Mutation 2: add another daily + modify MEMORY.md ──────────
    fs::write(
        ws.join("memory/2026-01-17.md"),
        "## Performance\n\nOptimized queries.",
    )
    .unwrap();

    // Remove Session 2 from Jan 15 (simulates cleanup)
    fs::write(
        ws.join("memory/2026-01-15.md"),
        "## Session 1\n\nFixed hydration bug.",
    )
    .unwrap();

    let m2_path = tmp.path().join("m2.alf");
    let m2_bytes = export_workspace(&ws, &m2_path);
    let m2_records = read_records(&m2_bytes);
    // 1 MEMORY.md + 1 Jan 15 (session 2 deleted) + 1 Jan 16 + 1 Jan 17 = 4
    assert_eq!(m2_records.len(), 4);

    let delta2_entries = compute_delta(&m1_records, &m2_records);
    let delta2_bytes = build_delta(agent_id, 1, &delta2_entries);

    // ── Rebuild from base + [delta1, delta2] ──────────────────────
    let rebuilt_bytes = rebuild_snapshot(
        &base_bytes,
        &[delta1_bytes.as_slice(), delta2_bytes.as_slice()],
    )
    .expect("rebuild failed");

    let rebuilt_records = read_records(&rebuilt_bytes);

    // ── Verify rebuilt matches final mutated state ─────────────────
    if let Some(err) = compare_records(&m2_records, &rebuilt_records) {
        panic!(
            "rebuilt snapshot doesn't match final state after 2 deltas: {}",
            err
        );
    }

    // Also verify the deleted record is actually gone
    let rebuilt_contents: Vec<&str> = rebuilt_records.iter().map(|r| r.content.as_str()).collect();
    assert!(
        !rebuilt_contents.iter().any(|c| c.contains("SSE notifications")),
        "Session 2 (SSE notifications) should have been deleted by delta 2"
    );
}

/// Verify that identity survives the rebuild even when only memory changes.
#[test]
fn rebuild_preserves_identity_through_memory_delta() {
    let tmp = TempDir::new().unwrap();

    let ws = create_workspace(
        &tmp,
        "workspace",
        "Atlas",
        "## Fact\n\nOriginal fact.",
        &[],
    );

    let base_path = tmp.path().join("base.alf");
    let base_bytes = export_workspace(&ws, &base_path);
    let base_records = read_records(&base_bytes);

    let base_identity_version = read_identity_version(&base_bytes);
    assert!(base_identity_version.is_some(), "base should have identity");

    let agent_id = {
        let reader = AlfReader::new(Cursor::new(&base_bytes)).unwrap();
        reader.manifest().agent.id
    };

    // Mutate only memory — identity unchanged
    fs::write(
        ws.join("MEMORY.md"),
        "## Fact\n\nOriginal fact.\n\n## New fact\n\nAdded after sync.",
    )
    .unwrap();

    let m_path = tmp.path().join("mutated.alf");
    let m_bytes = export_workspace(&ws, &m_path);
    let m_records = read_records(&m_bytes);

    let delta_entries = compute_delta(&base_records, &m_records);
    let delta_bytes = build_delta(agent_id, 0, &delta_entries);

    let rebuilt_bytes =
        rebuild_snapshot(&base_bytes, &[delta_bytes.as_slice()]).expect("rebuild failed");

    // Identity should survive
    let rebuilt_identity_version = read_identity_version(&rebuilt_bytes);
    assert_eq!(
        base_identity_version, rebuilt_identity_version,
        "identity version should be preserved through memory-only delta"
    );

    // Memory should be updated
    let rebuilt_records = read_records(&rebuilt_bytes);
    assert_eq!(rebuilt_records.len(), 2, "rebuilt should have 2 MEMORY.md sections");
}

/// Full pipeline: export → rebuild with no deltas → import → re-export → compare.
/// Proves the rebuild serialization is compatible with the import pipeline.
#[test]
fn rebuild_then_import_produces_matching_workspace() {
    let tmp = TempDir::new().unwrap();

    let ws = create_workspace(
        &tmp,
        "workspace",
        "Atlas",
        "## Architecture\n\nEvent sourced.\n\n## Team\n\nThree engineers.",
        &[
            ("2026-01-15.md", "## Morning\n\nStandup notes.\n\n## EOD\n\nShipped v2."),
        ],
    );

    let base_path = tmp.path().join("base.alf");
    let base_bytes = export_workspace(&ws, &base_path);
    let base_records = read_records(&base_bytes);

    // Rebuild with no deltas (normalizes the archive)
    let rebuilt_bytes = rebuild_snapshot(&base_bytes, &[]).expect("rebuild failed");

    // Import the rebuilt snapshot into a fresh workspace
    let rebuilt_alf_path = tmp.path().join("rebuilt.alf");
    fs::write(&rebuilt_alf_path, &rebuilt_bytes).unwrap();

    let adapter = OpenClawAdapter;
    let restored_ws = tmp.path().join("restored");
    let import_report = adapter
        .import(&rebuilt_alf_path, &restored_ws)
        .expect("import of rebuilt snapshot failed");

    assert_eq!(
        import_report.memory_records,
        base_records.len() as u64,
        "imported record count should match base"
    );
    assert!(
        import_report.identity_imported,
        "identity should be imported from rebuilt snapshot"
    );

    // Re-export the restored workspace and compare records
    let re_exported_path = tmp.path().join("re-exported.alf");
    let re_exported_bytes = export_workspace(&restored_ws, &re_exported_path);
    let re_exported_records = read_records(&re_exported_bytes);

    if let Some(err) = compare_records(&base_records, &re_exported_records) {
        panic!(
            "re-exported records don't match original after rebuild → import: {}",
            err
        );
    }
}
