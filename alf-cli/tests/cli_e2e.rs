use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::sync::OnceLock;
use tempfile::TempDir;

/// Every `alf` subprocess runs with an isolated, clean `HOME` so the suite
/// never reads or writes the developer's real `~/.alf/vault`. Tests that need
/// their own HOME (e.g. to seed state) override it with a later `.env("HOME", …)`.
fn alf_cmd() -> Command {
    static TEST_HOME: OnceLock<TempDir> = OnceLock::new();
    let home = TEST_HOME.get_or_init(|| TempDir::new().unwrap());
    let mut cmd = cargo_bin_cmd!("alf");
    cmd.env("HOME", home.path());
    cmd
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
        .stdout(predicate::str::contains("purge"))
        .stdout(predicate::str::contains("Current status:"));
}

#[test]
fn help_status_json_default() {
    let assert = alf_cmd().arg("help").arg("status").assert().success();
    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(text).expect("alf help status must output valid JSON by default");
    assert!(
        v.get("config_path").is_some(),
        "JSON must include config_path"
    );
    assert!(
        v.get("service_reachable").is_some(),
        "JSON must include service_reachable"
    );
    assert!(
        v.get("agent_service_status").is_some(),
        "JSON must include agent_service_status"
    );
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
    let v: serde_json::Value =
        serde_json::from_str(text).expect("alf help status --json must still output valid JSON");
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

#[test]
fn help_purge_delegates() {
    alf_cmd()
        .arg("help")
        .arg("purge")
        .assert()
        .success()
        .stdout(predicate::str::contains("DELETE /v1/agents"))
        .stdout(predicate::str::contains("Usage: alf purge"));
}

// ---------------------------------------------------------------------------
// sync — branch C from docs/how_alf_syncs.md: last_synced_sequence is Some(N)
// but the local base snapshot is missing. `alf sync` must bail BEFORE making
// any network call, with an error pointing the operator at `alf sync --recover`.
//
// This is the failure mode that motivated the formalize-alf-sync work. It is
// what an operator sees in Fly suspend logs when a runtime carries orphan
// state from the pre-0.1.4 CLI. The test locks in the exact error wording so
// suspend log parsers don't break silently.
// ---------------------------------------------------------------------------

#[test]
fn sync_bails_when_local_base_missing_and_no_recover() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();

    // Known agent_id so we can pre-seed the matching state.toml below.
    let agent_id = "ee8c59c6-0424-4cd2-b89c-19d4609bbcdf";
    fs::write(workspace.join(".alf-agent-id"), agent_id).unwrap();
    fs::write(workspace.join("SOUL.md"), "Test Agent").unwrap();

    // Pre-seed ~/.alf with a config (so ApiClient::from_config doesn't bail on
    // a missing API key) and a state.toml at sequence 5. Deliberately do NOT
    // create the matching {agent_id}-snapshot.alf, simulating the E4 / failing
    // log scenario.
    let alf_dir = home.join(".alf");
    let state_dir = alf_dir.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        alf_dir.join("config.toml"),
        // api_url is intentionally unreachable; we expect the bail to happen
        // before any HTTP call.
        "[service]\n\
         api_url = \"https://api.example.invalid\"\n\
         api_key = \"alf_test_fake_key\"\n",
    )
    .unwrap();
    fs::write(
        state_dir.join(format!("{agent_id}.toml")),
        format!(
            "agent_id = \"{agent_id}\"\n\
             last_synced_sequence = 5\n\
             last_synced_at = \"2026-05-09T18:42:11Z\"\n"
        ),
    )
    .unwrap();

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("sync")
        .arg("--runtime")
        .arg("openclaw")
        .arg("--workspace")
        .arg(&workspace)
        .assert()
        .failure();

    let stdout = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&stdout).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).expect("error stdout must be valid JSON");

    assert_eq!(v["ok"], false);
    let err_msg = v["error"].as_str().expect("error field must be a string");
    assert!(
        err_msg.contains("Local delta base missing"),
        "expected bail message to mention the missing base; got: {err_msg}"
    );
    assert!(
        err_msg.contains("alf sync --recover"),
        "expected bail message to point at the recovery command; got: {err_msg}"
    );
    assert!(
        err_msg.contains("sequence 5"),
        "expected bail message to surface the last-synced sequence; got: {err_msg}"
    );

    // The local base must remain absent — the bail must not have created it.
    let base_path = state_dir.join(format!("{agent_id}-snapshot.alf"));
    assert!(
        !base_path.exists(),
        "bail path must not create base.alf at {}",
        base_path.display()
    );
}

