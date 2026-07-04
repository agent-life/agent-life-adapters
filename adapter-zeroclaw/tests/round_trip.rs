//! ZeroClaw export → import round-trip fidelity.
//!
//! Mirrors `adapter-openclaw/tests/round_trip.rs`: export a workspace to an
//! `.alf` archive, import it into a fresh workspace, and assert the raw
//! structural files survive byte-for-byte. ZeroClaw preserves the root
//! Markdown files (`SOUL.md`, …) and a redacted `config.toml` as raw sources;
//! SQLite memory is captured structurally (covered in `export_correctness`).

use std::fs;
use std::path::Path;

use adapter_zeroclaw::ZeroClawAdapter;
use alf_core::Adapter;
use tempfile::TempDir;

mod common;

fn assert_files_match(src: &Path, dst: &Path, files: &[&str]) {
    for file in files {
        let src_path = src.join(file);
        let dst_path = dst.join(file);
        assert!(src_path.exists(), "fixture missing source file: {file}");
        assert!(dst_path.exists(), "file missing in destination: {file}");
        assert_eq!(
            fs::read(&src_path).unwrap(),
            fs::read(&dst_path).unwrap(),
            "content mismatch for file: {file}"
        );
    }
}

const ROOT_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "TOOLS.md",
    "HEARTBEAT.md",
];

#[test]
fn round_trip_markdown_workspace() {
    common::isolate_home();

    let src_root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(src_root.path());

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("export.alf");
    // restored workspace lives under its own home dir so import's config.toml
    // placement (workspace.parent()) has somewhere to go.
    let restored_home = TempDir::new().unwrap();
    let restored = restored_home.path().join("workspace");

    let adapter = ZeroClawAdapter;
    let export_report = adapter
        .export(&workspace, &alf_path)
        .expect("export failed");
    let import_report = adapter.import(&alf_path, &restored).expect("import failed");

    assert_eq!(
        export_report.memory_records, import_report.memory_records,
        "memory record count should round-trip"
    );
    assert_files_match(&workspace, &restored, ROOT_FILES);
}

#[test]
fn round_trip_preserves_redacted_config() {
    common::isolate_home();

    let src_root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(src_root.path());

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("export.alf");
    let restored_home = TempDir::new().unwrap();
    let restored = restored_home.path().join("workspace");

    let adapter = ZeroClawAdapter;
    adapter
        .export(&workspace, &alf_path)
        .expect("export failed");
    adapter.import(&alf_path, &restored).expect("import failed");

    // Flat layout: raw restore writes config.toml to the install root (which,
    // for a fresh target with no existing config.toml, is the given workspace).
    let restored_config = restored.join("config.toml");
    assert!(
        restored_config.is_file(),
        "config.toml should be restored to the install root"
    );
    let body = fs::read_to_string(&restored_config).unwrap();
    assert!(
        body.contains("backend = \"markdown\""),
        "restored config should retain non-secret fields"
    );
}
