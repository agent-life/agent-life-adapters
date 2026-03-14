use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn alf_cmd() -> Command {
    cargo_bin_cmd!("alf")
}

#[test]
fn export_success_json() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Test Agent").unwrap();

    let output_alf = tmp.path().join("out.alf");

    let assert = alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--output")
        .arg(&output_alf)
        .assert()
        .success();

    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("stdout must be valid JSON");
    assert_eq!(v["ok"], true);
    assert!(v["output"].as_str().unwrap().contains("out.alf"));

    assert!(output_alf.exists());
}

#[test]
fn export_success_human() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Test Agent").unwrap();

    let output_alf = tmp.path().join("out.alf");

    alf_cmd()
        .arg("--human")
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

    let assert = alf_cmd()
        .arg("export")
        .arg("--runtime")
        .arg("unknown_rt")
        .arg("--workspace")
        .arg(&workspace)
        .assert()
        .failure();

    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("error stdout must be valid JSON");
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("Unknown runtime"));
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
fn import_success_json() {
    let tmp = TempDir::new().unwrap();

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

    let workspace2 = tmp.path().join("workspace2");
    let assert = alf_cmd()
        .arg("import")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace2)
        .arg(&output_alf)
        .assert()
        .success();

    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("stdout must be valid JSON");
    assert_eq!(v["ok"], true);

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

    let assert = alf_cmd()
        .arg("validate")
        .arg(&output_alf)
        .assert()
        .success();

    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("stdout must be valid JSON");
    assert_eq!(v["ok"], true);
    assert_eq!(v["valid"], true);
}

#[test]
fn validate_corrupt_archive() {
    let tmp = TempDir::new().unwrap();
    let corrupt_alf = tmp.path().join("corrupt.alf");
    fs::write(&corrupt_alf, "not a zip file").unwrap();

    let assert = alf_cmd()
        .arg("validate")
        .arg(&corrupt_alf)
        .assert()
        .failure();

    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("error stdout must be valid JSON");
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("invalid Zip archive"));
}

// ---------------------------------------------------------------------------
// Help system
// ---------------------------------------------------------------------------

#[test]
fn help_overview() {
    alf_cmd()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("alf — Agent Life Format"))
        .stdout(predicate::str::contains("Commands:"))
        .stdout(predicate::str::contains("export"))
        .stdout(predicate::str::contains("Current status:"));
}

#[test]
fn help_status_json_default() {
    let assert = alf_cmd()
        .arg("help")
        .arg("status")
        .assert()
        .success();
    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("alf help status must output valid JSON by default");
    assert!(v.get("config_path").is_some(), "JSON must include config_path");
    assert!(v.get("service_reachable").is_some(), "JSON must include service_reachable");
    assert!(v.get("agent_service_status").is_some(), "JSON must include agent_service_status");
}

#[test]
fn help_status_human() {
    alf_cmd()
        .arg("--human")
        .arg("help")
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Config:"))
        .stdout(predicate::str::contains("State directory:"))
        .stdout(predicate::str::contains("Service (agent-life API):"));
}

#[test]
fn help_status_json_flag_still_works() {
    let assert = alf_cmd()
        .arg("help")
        .arg("status")
        .arg("--json")
        .assert()
        .success();
    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value = serde_json::from_str(text).expect("alf help status --json must still output valid JSON");
    assert!(v.get("service_reachable").is_some());
}

#[test]
fn help_files() {
    alf_cmd()
        .arg("help")
        .arg("files")
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"))
        .stdout(predicate::str::contains("state/"));
}

#[test]
fn help_troubleshoot() {
    alf_cmd()
        .arg("help")
        .arg("troubleshoot")
        .assert()
        .success()
        .stdout(predicate::str::contains("No API key"))
        .stdout(predicate::str::contains("alf login"));
}

#[test]
fn help_export_delegates() {
    alf_cmd()
        .arg("help")
        .arg("export")
        .assert()
        .success()
        .stdout(predicate::str::contains("Export reads"))
        .stdout(predicate::str::contains("Usage: alf export"));
}
