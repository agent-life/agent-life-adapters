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

// ---------------------------------------------------------------------------
// Multi-agent core (WP0): mapping, selector, agents command, sync --all.
//
// Every test here runs with its own HOME (never the shared alf_cmd() one) so
// the [[agents]] mapping written by one test can't leak into another.
// ---------------------------------------------------------------------------

/// Pre-seed ~/.alf/config.toml under `home`. `api` adds a key + an
/// intentionally unreachable api_url (the pre-network test seam); `rows` are
/// pre-built [[agents]] rows as (alias, alf_agent_id, workspace, enabled).
fn write_config(
    home: &std::path::Path,
    api: bool,
    default_workspace: Option<&std::path::Path>,
    rows: &[(&str, &str, &std::path::Path, bool)],
) {
    let alf_dir = home.join(".alf");
    fs::create_dir_all(&alf_dir).unwrap();
    let mut content = String::new();
    if api {
        content.push_str(
            "[service]\n\
             api_url = \"https://api.example.invalid\"\n\
             api_key = \"alf_test_fake_key\"\n\n",
        );
    }
    if let Some(ws) = default_workspace {
        content.push_str(&format!(
            "[defaults]\nruntime = \"openclaw\"\nworkspace = \"{}\"\n\n",
            ws.display()
        ));
    }
    for (alias, id, ws, enabled) in rows {
        content.push_str(&format!(
            "[[agents]]\n\
             runtime = \"openclaw\"\n\
             runtime_agent = \"{alias}\"\n\
             alf_agent_id = \"{id}\"\n\
             workspace = \"{}\"\n\
             enabled = {enabled}\n\n",
            ws.display()
        ));
    }
    fs::write(alf_dir.join("config.toml"), content).unwrap();
}

/// Seed a minimal OpenClaw workspace; `agent_id` (if given) becomes its
/// `.alf-agent-id`.
fn seed_workspace(ws: &std::path::Path, agent_id: Option<&str>) {
    fs::create_dir_all(ws).unwrap();
    fs::write(ws.join("SOUL.md"), "# Test Agent\n\nsoul").unwrap();
    if let Some(id) = agent_id {
        fs::write(ws.join(".alf-agent-id"), id).unwrap();
    }
}

fn read_config_toml(home: &std::path::Path) -> toml::Value {
    let raw = fs::read_to_string(home.join(".alf").join("config.toml")).unwrap();
    raw.parse::<toml::Value>().unwrap()
}

/// DoD 2 (e2e half): `alf check` writes the mapping and adopts the
/// workspace's existing `.alf-agent-id` as the row identity.
#[test]
fn check_writes_mapping_and_adopts_existing_agent_id() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-aaaa-4aaa-8aaa-000000000001";
    seed_workspace(&ws, Some(id));

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("check")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&ws)
        .assert()
        .success();

    let v = json_stdout(&assert);
    assert_eq!(v["agents"]["first_run"], true);
    assert_eq!(v["agents"]["agents"][0]["alf_agent_id"], id);
    assert_eq!(v["agents"]["agents"][0]["enabled"], true);
    assert_eq!(v["agents"]["agents"][0]["status"], "new");

    let config = read_config_toml(&home);
    let rows = config["agents"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["alf_agent_id"].as_str().unwrap(), id);
    assert_eq!(rows[0]["enabled"].as_bool(), Some(true));
}

/// DoD 2 (e2e half): a re-check keeps `alf_agent_id` stable and reports no
/// new agents.
#[test]
fn recheck_keeps_alf_agent_id_stable() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    seed_workspace(&ws, None); // no pre-existing id: check derives + persists one

    let check = |()| -> serde_json::Value {
        let assert = alf_cmd()
            .env("HOME", &home)
            .arg("check")
            .arg("-r")
            .arg("openclaw")
            .arg("-w")
            .arg(&ws)
            .assert()
            .success();
        json_stdout(&assert)
    };

    let first = check(());
    let first_id = first["agents"]["agents"][0]["alf_agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    let second = check(());
    assert_eq!(second["agents"]["agents"][0]["alf_agent_id"], *first_id);
    assert_eq!(second["agents"]["first_run"], false);
    assert_eq!(second["agents"]["new"].as_array().unwrap().len(), 0);
    assert_eq!(second["agents"]["agents"][0]["status"], "existing");

    let config = read_config_toml(&home);
    assert_eq!(
        config["agents"].as_array().unwrap()[0]["alf_agent_id"]
            .as_str()
            .unwrap(),
        first_id
    );
}

/// DoD 3: single-agent zero-friction — bare check writes one enabled row,
/// bare sync (no flags at all beyond -r) resolves everything from the mapping
/// and fails only at the network step, never at selection or workspace
/// resolution.
#[test]
fn bare_check_then_bare_sync_no_flags() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-aaaa-4aaa-8aaa-000000000002";
    seed_workspace(&ws, Some(id));
    write_config(&home, true, Some(&ws), &[]);

    // Bare check (workspace comes from [defaults]).
    alf_cmd()
        .env("HOME", &home)
        .arg("check")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .success();
    let config = read_config_toml(&home);
    let rows = config["agents"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["alf_agent_id"].as_str().unwrap(), id);
    assert_eq!(rows[0]["enabled"].as_bool(), Some(true));

    // Bare sync: selection passes, export runs, the failure is the (first)
    // network call — registration against the unreachable host.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], "registration_failed");
    let err = v["error"].as_str().unwrap();
    assert!(
        !err.contains("No workspace specified"),
        "sync must resolve the workspace from the mapping: {err}"
    );
}

/// DoD 3 / user decision #2: a bare sync on an empty mapping lazy-inits the
/// mapping row (no prior check needed); a second run does not rewrite it.
#[test]
fn bare_sync_without_check_lazy_inits_mapping() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-aaaa-4aaa-8aaa-000000000003";
    seed_workspace(&ws, Some(id));
    write_config(&home, true, None, &[]);

    let sync = |()| {
        alf_cmd()
            .env("HOME", &home)
            .arg("sync")
            .arg("-r")
            .arg("openclaw")
            .arg("-w")
            .arg(&ws)
            .assert()
            .failure() // unreachable API; selection + mapping write happen first
    };

    sync(());
    let first = fs::read_to_string(home.join(".alf").join("config.toml")).unwrap();
    assert!(first.contains("[[agents]]"), "lazy init must write the row");
    assert!(first.contains(id), "the row must adopt the workspace id");

    sync(());
    let second = fs::read_to_string(home.join(".alf").join("config.toml")).unwrap();
    assert_eq!(first, second, "second run must not rewrite the mapping");
}

