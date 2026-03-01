use std::fs;
use std::path::Path;
use tempfile::TempDir;
use adapter_openclaw::OpenClawAdapter;
use alf_core::Adapter;

#[test]
fn import_creates_directory_structure() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    
    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).unwrap();

    let restore_dir = tmp.path().join("restored_workspace");
    assert!(!restore_dir.exists());

    adapter.import(&alf_path, &restore_dir).unwrap();

    assert!(restore_dir.exists());
    assert!(restore_dir.join("memory").exists());
    assert!(restore_dir.join("SOUL.md").exists());
}

#[test]
fn import_writes_agent_id() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    
    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).unwrap();

    let restore_dir = tmp.path().join("restored_workspace");
    adapter.import(&alf_path, &restore_dir).unwrap();

    let id_file = restore_dir.join(".alf-agent-id");
    assert!(id_file.exists(), ".alf-agent-id was not written");
    
    // Also check it matches the original
    let orig_id_file = fixture.join(".alf-agent-id");
    if orig_id_file.exists() {
        let orig_id = fs::read_to_string(&orig_id_file).unwrap();
        let restored_id = fs::read_to_string(&id_file).unwrap();
        assert_eq!(orig_id.trim(), restored_id.trim());
    }
}

#[test]
fn import_overwrites_existing() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    
    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).unwrap();

    let restore_dir = tmp.path().join("restored_workspace");
    fs::create_dir_all(&restore_dir).unwrap();
    
    let target_file = restore_dir.join("SOUL.md");
    fs::write(&target_file, "OLD STUFF").unwrap();

    adapter.import(&alf_path, &restore_dir).unwrap();

    let new_content = fs::read_to_string(&target_file).unwrap();
    assert_ne!(new_content, "OLD STUFF");
    assert!(new_content.contains("Core Directives"));
}
