#![cfg(unix)]

use adapter_hermes::HermesAdapter;
use alf_core::Adapter;
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

fn archive_with_raw_entry(name: &str) -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("SOUL.md"), "# Hermes\n").unwrap();
    fs::create_dir(home.join("memories")).unwrap();
    fs::write(home.join("memories/MEMORY.md"), "safe memory\n").unwrap();
    let archive = tmp.path().join("archive.alf");
    HermesAdapter.export(&home, &archive).unwrap();
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
    let (_keep, archive) = archive_with_raw_entry("raw/hermes/link/pwn.txt");
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&home).unwrap();
    symlink(&outside, home.join("link")).unwrap();

    assert!(HermesAdapter.import(&archive, &home).is_err());
    assert!(!outside.join("pwn.txt").exists());

    let (_keep, archive) = archive_with_raw_entry("raw/hermes/target.txt");
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let sentinel = root.path().join("sentinel.txt");
    fs::write(&sentinel, b"keep").unwrap();
    symlink(&sentinel, home.join("target.txt")).unwrap();

    assert!(HermesAdapter.import(&archive, &home).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"keep");
}