/// DoD 3 (the load-bearing seam): a bare export stamps the mapping's
/// alf_agent_id as the archive identity.
#[test]
fn bare_export_stamps_mapping_id() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-aaaa-4aaa-8aaa-000000000004";
    seed_workspace(&ws, Some(id));

    let out = tmp.path().join("out.alf");
    alf_cmd()
        .env("HOME", &home)
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&ws)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();

    // manifest.agent.id == [[agents]].alf_agent_id
    let config = read_config_toml(&home);
    let row_id = config["agents"].as_array().unwrap()[0]["alf_agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(row_id, id);
    let reader = alf_core::AlfReader::new(fs::File::open(&out).unwrap()).unwrap();
    assert_eq!(reader.manifest().agent.id.to_string(), row_id);
}

/// DoD 3: sync heals a missing workspace `.alf-agent-id` from the mapping
/// (the write-through), even though the network step then fails.
#[test]
fn sync_heals_missing_workspace_id_from_mapping() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-aaaa-4aaa-8aaa-000000000005";
    seed_workspace(&ws, None); // id file deliberately absent
    write_config(&home, true, None, &[("main", id, &ws, true)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .failure(); // unreachable API — but the export ran first
    assert_eq!(json_stdout(&assert)["code"], "registration_failed");

    let healed = fs::read_to_string(ws.join(".alf-agent-id")).unwrap();
    assert_eq!(healed.trim(), id, "sync must heal the id from the mapping");
}

/// DoD 4: `alf check` reports identity drift (workspace id X vs mapping Y)
/// as a warning and leaves the mapping untouched.
#[test]
fn check_reports_drift_json() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let x = "cfef1150-bbbb-4bbb-8bbb-000000000001";
    let y = "cfef1150-bbbb-4bbb-8bbb-000000000002";
    seed_workspace(&ws, Some(x));
    write_config(&home, false, None, &[("main", y, &ws, true)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("check")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&ws)
        .assert()
        .success(); // drift is warn-only for check

    let v = json_stdout(&assert);
    let drift = &v["agents"]["drift"][0];
    assert!(drift["message"].as_str().unwrap().contains(x));
    assert!(drift["message"].as_str().unwrap().contains(y));
    assert!(v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["code"] == "agent_identity_drift"));

    // The mapping keeps Y.
    let config = read_config_toml(&home);
    assert_eq!(
        config["agents"].as_array().unwrap()[0]["alf_agent_id"]
            .as_str()
            .unwrap(),
        y
    );
}

/// DoD 4: sync fails closed on identity drift, before any network call, with
/// a coded error naming both ids and the exact heal command.
#[test]
fn sync_bails_on_identity_drift_before_network() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let x = "cfef1150-bbbb-4bbb-8bbb-000000000003";
    let y = "cfef1150-bbbb-4bbb-8bbb-000000000004";
    seed_workspace(&ws, Some(x));
    write_config(&home, true, None, &[("main", y, &ws, true)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .failure();

    let v = json_stdout(&assert);
    assert_eq!(v["code"], "agent_id_drift");
    let err = v["error"].as_str().unwrap();
    assert!(err.contains(x), "must name the workspace id: {err}");
    assert!(err.contains(y), "must name the mapping id: {err}");
    let hint = v["hint"].as_str().unwrap();
    assert!(
        hint.contains(&format!("echo {y} > ")),
        "must give the exact heal command: {hint}"
    );
}

/// Command surface: `alf agents` joins the mapping with per-agent sync state.
#[test]
fn agents_list_shows_mapping_and_state() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-cccc-4ccc-8ccc-000000000001";
    seed_workspace(&ws, Some(id));
    write_config(&home, false, None, &[("main", id, &ws, true)]);

    // Seed state for the agent so the join has something to show.
    let state_dir = home.join(".alf").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join(format!("{id}.toml")),
        format!("agent_id = \"{id}\"\nlast_synced_sequence = 4\nlast_synced_at = \"2026-06-01T00:00:00Z\"\n"),
    )
    .unwrap();

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .assert()
        .success();

    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);
    assert!(
        v["runtime"].is_null(),
        "no -r filter ⇒ no top-level runtime; it lives per row"
    );
    assert!(v["mapping_path"].as_str().unwrap().contains("config.toml"));
    let row = &v["agents"][0];
    assert_eq!(row["runtime"], "openclaw");
    assert_eq!(row["runtime_agent"], "main");
    assert_eq!(row["alf_agent_id"], id);
    assert_eq!(row["enabled"], true);
    assert_eq!(row["last_synced_sequence"], 4);
    assert_eq!(row["snapshot_exists"], false);
}

/// Command surface: enable/disable round-trip, idempotent, lazy-registration
/// note on enable.
#[test]
fn agents_enable_disable_round_trip() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-cccc-4ccc-8ccc-000000000002";
    write_config(&home, false, None, &[("main", id, &ws, false)]);

    let toggle = |verb: &str| -> serde_json::Value {
        let assert = alf_cmd()
            .env("HOME", &home)
            .arg("agents")
            .arg(verb)
            .arg("main")
            .assert()
            .success();
        json_stdout(&assert)
    };

    let v = toggle("enable");
    assert_eq!(v["enabled"], true);
    assert!(v["note"].as_str().unwrap().contains("lazy"));
    assert_eq!(
        read_config_toml(&home)["agents"].as_array().unwrap()[0]["enabled"].as_bool(),
        Some(true)
    );

    // Idempotent.
    assert_eq!(toggle("enable")["enabled"], true);

    let v = toggle("disable");
    assert_eq!(v["enabled"], false);
    assert_eq!(
        read_config_toml(&home)["agents"].as_array().unwrap()[0]["enabled"].as_bool(),
        Some(false)
    );
}

