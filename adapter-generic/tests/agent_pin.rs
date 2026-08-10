//! RF-017 — the generic import agent-pin guard must fail closed.
//!
//! `.alf-agent-id` is the guard that stops one agent's history being overlaid on
//! another workspace. A malformed, unreadable, or non-regular pin must be an
//! error *before any workspace write*, never silently collapse to "absent" and
//! let the overlay rebind the workspace to the archive's agent behind a fresh,
//! valid pin (which would also erase the evidence that verification failed).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use adapter_generic::GenericAdapter;
use alf_core::{Adapter, AGENT_ID_FILE};
use tempfile::TempDir;

const AGENT_A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const AGENT_B: &str = "bbbbbbbb-0000-4000-8000-000000000002";

/// Isolate ALF_HOME/HOME process-wide so export's vault read and import's vault
/// write never touch the developer's real `~/.alf`.
fn isolate_home() {
    static HOME: OnceLock<TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", dir.path());
        std::env::set_var("HOME", dir.path());
        dir
    });
}

/// Build a generic archive owned by `agent_id`, carrying `notes.md = historical`
/// and `new.md`. Returns the keep-alive tempdir plus the archive path.
fn historical_archive(agent_id: &str) -> (TempDir, PathBuf) {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join(".alf-map.json"),
        r#"{"version":1,"memory_sources":[
            {"id":"notes","glob":"*.md","memory_type":"semantic",
             "namespace":"curated","chunking":"per_file"}]}"#,
    )
    .unwrap();
    fs::write(src.join(AGENT_ID_FILE), agent_id).unwrap();
    fs::write(src.join("notes.md"), "historical").unwrap();
    fs::write(src.join("new.md"), "brand new").unwrap();
    let archive = tmp.path().join("archive.alf");
    GenericAdapter
        .export(&src, &archive)
        .expect("export failed");
    (tmp, archive)
}

/// A fresh import target seeded with `notes.md = current` and no pin.
fn seed_workspace() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("notes.md"), "current").unwrap();
    (tmp, ws)
}

/// A refused import must not have overlaid anything: the seeded file keeps its
/// bytes and no archive-only file was created.
fn assert_workspace_untouched(ws: &Path) {
    assert_eq!(
        fs::read_to_string(ws.join("notes.md")).unwrap(),
        "current",
        "seeded file must be byte-identical after a refused import"
    );
    assert!(
        !ws.join("new.md").exists(),
        "archive-only file must not be created on refusal"
    );
}

#[test]
fn agent_pin_malformed_fails_closed_without_overlay() {
    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    fs::write(ws.join(AGENT_ID_FILE), "not-a-uuid\n").unwrap();

    let err = GenericAdapter.import(&archive, &ws).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains(AGENT_ID_FILE),
        "error must name the pin path: {msg}"
    );

    // The corrupted guard is preserved, not overwritten with a fresh valid pin —
    // the evidence that verification failed survives.
    assert_eq!(
        fs::read_to_string(ws.join(AGENT_ID_FILE)).unwrap(),
        "not-a-uuid\n",
        "malformed pin bytes must be left intact"
    );
    assert_workspace_untouched(&ws);
}

#[test]
fn agent_pin_matching_overlays_and_keeps_pin() {
    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    fs::write(ws.join(AGENT_ID_FILE), AGENT_A).unwrap();

    GenericAdapter
        .import(&archive, &ws)
        .expect("a matching pin must import");

    assert_eq!(
        fs::read_to_string(ws.join("notes.md")).unwrap(),
        "historical"
    );
    assert_eq!(fs::read_to_string(ws.join("new.md")).unwrap(), "brand new");
    assert_eq!(
        fs::read_to_string(ws.join(AGENT_ID_FILE)).unwrap().trim(),
        AGENT_A
    );
}

#[test]
fn agent_pin_mismatched_valid_reports_drift() {
    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    fs::write(ws.join(AGENT_ID_FILE), AGENT_B).unwrap();

    let err = GenericAdapter.import(&archive, &ws).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("drift"),
        "expected the agent-drift message: {msg}"
    );
    assert!(
        msg.contains(AGENT_A) && msg.contains(AGENT_B),
        "drift error must name both ids: {msg}"
    );

    assert_eq!(
        fs::read_to_string(ws.join(AGENT_ID_FILE)).unwrap().trim(),
        AGENT_B,
        "the existing pin must be left intact"
    );
    assert_workspace_untouched(&ws);
}

