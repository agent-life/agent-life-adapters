use std::fs;
use std::path::Path;
use tempfile::TempDir;
use adapter_openclaw::OpenClawAdapter;
use alf_core::Adapter;

fn assert_files_match(src: &Path, dst: &Path, files: &[&str]) {
    for file in files {
        let src_path = src.join(file);
        let dst_path = dst.join(file);

        if src_path.exists() {
            assert!(dst_path.exists(), "File missing in destination: {}", file);
            let src_content = fs::read(&src_path).unwrap();
            let dst_content = fs::read(&dst_path).unwrap();
            assert_eq!(
                src_content, dst_content,
                "Content mismatch for file: {}",
                file
            );
        } else {
            assert!(!dst_path.exists(), "Unexpected file created in destination: {}", file);
        }
    }
}

const MINIMAL_FILES: &[&str] = &["SOUL.md"];

const STANDARD_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "MEMORY.md",
    "TOOLS.md",
    "HEARTBEAT.md",
    "memory/2026-01-15.md",
    "memory/2026-01-16.md",
    "memory/active-context.md",
    "memory/project-clawsmith.md",
    "memory/gating-policies.md",
];

const COMMUNITY_FILES: &[&str] = &[
    "SOUL.md",
    "memory/2026-02-01.md",
    "memory/active-context.md",
];

#[test]
fn round_trip_minimal_workspace() {
    let fixture = Path::new("tests/fixtures/minimal");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    let restored = tmp.path().join("restored");

    let adapter = OpenClawAdapter;
    let export_report = adapter.export(fixture, &alf_path).expect("export failed");
    let import_report = adapter.import(&alf_path, &restored).expect("import failed");

    assert_eq!(export_report.memory_records, import_report.memory_records);
    assert_files_match(fixture, &restored, MINIMAL_FILES);
}

#[test]
fn round_trip_standard_workspace() {
    let fixture = Path::new("tests/fixtures/standard");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    let restored = tmp.path().join("restored");

    let adapter = OpenClawAdapter;
    let export_report = adapter.export(fixture, &alf_path).expect("export failed");
    let import_report = adapter.import(&alf_path, &restored).expect("import failed");

    assert_eq!(export_report.memory_records, import_report.memory_records);
    assert_files_match(fixture, &restored, STANDARD_FILES);
}

#[test]
fn round_trip_community_patterns() {
    let fixture = Path::new("tests/fixtures/community-patterns");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    let restored = tmp.path().join("restored");

    let adapter = OpenClawAdapter;
    let export_report = adapter.export(fixture, &alf_path).expect("export failed");
    let import_report = adapter.import(&alf_path, &restored).expect("import failed");

    assert_eq!(export_report.memory_records, import_report.memory_records);
    assert_files_match(fixture, &restored, COMMUNITY_FILES);
}

#[test]
fn round_trip_empty_workspace() {
    let fixture = Path::new("tests/fixtures/empty");
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("export.alf");
    let restored = tmp.path().join("restored");

    let adapter = OpenClawAdapter;
    let export_report = adapter.export(fixture, &alf_path).expect("export failed");
    let import_report = adapter.import(&alf_path, &restored).expect("import failed");

    assert_eq!(export_report.memory_records, 0);
    assert_eq!(import_report.memory_records, 0);
}