/// Command surface: unknown alias errors with the coded remedy.
#[test]
fn agents_enable_unknown_alias_errors_with_remedy() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-cccc-4ccc-8ccc-000000000003";
    write_config(&home, false, None, &[("main", id, &ws, true)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .arg("enable")
        .arg("ghost")
        .assert()
        .failure();

    let v = json_stdout(&assert);
    assert_eq!(v["code"], "agent_not_found");
    assert!(v["error"].as_str().unwrap().contains("main"));
    assert!(v["hint"].as_str().unwrap().contains("alf agents"));
}

/// Command surface: an empty mapping points at `alf check`.
#[test]
fn agents_with_no_mapping_points_to_check() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .assert()
        .failure();

    let v = json_stdout(&assert);
    assert_eq!(v["code"], "no_agents");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("No agents are mapped"));
    assert!(v["hint"].as_str().unwrap().contains("alf check"));
}

/// Review fix: without -r, `alf agents` spans every runtime — a zeroclaw row
/// must be listable and toggleable while [defaults].runtime is openclaw
/// (otherwise sync's `alf agents enable <alias>` remedies are dead ends).
#[test]
fn agents_commands_span_runtimes_without_filter() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let alf_dir = home.join(".alf");
    fs::create_dir_all(&alf_dir).unwrap();
    let ws = tmp.path().join("ws-zc");
    let id = "cfef1150-eeee-4eee-8eee-000000000001";
    fs::write(
        alf_dir.join("config.toml"),
        format!(
            "[[agents]]\nruntime = \"zeroclaw\"\nruntime_agent = \"default\"\n\
             alf_agent_id = \"{id}\"\nworkspace = \"{}\"\nenabled = false\n",
            ws.display()
        ),
    )
    .unwrap();

    // List spans runtimes.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["agents"][0]["runtime"], "zeroclaw");
    assert_eq!(v["agents"][0]["runtime_agent"], "default");

    // Enable resolves the alias across runtimes without -r.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .arg("enable")
        .arg("default")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["enabled"], true);
    assert_eq!(v["runtime"], "zeroclaw");
    assert_eq!(
        read_config_toml(&home)["agents"].as_array().unwrap()[0]["enabled"].as_bool(),
        Some(true)
    );
}

/// Review fix: an alias mapped for two runtimes is ambiguous without -r; the
/// -r form resolves it.
#[test]
fn agents_toggle_ambiguous_alias_requires_runtime_flag() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let alf_dir = home.join(".alf");
    fs::create_dir_all(&alf_dir).unwrap();
    let id1 = "cfef1150-eeee-4eee-8eee-000000000002";
    let id2 = "cfef1150-eeee-4eee-8eee-000000000003";
    fs::write(
        alf_dir.join("config.toml"),
        format!(
            "[[agents]]\nruntime = \"openclaw\"\nruntime_agent = \"main\"\n\
             alf_agent_id = \"{id1}\"\nworkspace = \"/ws-oc\"\nenabled = false\n\n\
             [[agents]]\nruntime = \"zeroclaw\"\nruntime_agent = \"main\"\n\
             alf_agent_id = \"{id2}\"\nworkspace = \"/ws-zc\"\nenabled = false\n"
        ),
    )
    .unwrap();

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .arg("enable")
        .arg("main")
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert_eq!(v["code"], "agent_selection_ambiguous");
    assert!(v["hint"].as_str().unwrap().contains("-r"));

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("agents")
        .arg("-r")
        .arg("zeroclaw")
        .arg("enable")
        .arg("main")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["runtime"], "zeroclaw");
    assert_eq!(v["alf_agent_id"], id2);
}

/// Review fix: import's own fail-closed remedy ("pass --agent <archive-id>")
/// must work on a mapped host — an unmapped UUID verifies the archive
/// directly and imports the legacy way.
#[test]
fn import_foreign_archive_by_uuid_on_mapped_host() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws_a = tmp.path().join("ws-a");
    let id_a = "cfef1150-ffff-4fff-8fff-00000000000a";
    let id_b = "cfef1150-ffff-4fff-8fff-00000000000b";
    seed_workspace(&ws_a, Some(id_a));
    write_config(&home, false, None, &[("main", id_a, &ws_a, true)]);

    // Build agent B's archive via an ad-hoc export of a foreign workspace.
    let ws_b = tmp.path().join("ws-b");
    seed_workspace(&ws_b, Some(id_b));
    let archive = tmp.path().join("b.alf");
    alf_cmd()
        .env("HOME", &home)
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&ws_b)
        .arg("-o")
        .arg(&archive)
        .assert()
        .success();

    // Importing B's archive as the mapped agent fails closed and advertises
    // the --agent remedy…
    let target = tmp.path().join("restored");
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("import")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&ws_a)
        .arg(&archive)
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains(&format!("--agent {id_b}")),
        "the fail-closed error must advertise the working remedy"
    );

    // …and following that exact remedy succeeds.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("import")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&target)
        .arg("--agent")
        .arg(id_b)
        .arg(&archive)
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);
    assert_eq!(
        fs::read_to_string(target.join(".alf-agent-id"))
            .unwrap()
            .trim(),
        id_b
    );
}

/// Review fix: the clap conflict cannot see a global --agent matched before
/// the subcommand — the runtime guard must reject it instead of silently
/// syncing every agent.
#[test]
fn sync_all_rejects_global_agent_before_subcommand() {
    let assert = alf_cmd()
        .arg("--agent")
        .arg("main")
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .arg("--all")
        .assert()
        .failure()
        .code(1);
    let v = json_stdout(&assert);
    assert!(
        v["error"].as_str().unwrap().contains("--all"),
        "must reject --agent + --all in any argument position"
    );
}

/// Review fix: `export --dry-run` previews the same workspace the real export
/// would use — mapping-based resolution, no -w and no [defaults].workspace.
#[test]
fn export_dry_run_resolves_workspace_from_mapping() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-ffff-4fff-8fff-00000000000c";
    seed_workspace(&ws, Some(id));
    write_config(&home, false, None, &[("main", id, &ws, true)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .current_dir(tmp.path())
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("--dry-run")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);
    assert_eq!(v["dry_run"], true);
    assert!(
        !v["files"].as_array().unwrap().is_empty(),
        "must preview the mapped workspace's files"
    );
}