// ---------------------------------------------------------------------------
// --dry-run + .alfignore
// ---------------------------------------------------------------------------

fn json_stdout(assert: &assert_cmd::assert::Assert) -> serde_json::Value {
    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    serde_json::from_str(text).expect("stdout must be valid JSON")
}

/// CLI-1 / IN-2: `export --dry-run` emits a preview and writes nothing —
/// no .alf archive, no .alf-agent-id.
#[test]
fn export_dry_run_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Atlas").unwrap();
    fs::write(workspace.join("MEMORY.md"), "## Facts\n\nThe sky is blue.").unwrap();

    let output_alf = tmp.path().join("out.alf");

    let assert = alf_cmd()
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&workspace)
        .arg("-o")
        .arg(&output_alf)
        .arg("--dry-run")
        .assert()
        .success();

    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);
    assert_eq!(v["dry_run"], true);
    assert!(v["excluded_by_alfignore"].is_number());
    let paths: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"SOUL.md"));

    // --dry-run must write neither the archive (even with -o) nor .alf-agent-id.
    assert!(!output_alf.exists());
    assert!(!workspace.join(".alf-agent-id").exists());
}

/// CLI-3: a `.alfignore` workflow end-to-end — dry-run shows the exclusion,
/// the real export drops the file, and the archive still validates.
#[test]
fn alfignore_workflow_dry_run_export_validate() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("SOUL.md"), "Atlas").unwrap();
    fs::write(workspace.join("memory/2026-01-15.md"), "## A\n\nlog one").unwrap();
    fs::write(workspace.join("memory/2026-01-16.md"), "## B\n\nlog two").unwrap();
    fs::write(workspace.join(".alfignore"), "memory/2026-01-15.md\n").unwrap();

    // 1. dry-run shows the file excluded.
    let assert = alf_cmd()
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&workspace)
        .arg("--dry-run")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["excluded_by_alfignore"], 1);
    let paths: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(!paths.contains(&"memory/2026-01-15.md"));
    assert!(paths.contains(&"memory/2026-01-16.md"));

    // 2. real export omits the file and reports the same exclusion count.
    let output_alf = tmp.path().join("out.alf");
    let assert = alf_cmd()
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&workspace)
        .arg("-o")
        .arg(&output_alf)
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["excluded_by_alfignore"], 1);
    assert!(output_alf.exists());

    // 3. the archive validates.
    let assert = alf_cmd()
        .arg("validate")
        .arg(&output_alf)
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["valid"], true);
}

/// IN-6: `alf check` reports `alfignore.present` in both states.
#[test]
fn check_reports_alfignore_present() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("SOUL.md"), "Atlas").unwrap();

    // Absent → present:false.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("check")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&workspace)
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["alfignore"]["present"], false);

    // Present → present:true.
    fs::write(workspace.join(".alfignore"), "memory/\n").unwrap();
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("check")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&workspace)
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["alfignore"]["present"], true);
}

/// IN-4 (local half): `restore --dry-run` bails cleanly when no API key is
/// configured and never creates the target workspace.
#[test]
fn restore_dry_run_without_api_key_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let workspace = tmp.path().join("workspace"); // deliberately absent

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("restore")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&workspace)
        .arg("-a")
        .arg("ee8c59c6-0424-4cd2-b89c-19d4609bbcdf")
        .arg("--dry-run")
        .assert()
        .failure();

    // clap accepted --dry-run; the failure is the runtime missing-key error.
    assert_eq!(json_stdout(&assert)["ok"], false);

    // The preview must not have created the workspace.
    assert!(!workspace.exists());
}
