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