/// Command surface: `sync --all` collects per-agent failures into ONE JSON
/// object and exits 1 — one agent's failure must not block the others.
#[test]
fn sync_all_collects_per_agent_failures() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws1 = tmp.path().join("ws1");
    let ws2 = tmp.path().join("ws2");
    let id1 = "cfef1150-dddd-4ddd-8ddd-000000000001";
    let id2 = "cfef1150-dddd-4ddd-8ddd-000000000002";
    seed_workspace(&ws1, Some(id1));
    seed_workspace(&ws2, Some(id2));
    write_config(
        &home,
        true,
        None,
        &[("main", id1, &ws1, true), ("helper", id2, &ws2, true)],
    );

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .arg("--all")
        .assert()
        .failure()
        .code(1);

    let out = assert.get_output().stdout.clone();
    let text = std::str::from_utf8(&out).unwrap();
    // Exactly ONE JSON object on stdout.
    let v: serde_json::Value = serde_json::from_str(text.trim()).expect("one JSON object");
    assert_eq!(v["ok"], false);
    assert_eq!(v["all"], true);
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "both agents must be attempted");
    for r in results {
        assert_eq!(r["ok"], false);
        assert_eq!(r["code"], "registration_failed");
    }
}

/// Command surface: --all conflicts with --agent at the clap level.
#[test]
fn sync_all_conflicts_with_agent_flag() {
    let assert = alf_cmd()
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .arg("--all")
        .arg("--agent")
        .arg("main")
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("--agent") && stderr.contains("cannot be used with"),
        "clap must reject the combination: {stderr}"
    );
}

/// Command surface: an unknown --agent fails at selection, before the API-key
/// check and before any network call.
#[test]
fn sync_unknown_agent_flag_fails_before_network() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-dddd-4ddd-8ddd-000000000003";
    // NO api key: reaching the API-key error would mean selection passed.
    write_config(&home, false, None, &[("main", id, &ws, true)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .arg("--agent")
        .arg("ghost")
        .assert()
        .failure();

    let v = json_stdout(&assert);
    assert_eq!(v["code"], "agent_not_found");
    assert!(v["error"].as_str().unwrap().contains("ghost"));
}

/// Command surface: a disabled agent selected via ALF_AGENT is refused by the
/// sync gate with the enable remedy (also before the API-key check).
#[test]
fn sync_disabled_agent_via_env_errors_with_enable_remedy() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-dddd-4ddd-8ddd-000000000004";
    seed_workspace(&ws, Some(id));
    write_config(&home, false, None, &[("main", id, &ws, false)]);

    let assert = alf_cmd()
        .env("HOME", &home)
        .env("ALF_AGENT", "main")
        .arg("sync")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .failure();

    let v = json_stdout(&assert);
    assert_eq!(v["code"], "agent_disabled");
    assert!(v["hint"]
        .as_str()
        .unwrap()
        .contains("alf agents enable main"));
}

/// Command surface: `vault add` defaults the record's agent_id to the
/// selection (sole enabled mapping row) when --agent-id is not passed.
#[test]
fn vault_add_defaults_record_agent_id_to_selection() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-eeee-4eee-8eee-000000000001";
    write_config(&home, false, None, &[("main", id, &ws, true)]);

    let key_file = tmp.path().join("vault.key");
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("keygen")
        .arg("--out")
        .arg(&key_file)
        .assert()
        .success();

    let target = tmp.path().join("credentials.json");
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--in")
        .arg(&target)
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("me@example.com")
        .arg("--secret")
        .arg("hunter2")
        .arg("--vault-key-file")
        .arg(&key_file)
        .assert()
        .success();

    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(
        doc["credentials"][0]["agent_id"].as_str().unwrap(),
        id,
        "the record must default to the selected agent's id"
    );
}

// ---------------------------------------------------------------------------
// Per-agent vault (WP1): per-agent default paths, isolation, migration,
// rotate-key. Every test runs with its own HOME.
// ---------------------------------------------------------------------------

/// The agent's default vault file under `home`.
fn agent_vault(home: &std::path::Path, id: &str) -> std::path::PathBuf {
    home.join(".alf")
        .join("vault")
        .join(id)
        .join("credentials.json")
}

/// The agent's default openclaw vault-key file under `home`.
fn agent_key(home: &std::path::Path, id: &str) -> std::path::PathBuf {
    home.join(".openclaw")
        .join("state")
        .join(id)
        .join(".alf-vault-key")
}

/// `alf vault keygen --out <path>` under `home`.
fn keygen_at(home: &std::path::Path, path: &std::path::Path) {
    alf_cmd()
        .env("HOME", home)
        .arg("vault")
        .arg("keygen")
        .arg("--out")
        .arg(path)
        .assert()
        .success();
}

/// E-1: with a sole enabled agent and a key at the per-agent default path,
/// a bare `vault add` (no key flags, no --in) lands in the per-agent vault.
#[test]
fn vault_add_defaults_to_per_agent_vault_path() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-1111-4111-8111-000000000001";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    keygen_at(&home, &agent_key(&home, id));

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("me@example.com")
        .arg("--secret")
        .arg("hunter2")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);

    let vault_path = agent_vault(&home, id);
    assert_eq!(
        v["written_to"].as_str().unwrap(),
        vault_path.to_str().unwrap(),
        "must write to the per-agent vault"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&vault_path).unwrap()).unwrap();
    assert_eq!(doc["credentials"][0]["agent_id"], id);
}

