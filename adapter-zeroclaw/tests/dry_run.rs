//! Dry-run enumeration parity: `export --dry-run` and `restore --dry-run`
//! preview the files that a real run would touch, without writing anything.

use adapter_zeroclaw::ZeroClawAdapter;
use alf_core::Adapter;
use tempfile::TempDir;

mod common;

#[test]
fn enumerate_workspace_lists_root_files() {
    common::isolate_home();
    let root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(root.path());

    let adapter = ZeroClawAdapter;
    let enumeration = adapter
        .enumerate_workspace(&workspace)
        .expect("enumerate_workspace failed");

    let paths: Vec<&str> = enumeration.files.iter().map(|f| f.path.as_str()).collect();
    for expected in ["config.toml", "SOUL.md", "IDENTITY.md", "AGENTS.md"] {
        assert!(
            paths.contains(&expected),
            "dry-run file list should include {expected}; got {paths:?}"
        );
    }
    // Dry-run must not create an archive or mutate the workspace.
    assert!(
        !workspace.join(".alf-agent-id").exists(),
        "enumerate_workspace must not persist .alf-agent-id"
    );
}

#[test]
fn enumerate_archive_matches_export() {
    common::isolate_home();
    let root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(root.path());

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("export.alf");

    let adapter = ZeroClawAdapter;
    adapter
        .export(&workspace, &alf_path)
        .expect("export failed");

    let enumeration = adapter
        .enumerate_archive(&alf_path)
        .expect("enumerate_archive failed");
    let paths: Vec<&str> = enumeration.files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"SOUL.md"),
        "archive enumeration should list raw SOUL.md; got {paths:?}"
    );
}
