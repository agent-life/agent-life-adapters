//! `alf add` tracked-file parity for ZeroClaw.
//!
//! The include-list feature (`.alf-include.json`) is runtime-agnostic
//! (alf_core::include) and the ZeroClaw adapter's `export` packs the tracked
//! files plus the sentinels under `raw/zeroclaw/`, exactly as OpenClaw does
//! under `raw/openclaw/`. These tests assert that parity end to end.

use std::fs;
use std::io::BufReader;

use adapter_zeroclaw::ZeroClawAdapter;
use alf_core::{Adapter, AlfReader, IncludeList};
use tempfile::TempDir;

mod common;

#[test]
fn export_packs_tracked_file_and_sentinels() {
    common::isolate_home();

    let root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(root.path());

    // The agent tracks an arbitrary file (what `alf add report.md` records).
    fs::write(workspace.join("report.md"), "# Q1\n\nNumbers up.\n").unwrap();
    let mut list = IncludeList::default();
    assert!(list.add("report.md"));
    list.save(&workspace).unwrap();

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("export.alf");
    ZeroClawAdapter
        .export(&workspace, &alf_path)
        .expect("export failed");

    let reader = AlfReader::new(BufReader::new(fs::File::open(&alf_path).unwrap())).unwrap();
    let names = reader.file_names();
    assert!(
        names.contains(&"raw/zeroclaw/report.md".to_string()),
        "tracked file must be packed under raw/zeroclaw/; got {names:?}"
    );
    assert!(
        names.contains(&"raw/zeroclaw/.alf-include.json".to_string()),
        "include list itself must travel so it round-trips on restore"
    );
}

#[test]
fn export_reports_missing_tracked_file() {
    common::isolate_home();

    let root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(root.path());

    // Track a file that was never created on disk — export must surface it in
    // `missing_includes` (what `alf sync` then prunes + logs).
    let mut list = IncludeList::default();
    list.add("gone.txt");
    list.save(&workspace).unwrap();

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("export.alf");
    let report = ZeroClawAdapter
        .export(&workspace, &alf_path)
        .expect("export failed");

    assert_eq!(
        report.missing_includes,
        vec!["gone.txt".to_string()],
        "a tracked-but-absent file must be reported, not silently dropped"
    );
}

#[test]
fn tracked_file_round_trips_through_import() {
    common::isolate_home();

    let root = TempDir::new().unwrap();
    let workspace = common::make_markdown_home(root.path());
    fs::write(workspace.join("notes.txt"), "remember this\n").unwrap();
    let mut list = IncludeList::default();
    list.add("notes.txt");
    list.save(&workspace).unwrap();

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("export.alf");
    ZeroClawAdapter
        .export(&workspace, &alf_path)
        .expect("export failed");

    let restored_home = TempDir::new().unwrap();
    let restored = restored_home.path().join("workspace");
    ZeroClawAdapter
        .import(&alf_path, &restored)
        .expect("import failed");

    let restored_notes = restored.join("notes.txt");
    assert!(restored_notes.is_file(), "tracked file must restore");
    assert_eq!(
        fs::read_to_string(&restored_notes).unwrap(),
        "remember this\n",
        "tracked file content must round-trip byte-for-byte"
    );
}