/// RF-012 (DoD): a direct-CLI default-vault add serializes on the same
/// per-agent flock the watch loop and MCP tools take. The test holds that exact
/// lock (`~/.alf/state/{id}.lock`) and proves a concurrent `alf vault add`
/// cannot proceed while it is held; once released, the add lands WITHOUT losing
/// the record already in the vault. Uses a real subprocess and a real flock —
/// an intra-process mutex would not prove cross-process coordination.
#[test]
fn concurrent_default_vault_add_blocks_on_shared_lock_and_preserves_records() {
    use fs2::FileExt;
    use std::process::{Command as StdCommand, Stdio};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-1111-4111-8111-0000000000aa";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    keygen_at(&home, &agent_key(&home, id));

    // Seed one record so we can later prove the contended RMW did not discard a
    // pre-existing credential.
    alf_cmd()
        .env("HOME", &home)
        .args([
            "vault",
            "add",
            "-r",
            "openclaw",
            "--service",
            "email",
            "--label",
            "first@example.com",
            "--secret",
            "one",
        ])
        .assert()
        .success();

    // Hold the agent's L3 lock exactly as the CLI does: an exclusive flock on
    // the per-agent lock file under ~/.alf/state.
    let lock_path = home.join(".alf").join("state").join(format!("{id}.lock"));
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.lock_exclusive().unwrap();

    // A concurrent add must block on that lock rather than racing the RMW. If
    // the CLI did NOT take the lock, this fast add would exit well under 800ms.
    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_alf"))
        .env("HOME", &home)
        .args([
            "vault",
            "add",
            "-r",
            "openclaw",
            "--service",
            "slack",
            "--label",
            "second@example.com",
            "--secret",
            "two",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_millis(800));
    assert!(
        child.try_wait().unwrap().is_none(),
        "vault add must block while another holder owns the per-agent lock"
    );

    // Release; the add now proceeds and commits its record.
    fs2::FileExt::unlock(&lock_file).unwrap();
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "vault add must succeed once the lock is free"
    );

    // Both records survive — the contended RMW read the just-released state.
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(agent_vault(&home, id)).unwrap()).unwrap();
    let services: Vec<&str> = doc["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["service"].as_str().unwrap())
        .collect();
    assert!(services.contains(&"email"), "pre-existing record preserved");
    assert!(services.contains(&"slack"), "contended add landed");
    assert_eq!(services.len(), 2, "exactly the two records, no loss");
}

/// RF-012 (DoD): rotation and add contend on the same real process lock. The
/// winner is intentionally unspecified; whichever runs second must re-read the
/// final key/vault state, leaving both records decryptable with the one default
/// key and no abandoned staged key.
#[test]
fn concurrent_default_vault_rotation_and_add_leave_a_coherent_vault() {
    use fs2::FileExt;
    use std::process::{Command as StdCommand, Stdio};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-1111-4111-8111-0000000000ab";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    keygen_at(&home, &agent_key(&home, id));

    alf_cmd()
        .env("HOME", &home)
        .args([
            "vault",
            "add",
            "-r",
            "openclaw",
            "--service",
            "email",
            "--label",
            "before-rotation",
            "--secret",
            "old-secret",
        ])
        .assert()
        .success();

    let lock_path = home.join(".alf").join("state").join(format!("{id}.lock"));
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.lock_exclusive().unwrap();

    let mut rotate = StdCommand::new(env!("CARGO_BIN_EXE_alf"))
        .env("HOME", &home)
        .args(["vault", "rotate-key", "-r", "openclaw"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut add = StdCommand::new(env!("CARGO_BIN_EXE_alf"))
        .env("HOME", &home)
        .args([
            "vault",
            "add",
            "-r",
            "openclaw",
            "--service",
            "slack",
            "--label",
            "during-rotation",
            "--secret",
            "new-secret",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_millis(800));
    assert!(
        rotate.try_wait().unwrap().is_none(),
        "rotation must wait for L3"
    );
    assert!(add.try_wait().unwrap().is_none(), "add must wait for L3");
    fs2::FileExt::unlock(&lock_file).unwrap();
    assert!(rotate.wait().unwrap().success(), "rotation must complete");
    assert!(add.wait().unwrap().success(), "add must complete");

    for (label, expected) in [
        ("before-rotation", "old-secret"),
        ("during-rotation", "new-secret"),
    ] {
        let assert = alf_cmd()
            .env("HOME", &home)
            .args([
                "vault",
                "decrypt",
                "-r",
                "openclaw",
                "--label",
                label,
                "--yes-insecure",
            ])
            .assert()
            .success();
        assert_eq!(json_stdout(&assert)["payload"]["secret"], expected);
    }
    assert!(
        !agent_key(&home, id)
            .with_file_name(".alf-vault-key.new")
            .exists(),
        "a normal contended rotation must not leave an unreconciled staged key"
    );
}

/// RF-012 (DoD): a real default-vault mutation that outlives the production
/// bounded wait reports `agent_busy` and leaves both canonical vault and key
/// bytes exactly unchanged. The production timeout is deliberately exercised
/// here; no release-only timeout override weakens the contract.
#[test]
fn default_vault_lock_timeout_leaves_canonical_bytes_unchanged() {
    use fs2::FileExt;
    use std::process::Command as StdCommand;
    use std::time::{Duration, Instant};

    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-1111-4111-8111-0000000000ac";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    keygen_at(&home, &agent_key(&home, id));
    alf_cmd()
        .env("HOME", &home)
        .args([
            "vault",
            "add",
            "-r",
            "openclaw",
            "--service",
            "email",
            "--label",
            "before-timeout",
            "--secret",
            "stable",
        ])
        .assert()
        .success();

    let vault_path = agent_vault(&home, id);
    let key_path = agent_key(&home, id);
    let vault_before = fs::read(&vault_path).unwrap();
    let key_before = fs::read(&key_path).unwrap();
    let lock_path = home.join(".alf").join("state").join(format!("{id}.lock"));
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock_file.lock_exclusive().unwrap();

    let started = Instant::now();
    let output = StdCommand::new(env!("CARGO_BIN_EXE_alf"))
        .env("HOME", &home)
        .args([
            "vault",
            "add",
            "-r",
            "openclaw",
            "--service",
            "slack",
            "--label",
            "must-not-write",
            "--secret",
            "new-value",
        ])
        .output()
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "the mutation must wait for the bounded L3 acquisition before failing"
    );
    assert!(!output.status.success(), "held L3 must reject the mutation");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["code"], "agent_busy", "lock timeout must stay coded");
    assert_eq!(
        fs::read(&vault_path).unwrap(),
        vault_before,
        "vault changed on timeout"
    );
    assert_eq!(
        fs::read(&key_path).unwrap(),
        key_before,
        "key changed on timeout"
    );
    fs2::FileExt::unlock(&lock_file).unwrap();
}

/// E-2: the secret can arrive on stdin (no --secret flag on the argv secret
/// surface) and round-trips through a bare decrypt.
#[test]
fn vault_add_secret_via_stdin_round_trips() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-1111-4111-8111-000000000002";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    keygen_at(&home, &agent_key(&home, id));

    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("stdin-label")
        .write_stdin("s3cret-from-stdin\n")
        .assert()
        .success();

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("stdin-label")
        .arg("--yes-insecure")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["payload"]["secret"], "s3cret-from-stdin");
}

