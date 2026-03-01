use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn alf_cmd() -> Command {
    Command::cargo_bin("alf").expect("binary not found")
}

#[test]
fn export_success() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Test Agent").unwrap();

    let output_alf = tmp.path().join("out.alf");

    alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--output")
        .arg(&output_alf)
        .assert()
        .success()
        .stdout(predicate::str::contains("Export complete"));

    assert!(output_alf.exists());
}

#[test]
fn export_unknown_runtime() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();

    alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("unknown_rt")
        .arg("--workspace")
        .arg(&workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown runtime"));
}

#[test]
fn export_missing_workspace() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("nonexistent_workspace");

    alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace)
        .assert()
        .failure();
}

#[test]
fn import_success() {
    let tmp = TempDir::new().unwrap();
    
    // 1. Export first
    let workspace1 = tmp.path().join("workspace1");
    fs::create_dir_all(&workspace1).unwrap();
    fs::write(workspace1.join("SOUL.md"), "Test Agent").unwrap();

    let output_alf = tmp.path().join("out.alf");
    alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace1)
        .arg("--output")
        .arg(&output_alf)
        .assert()
        .success();

    // 2. Import
    let workspace2 = tmp.path().join("workspace2");
    alf_cmd()
        .arg("import")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace2)
        .arg(&output_alf)
        .assert()
        .success()
        .stdout(predicate::str::contains("Import complete"));

    assert!(workspace2.join("SOUL.md").exists());
}

#[test]
fn validate_valid_archive() {
    let tmp = TempDir::new().unwrap();
    
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Test Agent").unwrap();

    let output_alf = tmp.path().join("out.alf");
    alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--output")
        .arg(&output_alf)
        .assert()
        .success();

    alf_cmd()
        .arg("validate")
        .arg(&output_alf)
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));
}

#[test]
fn validate_corrupt_archive() {
    let tmp = TempDir::new().unwrap();
    let corrupt_alf = tmp.path().join("corrupt.alf");
    fs::write(&corrupt_alf, "not a zip file").unwrap();

    alf_cmd()
        .arg("validate")
        .arg(&corrupt_alf)
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid Zip archive"));
}
