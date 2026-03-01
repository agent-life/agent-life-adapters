use std::fs;
use std::path::Path;
use tempfile::TempDir;
use adapter_openclaw::OpenClawAdapter;
use alf_core::{Adapter, AlfReader, MemoryType};
use uuid::Uuid;

#[test]
fn manifest_metadata() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).expect("export failed");

    let file = std::fs::File::open(&alf_path).unwrap();
    let buf_reader = std::io::BufReader::new(file);
    let reader = AlfReader::new(buf_reader).expect("open failed");
    let manifest = reader.manifest().clone();

    assert_eq!(manifest.agent.source_runtime, "openclaw");
    assert_eq!(manifest.agent.name, "Core Directives");
}

#[test]
fn memory_record_classification() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).expect("export failed");

    let file = std::fs::File::open(&alf_path).unwrap();
    let buf_reader = std::io::BufReader::new(file);
    let mut reader = AlfReader::new(buf_reader).expect("open failed");
    let records = reader.read_all_memory().expect("read memory failed");

    // Check classification based on our standard workspace files
    let mut has_daily = false;
    let mut has_semantic = false;
    let mut has_procedural = false;
    let mut has_project = false;

    for r in records {
        match r.namespace.as_str() {
            "daily" => {
                has_daily = true;
                assert_eq!(r.memory_type, MemoryType::Episodic);
                assert!(r.temporal.observed_at.is_some(), "Daily log should have observed_at");
            }
            "curated" => {
                has_semantic = true;
                assert_eq!(r.memory_type, MemoryType::Semantic);
            }
            "procedural" => {
                has_procedural = true;
                assert_eq!(r.memory_type, MemoryType::Procedural);
            }
            "project" => {
                has_project = true;
                assert_eq!(r.memory_type, MemoryType::Semantic);
            }
            _ => {}
        }
    }

    assert!(has_daily, "Should have daily records");
    assert!(has_semantic, "Should have semantic records");
    assert!(has_procedural, "Should have procedural records");
    assert!(has_project, "Should have project records");
}

#[test]
fn identity_and_principals_populated() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).expect("export failed");

    let file = std::fs::File::open(&alf_path).unwrap();
    let buf_reader = std::io::BufReader::new(file);
    let mut reader = AlfReader::new(buf_reader).expect("open failed");
    
    let identity = reader.read_identity().expect("read identity failed").expect("missing identity");
    assert!(identity.prose.is_some());
    let prose = identity.prose.unwrap();
    assert!(prose.soul.is_some(), "SOUL.md should map to soul");
    assert!(prose.identity_profile.is_some(), "IDENTITY.md should map to identity_profile");

    let principals = reader.read_principals().expect("read principals failed").expect("missing principals");
    let profile = &principals.principals[0].profile;
    assert!(profile.prose.is_some());
    let p_prose = profile.prose.as_ref().unwrap();
    assert!(p_prose.user_profile.is_some(), "USER.md should map to user_profile");
}

#[test]
fn raw_sources_included() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).expect("export failed");

    let file = std::fs::File::open(&alf_path).unwrap();
    let buf_reader = std::io::BufReader::new(file);
    let reader = AlfReader::new(buf_reader).expect("open failed");
    
    // Check if raw/openclaw/ files are correctly saved
    let raw_files = reader.file_names();
    assert!(raw_files.contains(&"raw/openclaw/SOUL.md".to_string()));
    assert!(raw_files.contains(&"raw/openclaw/MEMORY.md".to_string()));
    assert!(raw_files.contains(&"raw/openclaw/memory/2026-01-15.md".to_string()));
}

#[test]
fn agent_id_persisted() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Agent").unwrap();

    let alf_path = tmp.path().join("export.alf");

    let adapter = OpenClawAdapter;
    adapter.export(&workspace, &alf_path).expect("export failed");

    let id_file = workspace.join(".alf-agent-id");
    assert!(id_file.exists(), ".alf-agent-id should be created");

    let id_str = fs::read_to_string(&id_file).unwrap();
    let uuid = Uuid::parse_str(id_str.trim()).expect("Valid UUID");

    let file = std::fs::File::open(&alf_path).unwrap();
    let buf_reader = std::io::BufReader::new(file);
    let reader = AlfReader::new(buf_reader).expect("open failed");
    let manifest = reader.manifest();
    assert_eq!(manifest.agent.id, uuid, "Manifest UUID should match generated UUID");
}

#[test]
fn importance_tags_extracted() {
    let fixture = Path::new("tests/fixtures/community-patterns");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");

    let adapter = OpenClawAdapter;
    adapter.export(fixture, &alf_path).expect("export failed");

    let file = std::fs::File::open(&alf_path).unwrap();
    let buf_reader = std::io::BufReader::new(file);
    let mut reader = AlfReader::new(buf_reader).expect("open failed");
    let records = reader.read_all_memory().expect("read memory failed");

    // "memory/2026-02-01.md" has "## Decision [decision|i=0.9]"
    let mut found_decision = false;
    for r in records {
        if r.tags.contains(&"decision".to_string()) {
            found_decision = true;
            assert_eq!(r.confidence, Some(0.9));
        }
    }
    assert!(found_decision, "Should have extracted the decision tag and confidence");
}