/// E-3 (DoD): two agents get distinct default keys and vaults, auto-picked
/// with no key flags; a cross-agent key fails closed at the AEAD layer.
#[test]
fn two_agents_distinct_default_keys_and_vaults() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws_a = tmp.path().join("ws-a");
    let ws_b = tmp.path().join("ws-b");
    let a = "cfef1150-2222-4222-8222-00000000000a";
    let b = "cfef1150-2222-4222-8222-00000000000b";
    write_config(
        &home,
        false,
        None,
        &[("main", a, &ws_a, true), ("helper", b, &ws_b, true)],
    );
    keygen_at(&home, &agent_key(&home, a));
    keygen_at(&home, &agent_key(&home, b));

    let add = |agent: &str, secret: &str| {
        alf_cmd()
            .env("HOME", &home)
            .arg("--agent")
            .arg(agent)
            .arg("vault")
            .arg("add")
            .arg("-r")
            .arg("openclaw")
            .arg("--service")
            .arg("email")
            .arg("--label")
            .arg("shared-label")
            .arg("--secret")
            .arg(secret)
            .assert()
            .success();
    };
    add("main", "secret-a");
    add("helper", "secret-b");

    assert!(agent_vault(&home, a).is_file());
    assert!(agent_vault(&home, b).is_file());

    // Own agent, auto key: opens.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg("main")
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("shared-label")
        .arg("--yes-insecure")
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["payload"]["secret"], "secret-a");

    // Cross-agent key: fails closed at the AEAD layer.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg("main")
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("shared-label")
        .arg("--yes-insecure")
        .arg("--vault-key-file")
        .arg(agent_key(&home, b))
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert!(
        v["error"].as_str().unwrap().contains("Decryption failed"),
        "cross-agent key must AEAD-fail: {v}"
    );
}

/// E-4 (D9): a default-path vault command with two enabled agents and no
/// selector stops and asks with the coded ambiguity error.
#[test]
fn vault_add_ambiguous_selection_errors() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws_a = tmp.path().join("ws-a");
    let ws_b = tmp.path().join("ws-b");
    let a = "cfef1150-3333-4333-8333-00000000000a";
    let b = "cfef1150-3333-4333-8333-00000000000b";
    write_config(
        &home,
        false,
        None,
        &[("main", a, &ws_a, true), ("helper", b, &ws_b, true)],
    );

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("email")
        .arg("--secret")
        .arg("s")
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert_eq!(v["code"], "agent_selection_ambiguous");
}

/// E-5 (DoD): per-agent round-trip — add → export carries only the agent's
/// Layer 4 → wipe → import re-creates the per-agent vault → decrypt with the
/// agent's default key.
#[test]
fn per_agent_round_trip_export_import() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws_a = tmp.path().join("ws-a");
    let ws_b = tmp.path().join("ws-b");
    let a = "cfef1150-4444-4444-8444-00000000000a";
    let b = "cfef1150-4444-4444-8444-00000000000b";
    seed_workspace(&ws_a, Some(a));
    seed_workspace(&ws_b, Some(b));
    write_config(
        &home,
        false,
        None,
        &[("main", a, &ws_a, true), ("helper", b, &ws_b, true)],
    );
    keygen_at(&home, &agent_key(&home, a));
    keygen_at(&home, &agent_key(&home, b));

    let add = |agent: &str, label: &str, secret: &str| {
        alf_cmd()
            .env("HOME", &home)
            .arg("--agent")
            .arg(agent)
            .arg("vault")
            .arg("add")
            .arg("-r")
            .arg("openclaw")
            .arg("--service")
            .arg("email")
            .arg("--label")
            .arg(label)
            .arg("--secret")
            .arg(secret)
            .assert()
            .success();
    };
    add("main", "label-a", "secret-A");
    add("helper", "label-b", "secret-B");

    let archive_a = tmp.path().join("a.alf");
    let archive_b = tmp.path().join("b.alf");
    for (agent, out) in [("main", &archive_a), ("helper", &archive_b)] {
        alf_cmd()
            .env("HOME", &home)
            .arg("--agent")
            .arg(agent)
            .arg("export")
            .arg("-r")
            .arg("openclaw")
            .arg("-o")
            .arg(out)
            .assert()
            .success();
    }

    // Layer-4 isolation: each archive holds only its own agent's records.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("list")
        .arg("--in")
        .arg(&archive_a)
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["count"], 1);
    assert_eq!(v["credentials"][0]["label"], "label-a");

    // Wipe every local vault, then restore agent A's from its archive.
    fs::remove_dir_all(home.join(".alf").join("vault")).unwrap();
    alf_cmd()
        .env("HOME", &home)
        .arg("import")
        .arg("-r")
        .arg("openclaw")
        .arg("--agent")
        .arg("main")
        .arg(&archive_a)
        .assert()
        .success();

    assert!(agent_vault(&home, a).is_file(), "a's vault re-created");
    assert!(!agent_vault(&home, b).exists(), "b's vault stays wiped");

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg("main")
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("label-a")
        .arg("--yes-insecure")
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["payload"]["secret"], "secret-A");
}