#[test]
fn agent_pin_missing_overlays_and_writes_pin() {
    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    assert!(!ws.join(AGENT_ID_FILE).exists());

    GenericAdapter
        .import(&archive, &ws)
        .expect("a genuinely absent pin must import");

    assert_eq!(
        fs::read_to_string(ws.join("notes.md")).unwrap(),
        "historical"
    );
    assert!(ws.join("new.md").exists());
    assert_eq!(
        fs::read_to_string(ws.join(AGENT_ID_FILE)).unwrap().trim(),
        AGENT_A,
        "a missing pin is created from the archive after a successful overlay"
    );
}

#[test]
fn agent_pin_directory_fails_closed() {
    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    fs::create_dir(ws.join(AGENT_ID_FILE)).unwrap();

    let err = GenericAdapter.import(&archive, &ws).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains(AGENT_ID_FILE),
        "error must name the pin path: {msg}"
    );
    assert!(
        ws.join(AGENT_ID_FILE).is_dir(),
        "a directory pin must be left intact, not auto-repaired"
    );
    assert_workspace_untouched(&ws);
}

#[test]
#[cfg(unix)]
fn agent_pin_unreadable_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    let pin = ws.join(AGENT_ID_FILE);
    fs::write(&pin, AGENT_A).unwrap();
    fs::set_permissions(&pin, fs::Permissions::from_mode(0o000)).unwrap();

    // chmod 000 does not stop root; if the file is still readable we are running
    // as root and the permission arm cannot be exercised — restore mode and bow out.
    if fs::read_to_string(&pin).is_ok() {
        fs::set_permissions(&pin, fs::Permissions::from_mode(0o600)).unwrap();
        eprintln!("skipping unreadable_pin_fails_closed: process can read 0o000 (root)");
        return;
    }

    let err = GenericAdapter.import(&archive, &ws).unwrap_err();
    fs::set_permissions(&pin, fs::Permissions::from_mode(0o600)).unwrap();
    let msg = format!("{err:#}");
    assert!(
        msg.contains(AGENT_ID_FILE),
        "error must name the pin path: {msg}"
    );
    assert_workspace_untouched(&ws);
}

#[test]
#[cfg(unix)]
fn agent_pin_dangling_symlink_fails_closed() {
    use std::os::unix::fs::symlink;

    let (_a, archive) = historical_archive(AGENT_A);
    let (_w, ws) = seed_workspace();
    symlink(ws.join("does-not-exist"), ws.join(AGENT_ID_FILE)).unwrap();

    let err = GenericAdapter.import(&archive, &ws).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains(AGENT_ID_FILE),
        "error must name the pin path: {msg}"
    );
    // The symlink is neither followed nor written through.
    assert!(
        fs::symlink_metadata(ws.join(AGENT_ID_FILE))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the dangling symlink must be left intact"
    );
    assert_workspace_untouched(&ws);
}

/// A restore-helper failure *after* a genuinely absent pin passes the guard must
/// still leave the workspace unpinned: the pin write only happens after a
/// successful overlay.
#[test]
#[cfg(unix)]
fn agent_pin_restore_failure_after_missing_leaves_pin_absent() {
    use std::io::Write;
    use std::os::unix::fs::symlink;

    // Archive owned by A plus a raw entry that escapes containment through a
    // pre-existing workspace symlink (the RF-006 raw-restore refusal).
    let (_a, archive) = historical_archive(AGENT_A);
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&archive)
            .unwrap();
        let mut writer = zip::ZipWriter::new_append(file).unwrap();
        writer
            .start_file(
                "raw/generic/link/pwn.txt",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"owned").unwrap();
        writer.finish().unwrap();
    }

    let root = TempDir::new().unwrap();
    let ws = root.path().join("ws");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&ws).unwrap();
    symlink(&outside, ws.join("link")).unwrap();
    assert!(!ws.join(AGENT_ID_FILE).exists());

    assert!(
        GenericAdapter.import(&archive, &ws).is_err(),
        "the containment violation must fail the import"
    );
    assert!(
        !ws.join(AGENT_ID_FILE).exists(),
        "the pin write is after a successful overlay; a mid-restore failure must \
         leave the workspace unpinned"
    );
    assert!(!outside.join("pwn.txt").exists());
}
