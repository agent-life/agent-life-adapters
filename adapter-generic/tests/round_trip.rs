//! Round-trip, determinism, and reconcile DoD tests for the generic adapter.
//!
//! - `raw/generic/` round-trips byte-for-byte (incl. the binary SQLite file).
//! - double export ⇒ zero delta (deterministic extraction).
//! - restore → re-export ⇒ zero delta *through reconcile* (a restore re-stamps
//!   file mtimes, which reconcile absorbs — the production sync path).
//! - in-place body edit → same id (P2 Update); heading+body edit → P4
//!   create+delete.

use std::fs;
use std::path::Path;

use adapter_generic::GenericAdapter;
use alf_core::{compute_delta, reconcile, Adapter, AlfReader, MemoryRecord};
use std::sync::OnceLock;
use uuid::Uuid;

/// Isolate ALF_HOME/HOME for the whole test process so `export`'s vault read and
/// `import`'s vault write never touch the developer's real `~/.alf`.
fn isolate_home() {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", dir.path());
        std::env::set_var("HOME", dir.path());
        dir
    });
}

const FIXTURE: &str = "tests/fixtures/toy";
/// Fixed agent id for the reconcile mini-workspaces so birth ids are comparable
/// across the v1/v2/v3 edits (a path-derived id would differ per tempdir).
const AGENT_ID: &str = "f47ac10b-58cc-4372-a567-0e02b2c3d479";

fn read_memory(alf: &Path) -> Vec<MemoryRecord> {
    let file = fs::File::open(alf).unwrap();
    let mut reader = AlfReader::new(std::io::BufReader::new(file)).unwrap();
    reader.read_all_memory().unwrap()
}

fn export(workspace: &Path, out: &Path) {
    // The first export in this binary isolates HOME process-wide (OnceLock), so
    // every later export/import here uses the throwaway vault path.
    isolate_home();
    GenericAdapter
        .export(workspace, out)
        .expect("export failed");
}

// ---------------------------------------------------------------------------
// Manifest registration (backs the live `alf sync -r generic` DoD)
// ---------------------------------------------------------------------------

#[test]
fn manifest_declares_generic_runtime_and_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let alf = tmp.path().join("out.alf");
    export(Path::new(FIXTURE), &alf);

    let reader = AlfReader::new(fs::File::open(&alf).unwrap()).unwrap();
    let agent = &reader.manifest().agent;
    assert_eq!(agent.source_runtime, "generic");
    assert_eq!(
        agent.source_runtime_version.as_deref(),
        Some("toybox/0.1.0")
    );
    assert_eq!(agent.name, "Toybot"); // identity_file parse
}

// ---------------------------------------------------------------------------
// Raw byte-fidelity
// ---------------------------------------------------------------------------

#[test]
fn raw_tree_round_trips_byte_for_byte() {
    let tmp = tempfile::TempDir::new().unwrap();
    let alf = tmp.path().join("out.alf");
    export(Path::new(FIXTURE), &alf);

    let dest = tmp.path().join("restored");
    GenericAdapter.import(&alf, &dest).expect("import failed");

    // Every file the raw tree carries must restore byte-identically — including
    // the map, the identity file, the tracked config/notes, and the *binary*
    // SQLite database.
    for rel in [
        "memories/2026-07-04.md",
        "knowledge/rust.md",
        "knowledge/systems/deploy.md",
        "procedures/backup.md",
        "IDENTITY.md",
        ".alf-map.json",
        ".alf-include.json",
        "config.toml",
        "notes.txt",
        "data/brain.db",
    ] {
        let original = fs::read(Path::new(FIXTURE).join(rel)).unwrap();
        let restored =
            fs::read(dest.join(rel)).unwrap_or_else(|_| panic!("{rel} was not restored"));
        assert_eq!(original, restored, "byte mismatch after restore: {rel}");
    }

    // The SQLite header survived (binary fidelity, not eol-mangled).
    assert!(fs::read(dest.join("data/brain.db"))
        .unwrap()
        .starts_with(b"SQLite format 3\0"));
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn double_export_is_zero_delta() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a.alf");
    let b = tmp.path().join("b.alf");
    export(Path::new(FIXTURE), &a);
    export(Path::new(FIXTURE), &b);

    let delta = compute_delta(&read_memory(&a), &read_memory(&b));
    assert!(
        delta.is_empty(),
        "re-exporting unchanged input produced a delta"
    );
}