/// E-6 (DoD): a wrong-agent restore fails closed; the UUID escape hatch
/// lands the records in the ARCHIVE agent's own vault dir, never the
/// victim's; a cross-agent key still AEAD-fails on the restored records.
#[test]
fn wrong_agent_restore_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws_a = tmp.path().join("ws-a");
    let a = "cfef1150-5555-4555-8555-00000000000a";
    let c = "cfef1150-5555-4555-8555-00000000000c"; // unmapped foreign agent
    seed_workspace(&ws_a, Some(a));
    write_config(&home, false, None, &[("main", a, &ws_a, true)]);
    keygen_at(&home, &agent_key(&home, a));
    keygen_at(&home, &agent_key(&home, c));

    // Seed c's vault (raw-UUID scope passthrough) and export c's archive from
    // an ad-hoc foreign workspace.
    alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg(c)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("c-label")
        .arg("--secret")
        .arg("c-secret")
        .assert()
        .success();
    let ws_c = tmp.path().join("ws-c");
    seed_workspace(&ws_c, Some(c));
    let archive_c = tmp.path().join("c.alf");
    alf_cmd()
        .env("HOME", &home)
        .arg("export")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&ws_c)
        .arg("-o")
        .arg(&archive_c)
        .assert()
        .success();

    // Wipe local vaults so the restore's writes are unambiguous.
    fs::remove_dir_all(home.join(".alf").join("vault")).unwrap();

    // 1. Importing c's archive as the mapped agent fails closed.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("import")
        .arg("-r")
        .arg("openclaw")
        .arg(&archive_c)
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert!(
        v["error"].as_str().unwrap().contains("Refusing to import"),
        "must fail closed: {v}"
    );

    // 2. The UUID escape hatch imports it as ITS OWN agent: records land in
    //    c's vault dir, never a's.
    let restored_ws = tmp.path().join("restored-c");
    alf_cmd()
        .env("HOME", &home)
        .arg("import")
        .arg("-r")
        .arg("openclaw")
        .arg("-w")
        .arg(&restored_ws)
        .arg("--agent")
        .arg(c)
        .arg(&archive_c)
        .assert()
        .success();
    assert!(agent_vault(&home, c).is_file(), "c's own vault dir");
    assert!(!agent_vault(&home, a).exists(), "never the victim's dir");

    // 3. AEAD backstop: a's key cannot open c's restored records.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("decrypt")
        .arg("--in")
        .arg(agent_vault(&home, c))
        .arg("--label")
        .arg("c-label")
        .arg("--yes-insecure")
        .arg("--vault-key-file")
        .arg(agent_key(&home, a))
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert!(v["error"].as_str().unwrap().contains("Decryption failed"));
}

/// E-7: rotate-key end-to-end — default-file old key, in-place replacement,
/// no-flag decrypt under the new key, fingerprints + next in the JSON; a
/// fabricated crash window self-heals with `recovered: true`.
#[test]
fn vault_rotate_key_e2e() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-6666-4666-8666-000000000001";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    let key_path = agent_key(&home, id);
    keygen_at(&home, &key_path);
    let old_key_content = fs::read_to_string(&key_path).unwrap();

    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("rot-label")
        .arg("--secret")
        .arg("rot-secret")
        .assert()
        .success();

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("rotate-key")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);
    assert_eq!(v["rotated"], 1);
    assert_ne!(v["old_fingerprint"], v["new_fingerprint"]);
    assert!(v["next"].as_str().unwrap().contains("alf sync"));
    assert_eq!(
        v["new_key_written_to"].as_str().unwrap(),
        key_path.to_str().unwrap()
    );
    assert_ne!(
        fs::read_to_string(&key_path).unwrap(),
        old_key_content,
        "the default key file must hold the new key"
    );

    // No-flag decrypt under the new key.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("rot-label")
        .arg("--yes-insecure")
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["payload"]["secret"], "rot-secret");

    // Fabricate the crash-after-step-2 window: re-encrypt the vault under a
    // key that exists only at `<keypath>.new` (exactly what --new-key-out to
    // that path produces), leaving the dead key at the default path.
    let pending = key_path.with_file_name(".alf-vault-key.new");
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("rotate-key")
        .arg("-r")
        .arg("openclaw")
        .arg("--new-key-out")
        .arg(&pending)
        .assert()
        .success();
    assert!(pending.is_file());

    // The next rotation self-heals (recovered: true) and completes.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("rotate-key")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["recovered"], true);
    assert!(!pending.exists(), "recovery must consume the .new file");

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("rot-label")
        .arg("--yes-insecure")
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["payload"]["secret"], "rot-secret");
}

/// M-1 (DoD): a legacy single-agent install migrates transparently on the
/// first bare vault command — the vault and key files relocate verbatim, and
/// flag-less default key resolution keeps opening pre-migration records.
#[test]
fn legacy_single_agent_migrates_transparently_no_flags() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    // Pre-migration state: a legacy vault holding one record sealed under
    // what becomes the legacy install-scoped key (seeded via --in and a
    // scratch key file, so the seeding itself never trips the migration
    // gate), plus that key at the legacy path.
    let kf = tmp.path().join("seed.key");
    keygen_at(&home, &kf);
    let legacy_vault = home.join(".alf").join("vault").join("credentials.json");
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--in")
        .arg(&legacy_vault)
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("legacy-label")
        .arg("--secret")
        .arg("legacy-secret")
        .arg("--vault-key-file")
        .arg(&kf)
        .assert()
        .success();
    let legacy_key = home.join(".openclaw").join("state").join(".alf-vault-key");
    fs::create_dir_all(legacy_key.parent().unwrap()).unwrap();
    fs::copy(&kf, &legacy_key).unwrap();
    let legacy_key_content = fs::read_to_string(&legacy_key).unwrap();

    // The upgrade: a mapping row appears (as alf check would create it).
    let ws = tmp.path().join("ws");
    let id = "cfef1150-7777-4777-8777-000000000001";
    write_config(&home, false, None, &[("main", id, &ws, true)]);

    // Bare add (no key flags): migrates, then encrypts under the migrated
    // per-agent default key.
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("github")
        .arg("--label")
        .arg("new-label")
        .arg("--secret")
        .arg("new-secret")
        .assert()
        .success();

    // Files relocated; key bytes unchanged.
    assert!(!legacy_vault.exists(), "legacy vault must be gone");
    assert!(!legacy_key.exists(), "legacy key must be gone");
    assert!(agent_vault(&home, id).is_file());
    assert_eq!(
        fs::read_to_string(agent_key(&home, id)).unwrap(),
        legacy_key_content,
        "key content must move verbatim"
    );

    // Continuity: flag-less decrypt (default key resolution) opens BOTH the
    // pre-migration and post-migration records.
    for (label, secret) in [
        ("legacy-label", "legacy-secret"),
        ("new-label", "new-secret"),
    ] {
        let assert = alf_cmd()
            .env("HOME", &home)
            .arg("vault")
            .arg("decrypt")
            .arg("-r")
            .arg("openclaw")
            .arg("--label")
            .arg(label)
            .arg("--yes-insecure")
            .assert()
            .success();
        assert_eq!(json_stdout(&assert)["payload"]["secret"], secret);
    }

    // Idempotence: a second bare command does not re-migrate or fail.
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("list")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .success();
}

