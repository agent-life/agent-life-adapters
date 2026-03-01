use std::fs;
use std::path::Path;
use tempfile::TempDir;
use adapter_openclaw::OpenClawAdapter;
use alf_core::Adapter;
use alf_core::AlfReader;
use alf_core::delta::compute_delta;
use alf_core::manifest::DeltaOperation;

fn get_records(workspace: &Path) -> Vec<alf_core::memory::MemoryRecord> {
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    OpenClawAdapter.export(workspace, &alf_path).unwrap();

    let file = fs::File::open(&alf_path).unwrap();
    let reader = std::io::BufReader::new(file);
    let mut alf = AlfReader::new(reader).unwrap();
    alf.read_all_memory().unwrap()
}

#[test]
fn no_changes_empty_delta() {
    let fixture = Path::new("tests/fixtures/standard");
    let old = get_records(fixture);
    let new = get_records(fixture);

    let delta = compute_delta(&old, &new);
    assert!(delta.is_empty(), "Delta should be empty for identical workspaces");
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
    fs::write(&mem_file, "## Section 1\nContent 1\n\n## Section 2\nContent 2\n").unwrap();
    let new = get_records(&workspace);

    let delta = compute_delta(&old, &new);
    let creates: Vec<_> = delta.iter().filter(|d| d.operation == DeltaOperation::Create).collect();
    assert_eq!(creates.len(), 1);
    assert!(creates[0].record.content.contains("Content 2"));
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

    // Modify section
    fs::write(&mem_file, "## Section 1\nContent Modified\n").unwrap();
    let new = get_records(&workspace);

    let delta = compute_delta(&old, &new);
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].operation, DeltaOperation::Update);
    assert!(delta[0].record.content.contains("Content Modified"));
}

#[test]
fn deleted_section_detected() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "# Identity").unwrap();
    
    let mem_file = workspace.join("memory/2026-01-01.md");
    fs::write(&mem_file, "## Section 1\nContent 1\n\n## Section 2\nContent 2\n").unwrap();
    
    let old = get_records(&workspace);

    // Delete section
    fs::write(&mem_file, "## Section 1\nContent 1\n").unwrap();
    let new = get_records(&workspace);

    let delta = compute_delta(&old, &new);
    let deletes: Vec<_> = delta.iter().filter(|d| d.operation == DeltaOperation::Delete).collect();
    assert_eq!(deletes.len(), 1);
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

    let delta = compute_delta(&old, &new);
    // Renaming a file changes its deterministic ID base, so it should delete the old and create the new.
    assert_eq!(delta.len(), 2);
    let creates = delta.iter().filter(|d| d.operation == DeltaOperation::Create).count();
    let deletes = delta.iter().filter(|d| d.operation == DeltaOperation::Delete).count();
    assert_eq!(creates, 1);
    assert_eq!(deletes, 1);
}
