//! WP3: arbitrary workspace files tracked via the include list (`alf add`)
//! round-trip byte-identically through export → import.

use adapter_openclaw::{IncludeList, OpenClawAdapter, INCLUDE_FILE};
use alf_core::Adapter;
use std::fs;
use tempfile::TempDir;

mod common;

#[test]
fn tracked_files_round_trip_byte_identical() {
    common::isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(ws.join("my-project")).unwrap();
    fs::write(ws.join("SOUL.md"), "# Bot\n\nSoul.").unwrap();
    fs::write(ws.join("notes.txt"), "free-form agent notes\n").unwrap();
    // A binary file with NUL + non-UTF8 bytes.
    let binary: Vec<u8> = vec![0u8, 159, 146, 150, 255, 1, 2, 3];
    fs::write(ws.join("my-project/blob.bin"), &binary).unwrap();

    // Opt the arbitrary files in.
    let mut list = IncludeList::default();
    list.add("notes.txt");
    list.add("my-project/blob.bin");
    list.save(&ws).unwrap();

    let adapter = OpenClawAdapter;
    let alf_path = tmp.path().join("export.alf");
    adapter.export(&ws, &alf_path).expect("export failed");

    let restored = tmp.path().join("restored");
    adapter.import(&alf_path, &restored).expect("import failed");

    // Tracked text file restored byte-identical.
    assert_eq!(
        fs::read(restored.join("notes.txt")).unwrap(),
        b"free-form agent notes\n"
    );
    // Tracked binary file restored byte-identical, nested dir preserved.
    assert_eq!(fs::read(restored.join("my-project/blob.bin")).unwrap(), binary);
    // The include list travels so the agent keeps tracking on machine B.
    assert!(restored.join(INCLUDE_FILE).is_file());
    let reloaded = IncludeList::load(&restored).unwrap();
    assert_eq!(reloaded.paths(), vec!["my-project/blob.bin", "notes.txt"]);
}

#[test]
fn untracked_files_are_not_synced() {
    common::isolate_home();
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("SOUL.md"), "# Bot\n\nSoul.").unwrap();
    fs::write(ws.join("secret.txt"), "not opted in").unwrap();

    let adapter = OpenClawAdapter;
    let alf_path = tmp.path().join("export.alf");
    adapter.export(&ws, &alf_path).expect("export failed");

    let restored = tmp.path().join("restored");
    adapter.import(&alf_path, &restored).expect("import failed");

    assert!(restored.join("SOUL.md").is_file());
    assert!(
        !restored.join("secret.txt").exists(),
        "files not opted in via `alf add` must not be synced"
    );
}
