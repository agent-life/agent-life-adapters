//! Dry-run enumeration integration tests (IN-1, IN-3).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use adapter_openclaw::OpenClawAdapter;
use alf_core::{Adapter, AlfReader};
use tempfile::TempDir;

fn write(ws: &Path, name: &str, content: &str) {
    let path = ws.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn sample_workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let ws = dir.path();
    write(ws, "SOUL.md", "# Atlas\n\nA helpful agent.");
    write(ws, "IDENTITY.md", "# Identity\n\n- **Name:** Atlas");
    write(ws, "MEMORY.md", "## Facts\n\nThe sky is blue.");
    write(ws, "memory/2026-01-15.md", "## Morning\n\nDid stuff.");
    write(ws, "memory/2026-01-16.md", "## Day\n\nMore stuff.");
    dir
}

/// Raw-source paths inside an archive, stripped of the `raw/openclaw/` prefix.
fn archive_raw_paths(alf: &Path) -> BTreeSet<String> {
    let reader = AlfReader::new(std::io::BufReader::new(fs::File::open(alf).unwrap())).unwrap();
    let prefix = "raw/openclaw/";
    reader
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with(prefix) && n.len() > prefix.len())
        .map(|n| n[prefix.len()..].to_string())
        .collect()
}

/// IN-1: `export --dry-run` file list equals the real archive's raw file set.
#[test]
fn in1_dry_run_files_match_real_export() {
    let dir = sample_workspace();
    let ws = dir.path();
    let adapter = OpenClawAdapter;

    let preview = adapter.enumerate_workspace(ws).unwrap();
    let preview_paths: BTreeSet<String> = preview.files.iter().map(|f| f.path.clone()).collect();

    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("export.alf");
    adapter.export(ws, &out).unwrap();

    assert_eq!(preview_paths, archive_raw_paths(&out));
}

/// IN-2 (adapter half): `enumerate_workspace` writes no archive and does not
/// persist `.alf-agent-id` — it is strictly read-only.
#[test]
fn in2_dry_run_writes_nothing() {
    let dir = sample_workspace();
    let ws = dir.path();

    OpenClawAdapter.enumerate_workspace(ws).unwrap();

    assert!(!ws.join(".alf-agent-id").exists());
    let alf_files: Vec<_> = fs::read_dir(ws)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "alf"))
        .collect();
    assert!(alf_files.is_empty());
}

/// IN-3: `restore --dry-run` lists every `raw/openclaw/` entry in an archive.
#[test]
fn in3_enumerate_archive_lists_raw_entries() {
    let dir = sample_workspace();
    let ws = dir.path();
    let adapter = OpenClawAdapter;

    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("archive.alf");
    adapter.export(ws, &out).unwrap();

    let enumeration = adapter.enumerate_archive(&out).unwrap();
    let enumerated: BTreeSet<String> = enumeration.files.iter().map(|f| f.path.clone()).collect();

    assert_eq!(enumerated, archive_raw_paths(&out));
    // Sizes come from the archive central directory — non-zero for real files.
    assert!(enumeration.files.iter().all(|f| f.size > 0));
}

/// `.alfignore` is honored identically by `export` and `export --dry-run`:
/// an excluded file is absent from both the preview and the real archive.
#[test]
fn alfignore_applies_to_dry_run_and_real_export() {
    let dir = sample_workspace();
    let ws = dir.path();
    write(ws, ".alfignore", "memory/2026-01-15.md\n");
    let adapter = OpenClawAdapter;

    let preview = adapter.enumerate_workspace(ws).unwrap();
    assert_eq!(preview.excluded_by_alfignore, 1);
    assert!(!preview
        .files
        .iter()
        .any(|f| f.path == "memory/2026-01-15.md"));

    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("export.alf");
    adapter.export(ws, &out).unwrap();
    assert!(!archive_raw_paths(&out).contains("memory/2026-01-15.md"));
}
