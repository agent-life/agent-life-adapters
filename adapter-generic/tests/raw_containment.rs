#![cfg(unix)]

use adapter_generic::GenericAdapter;
use alf_core::Adapter;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use tempfile::TempDir;

const FIXTURE: &str = "tests/fixtures/toy";

fn isolate_home() {
    static HOME: OnceLock<TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", home.path());
        std::env::set_var("HOME", home.path());
        home
    });
}

fn archive_with_raw_entry(name: &str) -> (TempDir, std::path::PathBuf) {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("archive.alf");
    GenericAdapter.export(Path::new(FIXTURE), &archive).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&archive)
        .unwrap();
    let mut writer = zip::ZipWriter::new_append(file).unwrap();
    writer
        .start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"owned").unwrap();
    writer.finish().unwrap();
    (tmp, archive)
}

#[test]
fn raw_restore_rejects_symlinked_parent_and_final_target() {
    use std::os::unix::fs::symlink;

    let (_keep, archive) = archive_with_raw_entry("raw/generic/link/pwn.txt");
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&workspace).unwrap();
    symlink(&outside, workspace.join("link")).unwrap();

    assert!(GenericAdapter.import(&archive, &workspace).is_err());
    assert!(!outside.join("pwn.txt").exists());

    let (_keep, archive) = archive_with_raw_entry("raw/generic/target.txt");
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let sentinel = root.path().join("sentinel.txt");
    fs::write(&sentinel, b"keep").unwrap();
    symlink(&sentinel, workspace.join("target.txt")).unwrap();

    assert!(GenericAdapter.import(&archive, &workspace).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"keep");
}
