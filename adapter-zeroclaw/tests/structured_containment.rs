use adapter_zeroclaw::ZeroClawAdapter;
use alf_core::Adapter;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use tempfile::TempDir;

mod common;

fn build_cross_runtime_archive(out_path: &Path, origin_file: &str) {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("config.toml"),
        "[memory]\nbackend = \"markdown\"\n",
    )
    .unwrap();
    fs::write(
        source.join("SOUL.md"),
        "# Identity\nStructured safety test.",
    )
    .unwrap();
    fs::create_dir(source.join("memory")).unwrap();
    fs::write(source.join("memory/2026-07-30.md"), "A daily log entry.").unwrap();

    let source_archive = tmp.path().join("source.alf");
    ZeroClawAdapter.export(&source, &source_archive).unwrap();

    let input = fs::File::open(source_archive).unwrap();
    let mut zip_in = zip::ZipArchive::new(input).unwrap();
    let output = fs::File::create(out_path).unwrap();
    let mut zip_out = zip::ZipWriter::new(output);
    let options = zip::write::SimpleFileOptions::default();
    for index in 0..zip_in.len() {
        let mut entry = zip_in.by_index(index).unwrap();
        let name = entry.name().to_string();
        if name.starts_with("raw/") {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if name.starts_with("memory/") && name.ends_with(".jsonl") {
            let mut rewritten = String::new();
            for line in String::from_utf8(bytes).unwrap().lines() {
                let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                record["source"]["origin_file"] = serde_json::Value::String(origin_file.into());
                rewritten.push_str(&serde_json::to_string(&record).unwrap());
                rewritten.push('\n');
            }
            bytes = rewritten.into_bytes();
        }
        zip_out.start_file(name, options).unwrap();
        zip_out.write_all(&bytes).unwrap();
    }
    zip_out.finish().unwrap();
}

#[test]
fn cross_import_rejects_unsafe_origin_file_without_mutation() {
    common::isolate_home();
    for origin_file in [
        "../../outside.md",
        "/absolute/outside.md",
        "C:/outside.md",
        "C:outside.md",
        "..\\..\\outside.md",
    ] {
        let tmp = TempDir::new().unwrap();
        let archive = tmp.path().join("foreign.alf");
        build_cross_runtime_archive(&archive, origin_file);

        let workspace = tmp.path().join("inside/workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("sentinel.txt"), b"workspace stays unchanged").unwrap();
        let outside = tmp.path().join("outside.md");
        fs::write(&outside, b"outside stays unchanged").unwrap();

        let error = ZeroClawAdapter.import(&archive, &workspace).unwrap_err();
        assert!(
            format!("{error:#}").contains("unsafe structured"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&outside).unwrap(),
            b"outside stays unchanged",
            "{origin_file}"
        );
        let names: Vec<_> = fs::read_dir(&workspace)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            [std::ffi::OsString::from("sentinel.txt")],
            "{origin_file}"
        );
    }
}

#[test]
fn cross_import_accepts_safe_origin_file() {
    common::isolate_home();
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("foreign.alf");
    build_cross_runtime_archive(&archive, "memory/2026-07-30.md");
    let workspace = tmp.path().join("workspace");

    ZeroClawAdapter.import(&archive, &workspace).unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("memory/2026-07-30.md")).unwrap(),
        "A daily log entry."
    );
}