/// M-2: two enabled agents block the automatic migration with the exact
/// `alf vault migrate` remedy; following it resolves the block.
#[test]
fn legacy_two_enabled_blocks_with_migrate_remedy() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws_a = tmp.path().join("ws-a");
    let ws_b = tmp.path().join("ws-b");
    let a = "cfef1150-8888-4888-8888-00000000000a";
    let b = "cfef1150-8888-4888-8888-00000000000b";
    write_config(
        &home,
        false,
        None,
        &[("main", a, &ws_a, true), ("helper", b, &ws_b, true)],
    );

    // Legacy vault + key (seeded via --in, which stays lenient).
    let kf = tmp.path().join("seed.key");
    keygen_at(&home, &kf);
    let legacy_vault = home.join(".alf").join("vault").join("credentials.json");
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--in")
        .arg(&legacy_vault)
        .arg("--service")
        .arg("email")
        .arg("--label")
        .arg("legacy-label")
        .arg("--secret")
        .arg("legacy-secret")
        .arg("--vault-key-file")
        .arg(&kf)
        .assert()
        .success();
    let legacy_key = home.join(".openclaw").join("state").join(".alf-vault-key");
    fs::create_dir_all(legacy_key.parent().unwrap()).unwrap();
    fs::copy(&kf, &legacy_key).unwrap();

    // Even an explicit --agent on the triggering command must NOT pick the
    // migration target (D5) — the op blocks with the migrate remedy.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg("main")
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("x")
        .arg("--secret")
        .arg("y")
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert_eq!(v["code"], "vault_migration_blocked");
    assert!(v["hint"].as_str().unwrap().contains("alf vault migrate"));
    assert!(legacy_vault.is_file(), "nothing moved while blocked");

    // The human decision resolves it.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg("main")
        .arg("vault")
        .arg("migrate")
        .arg("-r")
        .arg("openclaw")
        .assert()
        .success();
    let v = json_stdout(&assert);
    assert_eq!(v["ok"], true);
    assert_eq!(v["agent_id"], a);
    assert!(!legacy_vault.exists());
    assert!(agent_vault(&home, a).is_file());
    assert!(agent_key(&home, a).is_file());

    // And the blocked command now works with the migrated default key.
    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("--agent")
        .arg("main")
        .arg("vault")
        .arg("decrypt")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("legacy-label")
        .arg("--yes-insecure")
        .assert()
        .success();
    assert_eq!(json_stdout(&assert)["payload"]["secret"], "legacy-secret");
}

/// M-3: another runtime's legacy key blocks the automatic migration even
/// with a sole enabled agent (the legacy vault is runtime-neutral).
#[test]
fn cross_runtime_evidence_blocks() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let a = "cfef1150-9999-4999-8999-00000000000a";
    write_config(&home, false, None, &[("main", a, &ws, true)]);

    let kf = tmp.path().join("seed.key");
    keygen_at(&home, &kf);
    let legacy_vault = home.join(".alf").join("vault").join("credentials.json");
    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--in")
        .arg(&legacy_vault)
        .arg("--service")
        .arg("email")
        .arg("--secret")
        .arg("s")
        .arg("--vault-key-file")
        .arg(&kf)
        .assert()
        .success();
    // Cross-runtime evidence: a zeroclaw legacy key.
    keygen_at(
        &home,
        &home.join(".zeroclaw").join("state").join(".alf-vault-key"),
    );

    let assert = alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("add")
        .arg("-r")
        .arg("openclaw")
        .arg("--service")
        .arg("x")
        .arg("--secret")
        .arg("y")
        .arg("--vault-key-file")
        .arg(&kf)
        .assert()
        .failure();
    let v = json_stdout(&assert);
    assert_eq!(v["code"], "vault_migration_blocked");
    assert!(
        v["error"].as_str().unwrap().contains("zeroclaw"),
        "must name the other runtime's evidence: {v}"
    );
    assert!(v["hint"].as_str().unwrap().contains("alf vault migrate"));
}

/// M-5: unknown doc-level extra fields survive a `vault delete` rewrite
/// (forward-compat hygiene: the rewrite round-trips fields it doesn't know).
#[test]
fn doc_extra_survives_vault_delete_rewrite() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ws = tmp.path().join("ws");
    let id = "cfef1150-aaaa-4aaa-8aaa-0000000000f5";
    write_config(&home, false, None, &[("main", id, &ws, true)]);
    keygen_at(&home, &agent_key(&home, id));

    let add = |label: &str| {
        alf_cmd()
            .env("HOME", &home)
            .arg("vault")
            .arg("add")
            .arg("-r")
            .arg("openclaw")
            .arg("--service")
            .arg("email")
            .arg("--label")
            .arg(label)
            .arg("--secret")
            .arg("s")
            .assert()
            .success();
    };
    add("first");
    add("second");

    // Inject an unknown doc-level field (as a future alf version might).
    let vault_path = agent_vault(&home, id);
    let mut doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&vault_path).unwrap()).unwrap();
    doc["future_doc_field"] = serde_json::json!("kept");
    fs::write(&vault_path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    alf_cmd()
        .env("HOME", &home)
        .arg("vault")
        .arg("delete")
        .arg("-r")
        .arg("openclaw")
        .arg("--label")
        .arg("first")
        .assert()
        .success();

    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&vault_path).unwrap()).unwrap();
    assert_eq!(doc["credentials"].as_array().unwrap().len(), 1);
    assert_eq!(
        doc["future_doc_field"], "kept",
        "unknown doc-level extra must survive the delete rewrite"
    );
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
        .arg("--agent")
        .arg("ee8c59c6-0424-4cd2-b89c-19d4609bbcdf")
        .arg("--dry-run")
        .assert()
        .failure();

    // clap accepted --dry-run; the failure is the runtime missing-key error.
    assert_eq!(json_stdout(&assert)["ok"], false);

    // The preview must not have created the workspace.
    assert!(!workspace.exists());
}
