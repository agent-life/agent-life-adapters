use std::fs;
use std::path::Path;
use std::io::{Read, Write};
use tempfile::TempDir;
use adapter_openclaw::OpenClawAdapter;
use alf_core::Adapter;

fn build_cross_runtime_archive(out_path: &Path) {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();

    fs::write(workspace.join("SOUL.md"), "# Identity\nI am a cross-runtime agent.").unwrap();
    fs::write(workspace.join("IDENTITY.md"), "This is my profile.").unwrap();
    fs::write(workspace.join("USER.md"), "This is the cross user.").unwrap();
    fs::create_dir(workspace.join("memory")).unwrap();
    fs::write(workspace.join("memory").join("2026-01-01.md"), "A daily log entry.").unwrap();

    let temp_alf = tmp.path().join("temp.alf");
    let adapter = OpenClawAdapter;
    adapter.export(&workspace, &temp_alf).unwrap();

    // Now copy everything except raw/openclaw/ to out_path
    let in_file = fs::File::open(&temp_alf).unwrap();
    let mut zip_in = zip::ZipArchive::new(in_file).unwrap();

    let out_file = fs::File::create(out_path).unwrap();
    let mut zip_out = zip::ZipWriter::new(out_file);
    let options = zip::write::SimpleFileOptions::default();

    for i in 0..zip_in.len() {
        let mut file = zip_in.by_index(i).unwrap();
        let name = file.name().to_string();
        if !name.starts_with("raw/") {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            zip_out.start_file(name, options).unwrap();
            zip_out.write_all(&buf).unwrap();
        }
    }
    zip_out.finish().unwrap();
}

#[test]
fn cross_import_reconstructs_files() {
    let tmp = TempDir::new().unwrap();
    let alf_path = tmp.path().join("cross.alf");
    build_cross_runtime_archive(&alf_path);

    let workspace = tmp.path().join("workspace");
    let adapter = OpenClawAdapter;
    let report = adapter.import(&alf_path, &workspace).expect("import failed");

    // Expect warnings about reconstruction
    assert!(report.warnings.iter().any(|w| w.contains("reconstructing")), "Expected a reconstruction warning");

    // Verify SOUL.md
    assert!(workspace.join("SOUL.md").exists());
    let soul_content = fs::read_to_string(workspace.join("SOUL.md")).unwrap();
    assert!(soul_content.contains("I am a cross-runtime agent."));

    // Verify IDENTITY.md
    assert!(workspace.join("IDENTITY.md").exists());
    let id_content = fs::read_to_string(workspace.join("IDENTITY.md")).unwrap();
    assert!(id_content.contains("This is my profile."));

    // Verify USER.md
    assert!(workspace.join("USER.md").exists());
    let user_content = fs::read_to_string(workspace.join("USER.md")).unwrap();
    assert!(user_content.contains("This is the cross user."));

    // Verify daily memory exists
    let memory_files: Vec<_> = fs::read_dir(workspace.join("memory"))
        .unwrap()
        .map(|r| r.unwrap().path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "md"))
        .collect();
    
    assert!(!memory_files.is_empty(), "Expected a daily memory markdown file to be created");
}