#[test]
fn restore_then_reexport_is_zero_delta_through_reconcile() {
    let tmp = tempfile::TempDir::new().unwrap();
    let orig = tmp.path().join("orig.alf");
    export(Path::new(FIXTURE), &orig);
    let base = read_memory(&orig);

    // Restore into a fresh workspace (this re-stamps file mtimes) and re-export.
    let dest = tmp.path().join("restored");
    GenericAdapter.import(&orig, &dest).expect("import failed");
    let re = tmp.path().join("re.alf");
    export(&dest, &re);
    let reexported = read_memory(&re);

    // Same count, same ids — the restore did not orphan or duplicate anything.
    assert_eq!(base.len(), reexported.len());

    // A raw compute_delta may see mtime-only churn; the production path runs
    // reconcile first, which carries the volatile timestamps forward.
    let outcome = reconcile(&base, reexported);
    assert_eq!(outcome.stats.created, 0, "no records should be created");
    assert_eq!(outcome.stats.deleted, 0, "no records should be deleted");
    let delta = compute_delta(&base, &outcome.records);
    assert!(
        delta.is_empty(),
        "restore→re-export→reconcile produced a delta: {delta:?}"
    );
}

// ---------------------------------------------------------------------------
// Reconcile edit scenarios
// ---------------------------------------------------------------------------

/// Records for a single-journal workspace pinned to a fixed agent id.
fn journal_records(journal: &str) -> Vec<MemoryRecord> {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = tmp.path();
    fs::write(ws.join(".alf-agent-id"), AGENT_ID).unwrap();
    fs::write(
        ws.join(".alf-map.json"),
        r#"{"version":1,"memory_sources":[
            {"id":"journal","glob":"memories/*.md","memory_type":"episodic",
             "namespace":"daily","chunking":"by_heading","timestamp":"filename_date",
             "tags":["hashtags"]}]}"#,
    )
    .unwrap();
    fs::create_dir_all(ws.join("memories")).unwrap();
    fs::write(ws.join("memories/2026-07-04.md"), journal).unwrap();

    let alf = ws.join("out.alf");
    export(ws, &alf);
    read_memory(&alf)
}

#[test]
fn inplace_body_edit_keeps_id_as_update() {
    let prev = journal_records("## Deploy\n\nRoot cause was the cache.\n");
    let curr = journal_records("## Deploy\n\nActually the lockfile hash.\n");
    assert_eq!(prev.len(), 1);
    assert_eq!(curr.len(), 1);
    // A body edit re-mints the birth id...
    assert_ne!(prev[0].id, curr[0].id, "body edit changes the birth id");

    // ...but reconcile matches the stable heading (P2) and carries the id.
    let outcome = reconcile(&prev, curr.clone());
    assert_eq!(
        outcome.stats.heading_matched, 1,
        "expected a P2 heading match"
    );
    assert_eq!(outcome.stats.created, 0);
    assert_eq!(outcome.stats.deleted, 0);
    assert_eq!(outcome.records[0].id, prev[0].id, "id carried forward");
    assert!(outcome.records[0].content.contains("lockfile hash"));

    // The resulting change is exactly one Update on the carried id.
    let delta = compute_delta(&prev, &outcome.records);
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].record.id, prev[0].id);
}

#[test]
fn heading_and_body_edit_is_create_plus_delete() {
    let prev = journal_records("## Deploy\n\nRoot cause was the cache.\n");
    let curr = journal_records("## Incident review\n\nA totally different note.\n");

    let outcome = reconcile(&prev, curr.clone());
    assert_eq!(outcome.stats.created, 1, "new heading+body is a P4 create");
    assert_eq!(outcome.stats.deleted, 1, "the old section is deleted");
    assert_eq!(outcome.stats.heading_matched, 0);
    assert_ne!(
        outcome.records[0].id, prev[0].id,
        "a new identity, not a carry"
    );
}

#[test]
fn agent_id_derivation_matches_readonly_resolver() {
    // The pinned fixture id resolves back through the adapter (sanity: exports
    // under this fixture are stamped with the committed id).
    let id = GenericAdapter.resolve_agent_id(Path::new(FIXTURE)).unwrap();
    assert_eq!(id, Uuid::parse_str(AGENT_ID).unwrap());
}
