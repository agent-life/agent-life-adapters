//! Delta-shape pins for the OpenClaw adapter (WP4.1).
//!
//! `alf sync` reconciles the fresh export against the previous base before
//! diffing (`alf_core::reconcile`), so these tests exercise the same pipeline:
//! `compute_delta(old, reconcile(old, new).records)`. Assertions are exact —
//! a workspace edit must produce precisely the delta it means, with no
//! spurious sibling updates from mtime re-stamps and no id churn.

use adapter_openclaw::OpenClawAdapter;
use alf_core::delta::compute_delta;
use alf_core::manifest::DeltaOperation;
use alf_core::reconcile::reconcile;
use alf_core::Adapter;
use alf_core::AlfReader;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn get_records(workspace: &Path) -> Vec<alf_core::memory::MemoryRecord> {
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    OpenClawAdapter.export(workspace, &alf_path).unwrap();

    let file = fs::File::open(&alf_path).unwrap();
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader).unwrap();
    alf.read_all_memory().unwrap()
}

/// The sync pipeline's memory diff: reconcile identities against the base,
/// then compute the delta.
fn reconciled_delta(
    old: &[alf_core::memory::MemoryRecord],
    new: Vec<alf_core::memory::MemoryRecord>,
) -> Vec<alf_core::DeltaMemoryEntry> {
    compute_delta(old, &reconcile(old, new).records)
}

#[test]
fn no_changes_empty_delta() {
    let fixture = Path::new("tests/fixtures/standard");
    let old = get_records(fixture);
    let new = get_records(fixture);

    let delta = reconciled_delta(&old, new);
    assert!(
        delta.is_empty(),
        "Delta should be empty for identical workspaces"
    );
}

#[test]
fn new_section_detected() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "# Identity").unwrap();

    let mem_file = workspace.join("memory/2026-01-01.md");
    fs::write(&mem_file, "## Section 1\nContent 1\n").unwrap();

    let old = get_records(&workspace);

    // Add new section
    fs::write(
        &mem_file,
        "## Section 1\nContent 1\n\n## Section 2\nContent 2\n",
    )
    .unwrap();
    let new = get_records(&workspace);

    let delta = reconciled_delta(&old, new);
    // Exactly one entry: the sibling section's mtime re-stamp must NOT surface
    // as a spurious update.
    assert_eq!(delta.len(), 1, "delta: {delta:?}");
    assert_eq!(delta[0].operation, DeltaOperation::Create);
    assert!(delta[0].record.content.contains("Content 2"));
}

#[test]
fn modified_section_detected() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "# Identity").unwrap();

    let mem_file = workspace.join("memory/2026-01-01.md");
    fs::write(&mem_file, "## Section 1\nContent 1\n").unwrap();

    let old = get_records(&workspace);

    // Modify section body in place (the WP4.1 curation shape).
    fs::write(&mem_file, "## Section 1\nContent Modified\n").unwrap();
    let new = get_records(&workspace);

    let delta = reconciled_delta(&old, new);
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].operation, DeltaOperation::Update);
    assert!(delta[0].record.content.contains("Content Modified"));
    // Identity and partition anchor carried from the base record.
    assert_eq!(delta[0].record.id, old[0].id);
    assert_eq!(
        delta[0].record.temporal.created_at,
        old[0].temporal.created_at
    );
}

#[test]
fn deleted_section_detected() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "# Identity").unwrap();

    let mem_file = workspace.join("memory/2026-01-01.md");
    fs::write(
        &mem_file,
        "## Section 1\nContent 1\n\n## Section 2\nContent 2\n",
    )
    .unwrap();

    let old = get_records(&workspace);

    // Delete section
    fs::write(&mem_file, "## Section 1\nContent 1\n").unwrap();
    let new = get_records(&workspace);

    let delta = reconciled_delta(&old, new);
    assert_eq!(delta.len(), 1, "delta: {delta:?}");
    assert_eq!(delta[0].operation, DeltaOperation::Delete);
    assert!(delta[0].record.content.contains("Content 2"));
}

#[test]
fn reordered_sections_produce_empty_delta() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "# Identity").unwrap();

    let mem_file = workspace.join("MEMORY.md");
    fs::write(&mem_file, "## Alpha\nContent A\n\n## Beta\nContent B\n").unwrap();

    let old = get_records(&workspace);

    // Re-rank: the curation move that used to renumber every positional id.
    fs::write(&mem_file, "## Beta\nContent B\n\n## Alpha\nContent A\n").unwrap();
    let new = get_records(&workspace);

    let delta = reconciled_delta(&old, new);
    assert!(delta.is_empty(), "reorder is raw-only, delta: {delta:?}");
}

#[test]
fn renamed_file_produces_new_ids() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "# Identity").unwrap();

    let mem_file = workspace.join("memory/2026-01-01.md");
    fs::write(&mem_file, "## Section 1\nContent 1\n").unwrap();

    let old = get_records(&workspace);

    // Rename file
    let new_mem_file = workspace.join("memory/2026-01-02.md");
    fs::rename(&mem_file, &new_mem_file).unwrap();
    let new = get_records(&workspace);

    // Matching is scoped per origin_file, so a cross-file move is a
    // delete+create by design (lineage break documented in the WP4.1 design).
    let delta = reconciled_delta(&old, new);
    assert_eq!(delta.len(), 2);
    let creates = delta
        .iter()
        .filter(|d| d.operation == DeltaOperation::Create)
        .count();
    let deletes = delta
        .iter()
        .filter(|d| d.operation == DeltaOperation::Delete)
        .count();
    assert_eq!(creates, 1);
    assert_eq!(deletes, 1);
}
