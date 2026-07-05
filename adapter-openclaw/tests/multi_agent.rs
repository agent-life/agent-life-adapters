//! WP4 multi-agent discovery + per-agent isolation for the OpenClaw adapter.
//!
//! OpenClaw is directory-isolated: each agent owns a `workspace-<name>/` subtree,
//! declared in `openclaw.json` `agents.list[]`. These tests exercise the
//! `discover_agents` override (the only production change) and confirm that a
//! per-agent export/import carries exactly one agent's memory.

use adapter_openclaw::OpenClawAdapter;
use alf_core::{Adapter, MemorySource};
use std::fs;
use tempfile::TempDir;

mod common;

/// `agents.list[]` → one binding per agent: named agents keep their explicit
/// absolute `workspace`; the workspace-less `main` uses `<root>/workspace`
/// (the default agent's convention on OpenClaw 2026.6.11).
#[test]
fn discover_agents_from_agents_list() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".openclaw");
    let ws_a = root.join("workspace-agent_a");
    let ws_b = root.join("workspace-agent_b");
    fs::create_dir_all(&ws_a).unwrap();
    fs::create_dir_all(&ws_b).unwrap();
    let json = format!(
        r#"{{
          "agents": {{
            "list": [
              {{ "id": "main" }},
              {{ "id": "agent_a", "name": "agent_a", "workspace": "{a}" }},
              {{ "id": "agent_b", "name": "agent_b", "workspace": "{b}" }}
            ]
          }}
        }}"#,
        a = ws_a.display(),
        b = ws_b.display(),
    );
    fs::write(root.join("openclaw.json"), &json).unwrap();

    // `install` is a workspace dir under the root; discovery walks up to
    // openclaw.json (never reads the real ~/.openclaw).
    let bindings = OpenClawAdapter
        .discover_agents(&root.join("workspace-main"))
        .unwrap();

    assert_eq!(bindings.len(), 3);
    for b in &bindings {
        assert_eq!(b.memory_source, MemorySource::InWorkspaceFiles);
        assert_eq!(
            b.runtime_agent_id, None,
            "OpenClaw has no separate runtime id"
        );
        assert!(
            b.default_enabled,
            "declared OpenClaw agents are user-configured"
        );
    }
    let by_alias = |alias: &str| {
        bindings
            .iter()
            .find(|b| b.runtime_agent == alias)
            .unwrap_or_else(|| panic!("no binding for {alias}"))
    };
    assert_eq!(
        by_alias("main").workspace,
        root.join("workspace"),
        "workspace-less main uses <root>/workspace"
    );
    assert_eq!(
        by_alias("agent_a").workspace,
        ws_a,
        "explicit workspace honored"
    );
    assert_eq!(by_alias("agent_b").workspace, ws_b);
}

/// A user-relocated default workspace (`agents.defaults.workspace`) is honored
/// for the workspace-less `main` entry — the adapter agrees with `alf check`'s
/// `read_openclaw_workspace`, which reads the same field.
#[test]
fn discover_agents_honors_default_workspace_for_main() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".openclaw");
    let main_ws = root.join("main-ws");
    fs::create_dir_all(&main_ws).unwrap();
    let json = format!(
        r#"{{ "agents": {{ "defaults": {{ "workspace": "{ws}" }},
             "list": [ {{ "id": "main" }} ] }} }}"#,
        ws = main_ws.display(),
    );
    fs::write(root.join("openclaw.json"), &json).unwrap();

    let bindings = OpenClawAdapter
        .discover_agents(&root.join("main-ws"))
        .unwrap();

    assert_eq!(bindings.len(), 1);
    assert_eq!(
        bindings[0].workspace, main_ws,
        "main honors agents.defaults.workspace"
    );
}

/// No/empty `openclaw.json` ⇒ the WP0 single-`main` zero-friction fallback,
/// workspace = the passed install path.
#[test]
fn discover_agents_falls_back_to_single_main() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();

    let bindings = OpenClawAdapter.discover_agents(&ws).unwrap();

    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].runtime_agent, "main");
    assert_eq!(bindings[0].workspace, ws);
    assert_eq!(bindings[0].memory_source, MemorySource::InWorkspaceFiles);
    assert!(bindings[0].default_enabled);
}

/// A per-agent export carries only that agent's memory, and round-trips its
/// tracked `MEMORY.md` byte-identically — the DoD isolation guarantee at the
/// adapter level (the harness proves it end-to-end via markers).
#[test]
fn two_agent_export_is_isolated_and_round_trips() {
    common::isolate_home();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".openclaw");
    let ws_a = root.join("workspace-agent_a");
    let ws_b = root.join("workspace-agent_b");
    fs::create_dir_all(&ws_a).unwrap();
    fs::create_dir_all(&ws_b).unwrap();
    fs::write(ws_a.join("SOUL.md"), "# Agent A\n").unwrap();
    fs::write(
        ws_a.join("MEMORY.md"),
        "# Memory\n\n## Colors\n\nagent_a favorite color is teal.\n",
    )
    .unwrap();
    fs::write(ws_b.join("SOUL.md"), "# Agent B\n").unwrap();
    fs::write(
        ws_b.join("MEMORY.md"),
        "# Memory\n\n## Animals\n\nagent_b favorite animal is the otter.\n",
    )
    .unwrap();

    let adapter = OpenClawAdapter;
    let alf_a = tmp.path().join("a.alf");
    let alf_b = tmp.path().join("b.alf");
    adapter.export(&ws_a, &alf_a).unwrap();
    adapter.export(&ws_b, &alf_b).unwrap();

    let restored_a = tmp.path().join("restored-a");
    let restored_b = tmp.path().join("restored-b");
    adapter.import(&alf_a, &restored_a).unwrap();
    adapter.import(&alf_b, &restored_b).unwrap();

    let mem_a = fs::read_to_string(restored_a.join("MEMORY.md")).unwrap();
    let mem_b = fs::read_to_string(restored_b.join("MEMORY.md")).unwrap();
    assert!(mem_a.contains("teal"));
    assert!(
        !mem_a.contains("otter"),
        "agent_a's archive must not contain agent_b's memory"
    );
    assert!(mem_b.contains("otter"));
    assert!(
        !mem_b.contains("teal"),
        "agent_b's archive must not contain agent_a's memory"
    );

    assert_eq!(
        fs::read(ws_a.join("MEMORY.md")).unwrap(),
        fs::read(restored_a.join("MEMORY.md")).unwrap(),
        "agent_a MEMORY.md round-trips byte-identically"
    );
    assert_eq!(
        fs::read(ws_b.join("MEMORY.md")).unwrap(),
        fs::read(restored_b.join("MEMORY.md")).unwrap(),
    );
}
