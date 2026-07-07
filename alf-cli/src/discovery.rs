//! Agent discovery + mapping reconcile (WP0).
//!
//! `alf check` (and the selector's first-contact lazy init) discovers the
//! agents in an install via `Adapter::discover_agents`, reconciles them
//! against the persisted `[[agents]]` mapping, and persists the outcome.
//!
//! Invariants (design §6):
//! - `alf_agent_id` is written once per row and never re-minted.
//! - Re-check never flips `enabled` — discovery is information-only; enabling
//!   is always explicit (`alf agents enable`).
//! - Removed agents stay in the mapping, reported only.
//! - Drift (a recreated agent, or a workspace `.alf-agent-id` that disagrees
//!   with the mapping) is warn-only here; `alf sync` fails closed on it.

use crate::config::{AgentEntry, Config};
use crate::output;
use crate::state;

use alf_core::adapter::{Adapter, AgentBinding, AGENT_ID_FILE};

use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Identity probes gathered before reconcile. Pure inputs — the unit-test seam
/// for the allocation/stability matrix.
pub struct AllocationContext {
    /// `{binding.workspace}/.alf-agent-id` probes (present files only).
    pub workspace_ids: BTreeMap<PathBuf, Uuid>,
    /// `adapter.resolve_agent_id` per binding workspace. Must contain every
    /// discovered binding's workspace.
    pub derived_ids: BTreeMap<PathBuf, Uuid>,
    /// IDs found in `~/.alf/state/*.toml` (pre-WP0 synced installs).
    pub state_ids: Vec<Uuid>,
    /// IDs owned by mapping rows of OTHER runtimes (id → owning runtime).
    /// Adoption must never take one of these — `upsert_agent` keys globally
    /// by `alf_agent_id`, so a cross-runtime adoption would silently replace
    /// the other runtime's row.
    pub foreign_ids: BTreeMap<Uuid, String>,
}

/// How a reconciled row relates to the persisted mapping.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowStatus {
    Existing,
    New,
    Removed,
    Drift,
}

/// One reconciled mapping row plus its status.
pub struct ReconciledRow {
    pub entry: AgentEntry,
    pub status: RowStatus,
}

/// Warn-only identity-drift report (DoD 4).
#[derive(Serialize, schemars::JsonSchema)]
pub struct DriftWarning {
    pub runtime_agent: String,
    pub message: String,
    pub remedy: String,
}

/// Result of reconciling a discovery pass against the mapping.
pub struct ReconcileOutcome {
    pub rows: Vec<ReconciledRow>,
    pub drift: Vec<DriftWarning>,
    pub first_run: bool,
}

/// PURE reconcile of discovered bindings against the existing mapping rows for
/// one runtime.
///
/// Matching rules, in order per binding: (1) `runtime_agent_id` equality
/// (authoritative for shared stores); (2) workspace path equality; (3)
/// `runtime_agent` alias equality. A match whose stored and discovered
/// `runtime_agent_id` are both `Some` but differ is a recreated agent →
/// Drift, row untouched. A sole unmatched existing row is adopted into the
/// workspace-matching (else unique `default_enabled`) binding so the
/// `alf_agent_id` survives an alias change (the WP3–5 anti-rework seam).
pub fn reconcile(
    existing: &[AgentEntry],
    discovered: &[AgentBinding],
    runtime: &str,
    ctx: &AllocationContext,
) -> ReconcileOutcome {
    let first_run = existing.is_empty();
    let mut rows: Vec<ReconciledRow> = Vec::new();
    let mut drift: Vec<DriftWarning> = Vec::new();

    let mut row_matched: Vec<bool> = vec![false; existing.len()];
    let mut binding_row: Vec<Option<usize>> = vec![None; discovered.len()];

    // Rules 1–3, per binding.
    for (bi, b) in discovered.iter().enumerate() {
        let found = existing
            .iter()
            .enumerate()
            .filter(|(ri, _)| !row_matched[*ri])
            .find(|(_, r)| r.runtime_agent_id.is_some() && r.runtime_agent_id == b.runtime_agent_id)
            .or_else(|| {
                existing
                    .iter()
                    .enumerate()
                    .filter(|(ri, _)| !row_matched[*ri])
                    .find(|(_, r)| Path::new(&r.workspace) == b.workspace)
            })
            .or_else(|| {
                existing
                    .iter()
                    .enumerate()
                    .filter(|(ri, _)| !row_matched[*ri])
                    .find(|(_, r)| r.runtime_agent == b.runtime_agent)
            });
        if let Some((ri, _)) = found {
            row_matched[ri] = true;
            binding_row[bi] = Some(ri);
        }
    }

    // Sole-row adoption: exactly one existing row, unmatched by rules 1–3 →
    // carry its identity into the binding that workspace-matches, else the
    // unique default_enabled binding. Guarantees id continuity when WP3–5
    // replace the WP0 fallback alias with the real one.
    let mut adopted_binding: Option<usize> = None;
    if existing.len() == 1 && !row_matched[0] {
        let unmatched: Vec<usize> = (0..discovered.len())
            .filter(|bi| binding_row[*bi].is_none())
            .collect();
        let candidate = unmatched
            .iter()
            .copied()
            .find(|bi| Path::new(&existing[0].workspace) == discovered[*bi].workspace)
            .or_else(|| {
                let enabled: Vec<usize> = unmatched
                    .iter()
                    .copied()
                    .filter(|bi| discovered[*bi].default_enabled)
                    .collect();
                (enabled.len() == 1).then(|| enabled[0])
            });
        if let Some(bi) = candidate {
            row_matched[0] = true;
            binding_row[bi] = Some(0);
            adopted_binding = Some(bi);
        }
    }

    // IDs already spoken for: existing rows plus New allocations as they land.
    let mut used_ids: Vec<Uuid> = existing.iter().map(|r| r.alf_agent_id).collect();

    for (bi, b) in discovered.iter().enumerate() {
        match binding_row[bi] {
            Some(ri) => {
                let row = &existing[ri];

                // Recreated agent: stored and discovered runtime ids both
                // present but different. Row untouched; warn only.
                let recreated = matches!(
                    (&row.runtime_agent_id, &b.runtime_agent_id),
                    (Some(x), Some(y)) if x != y
                );
                if recreated {
                    let x = row.runtime_agent_id.as_deref().unwrap_or_default();
                    let y = b.runtime_agent_id.as_deref().unwrap_or_default();
                    drift.push(DriftWarning {
                        runtime_agent: row.runtime_agent.clone(),
                        message: format!(
                            "runtime agent '{}' was recreated: its runtime id changed \
                             from {} to {}. The mapping keeps the original identity.",
                            row.runtime_agent, x, y
                        ),
                        remedy: format!(
                            "Edit ~/.alf/config.toml if the new agent should take over this \
                             mapping, or `alf purge --agent {}` and re-enable to start fresh.",
                            row.alf_agent_id
                        ),
                    });
                    rows.push(ReconciledRow {
                        entry: row.clone(),
                        status: RowStatus::Drift,
                    });
                    continue;
                }

                // Workspace identity drift: the probed `.alf-agent-id` exists
                // and disagrees with the mapping. Row untouched; warn only.
                if let Some(ws_id) = ctx.workspace_ids.get(&b.workspace) {
                    if *ws_id != row.alf_agent_id {
                        drift.push(DriftWarning {
                            runtime_agent: row.runtime_agent.clone(),
                            message: format!(
                                "the workspace's {} ({}) does not match the mapping ({}) — \
                                 likely restored from a different agent or recreated.",
                                AGENT_ID_FILE, ws_id, row.alf_agent_id
                            ),
                            remedy: format!(
                                "Run: echo {} > {} to keep the mapped history, or run \
                                 `alf check` after intentional changes.",
                                row.alf_agent_id,
                                b.workspace.join(AGENT_ID_FILE).display()
                            ),
                        });
                        rows.push(ReconciledRow {
                            entry: row.clone(),
                            status: RowStatus::Drift,
                        });
                        continue;
                    }
                }

                // Matched: refresh non-identity fields. `alf_agent_id` and
                // `enabled` are never changed by discovery (design §6). The
                // alias refreshes only via sole-row adoption.
                let mut entry = row.clone();
                entry.workspace = b.workspace.to_string_lossy().into_owned();
                entry.runtime_agent_id = b.runtime_agent_id.clone();
                if adopted_binding == Some(bi) {
                    entry.runtime_agent = b.runtime_agent.clone();
                }
                rows.push(ReconciledRow {
                    entry,
                    status: RowStatus::Existing,
                });
            }
            None => {
                // New agent. Allocation order: (a) adopt the workspace's own
                // `.alf-agent-id` when unused; (b) first-run sole-binding
                // installs adopt a sole pre-WP0 state id (cloud identity +
                // delta continuity); (c) the adapter's deterministic
                // derivation — converges with a later bare export.
                // An id owned by another runtime's row is never adopted
                // (rule a warns; rule b falls through silently).
                let mut adopt_ws = ctx
                    .workspace_ids
                    .get(&b.workspace)
                    .copied()
                    .filter(|id| !used_ids.contains(id));
                if let Some(id) = adopt_ws {
                    if let Some(owner) = ctx.foreign_ids.get(&id) {
                        drift.push(DriftWarning {
                            runtime_agent: b.runtime_agent.clone(),
                            message: format!(
                                "the workspace's {} ({}) already identifies an agent mapped \
                                 for runtime '{}' — a new id was derived for this row instead.",
                                AGENT_ID_FILE, id, owner
                            ),
                            remedy: format!(
                                "If this workspace was intentionally migrated from {}, move \
                                 that row in ~/.alf/config.toml; otherwise remove {} and \
                                 re-run alf check.",
                                owner,
                                b.workspace.join(AGENT_ID_FILE).display()
                            ),
                        });
                        adopt_ws = None;
                    }
                }
                let alf_agent_id = adopt_ws
                    .or_else(|| {
                        (first_run && discovered.len() == 1 && ctx.state_ids.len() == 1)
                            .then(|| ctx.state_ids[0])
                            .filter(|id| !ctx.foreign_ids.contains_key(id))
                    })
                    .unwrap_or_else(|| {
                        *ctx.derived_ids
                            .get(&b.workspace)
                            .expect("AllocationContext invariant: derived_ids covers every binding")
                    });
                used_ids.push(alf_agent_id);

                rows.push(ReconciledRow {
                    entry: AgentEntry {
                        runtime: Some(runtime.to_string()),
                        runtime_agent: b.runtime_agent.clone(),
                        runtime_agent_id: b.runtime_agent_id.clone(),
                        alf_agent_id,
                        workspace: b.workspace.to_string_lossy().into_owned(),
                        // First run applies the adapter's classification;
                        // re-check is info-only — enabling is always explicit.
                        enabled: first_run && b.default_enabled,
                        extra: BTreeMap::new(),
                    },
                    status: RowStatus::New,
                });
            }
        }
    }

    // Rows no longer discovered stay in the mapping, reported only.
    for (ri, row) in existing.iter().enumerate() {
        if !row_matched[ri] {
            rows.push(ReconciledRow {
                entry: row.clone(),
                status: RowStatus::Removed,
            });
        }
    }

    ReconcileOutcome {
        rows,
        drift,
        first_run,
    }
}

/// Gather identity probes (fs reads + state-dir scan + adapter derivation) for
/// an install and reconcile against the mapping.
pub fn discover_and_reconcile(
    config: &Config,
    adapter: &dyn Adapter,
    runtime: &str,
    install: &Path,
) -> Result<ReconcileOutcome> {
    let discovered = adapter.discover_agents(install)?;
    let existing: Vec<AgentEntry> = config
        .agents_for_runtime(runtime)
        .into_iter()
        .cloned()
        .collect();

    let mut workspace_ids = BTreeMap::new();
    let mut derived_ids = BTreeMap::new();
    for b in &discovered {
        if let Some(id) = read_workspace_agent_id(&b.workspace) {
            workspace_ids.insert(b.workspace.clone(), id);
        }
        derived_ids.insert(b.workspace.clone(), adapter.resolve_agent_id(&b.workspace)?);
    }
    let foreign_ids: BTreeMap<Uuid, String> = config
        .agents
        .iter()
        .filter(|a| a.runtime.as_deref().map(|r| r != runtime).unwrap_or(false))
        .map(|a| (a.alf_agent_id, a.runtime.clone().unwrap_or_default()))
        .collect();
    let ctx = AllocationContext {
        workspace_ids,
        derived_ids,
        state_ids: state::tracked_agent_ids()?,
        foreign_ids,
    };

    Ok(reconcile(&existing, &discovered, runtime, &ctx))
}

/// Persist a reconcile outcome: upsert New rows, refresh matched rows, never
/// touch Removed/Drift rows, and save only when something changed. Then write
/// the row id into any live workspace still missing its `.alf-agent-id` (same
/// write export does, just earlier); a write failure is a warning, not fatal.
pub fn persist(config: &mut Config, outcome: &ReconcileOutcome) -> Result<bool> {
    let mut dirty = false;
    for row in &outcome.rows {
        match row.status {
            RowStatus::New => {
                config.upsert_agent(row.entry.clone());
                dirty = true;
            }
            RowStatus::Existing => {
                let unchanged = config
                    .agents
                    .iter()
                    .any(|a| a.alf_agent_id == row.entry.alf_agent_id && *a == row.entry);
                if !unchanged {
                    config.upsert_agent(row.entry.clone());
                    dirty = true;
                }
            }
            RowStatus::Removed | RowStatus::Drift => {}
        }
    }
    if dirty {
        config.save()?;
    }

    for row in &outcome.rows {
        if !matches!(row.status, RowStatus::New | RowStatus::Existing) {
            continue;
        }
        let ws = Path::new(&row.entry.workspace);
        let id_file = ws.join(AGENT_ID_FILE);
        if ws.is_dir() && !id_file.is_file() {
            if let Err(e) = fs::write(&id_file, row.entry.alf_agent_id.to_string()) {
                output::progress(&format!(
                    "  ! Could not persist agent id to {}: {e}",
                    id_file.display()
                ));
            }
        }
    }

    Ok(dirty)
}

/// Read `{workspace}/.alf-agent-id` if present and parseable.
fn read_workspace_agent_id(workspace: &Path) -> Option<Uuid> {
    let raw = fs::read_to_string(workspace.join(AGENT_ID_FILE)).ok()?;
    Uuid::parse_str(raw.trim()).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alf_core::adapter::MemorySource;

    fn uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    fn binding(alias: &str, workspace: &str) -> AgentBinding {
        AgentBinding {
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            workspace: PathBuf::from(workspace),
            memory_source: MemorySource::InWorkspaceFiles,
            default_enabled: true,
        }
    }

    fn entry(alias: &str, id: Uuid, workspace: &str, enabled: bool) -> AgentEntry {
        AgentEntry {
            runtime: Some("openclaw".into()),
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            alf_agent_id: id,
            workspace: workspace.into(),
            enabled,
            extra: BTreeMap::new(),
        }
    }

    /// A Hermes-shaped binding: profile-isolated, `runtime_agent_id: None`
    /// (matched by workspace/alias), `PerAgentDb` at `<profile>/state.db`.
    fn hermes_binding(alias: &str, workspace: &str) -> AgentBinding {
        AgentBinding {
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            workspace: PathBuf::from(workspace),
            memory_source: MemorySource::PerAgentDb {
                path: PathBuf::from(workspace).join("state.db"),
            },
            default_enabled: true,
        }
    }

    fn hermes_entry(alias: &str, id: Uuid, workspace: &str) -> AgentEntry {
        AgentEntry {
            runtime: Some("hermes".into()),
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            alf_agent_id: id,
            workspace: workspace.into(),
            enabled: true,
            extra: BTreeMap::new(),
        }
    }

    fn ctx() -> AllocationContext {
        AllocationContext {
            workspace_ids: BTreeMap::new(),
            derived_ids: BTreeMap::new(),
            state_ids: Vec::new(),
            foreign_ids: BTreeMap::new(),
        }
    }

    fn ctx_derived(pairs: &[(&str, Uuid)]) -> AllocationContext {
        let mut c = ctx();
        for (ws, id) in pairs {
            c.derived_ids.insert(PathBuf::from(ws), *id);
        }
        c
    }

    #[test]
    fn first_run_adopts_workspace_id() {
        let mut c = ctx_derived(&[("/ws", uuid(0xD1))]);
        c.workspace_ids.insert(PathBuf::from("/ws"), uuid(1));

        let out = reconcile(&[], &[binding("main", "/ws")], "openclaw", &c);
        assert!(out.first_run);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].status, RowStatus::New);
        assert_eq!(out.rows[0].entry.alf_agent_id, uuid(1));
        assert!(
            out.rows[0].entry.enabled,
            "first run applies default_enabled"
        );
    }

    #[test]
    fn first_run_adopts_sole_state_id() {
        let mut c = ctx_derived(&[("/ws", uuid(0xD1))]);
        c.state_ids = vec![uuid(2)];

        let out = reconcile(&[], &[binding("main", "/ws")], "openclaw", &c);
        assert_eq!(
            out.rows[0].entry.alf_agent_id,
            uuid(2),
            "a pre-WP0 synced install keeps its cloud identity"
        );
    }

    #[test]
    fn first_run_derives_id_when_nothing_to_adopt() {
        // Two state ids ⇒ ambiguous ⇒ rule (b) does not apply.
        let mut c = ctx_derived(&[("/ws", uuid(0xD1))]);
        c.state_ids = vec![uuid(2), uuid(3)];

        let out = reconcile(&[], &[binding("main", "/ws")], "openclaw", &c);
        assert_eq!(out.rows[0].entry.alf_agent_id, uuid(0xD1));
    }

    /// A workspace `.alf-agent-id` owned by another runtime's mapping row is
    /// never adopted — `upsert_agent` keys globally, so adoption would
    /// silently replace that row (cross-runtime collision).
    #[test]
    fn first_run_never_adopts_foreign_workspace_id() {
        let mut c = ctx_derived(&[("/ws-zc", uuid(0xD1))]);
        c.workspace_ids.insert(PathBuf::from("/ws-zc"), uuid(1));
        c.foreign_ids.insert(uuid(1), "openclaw".into());

        let out = reconcile(&[], &[binding("default", "/ws-zc")], "zeroclaw", &c);
        assert_eq!(
            out.rows[0].entry.alf_agent_id,
            uuid(0xD1),
            "must derive a fresh id, not steal the openclaw row's id"
        );
        assert_eq!(out.drift.len(), 1, "the denial is surfaced as drift");
        assert!(out.drift[0].message.contains("openclaw"));
        assert!(out.drift[0].remedy.contains("config.toml"));
    }

    /// Same guard for the sole-state-id adoption path (rule b).
    #[test]
    fn first_run_never_adopts_foreign_state_id() {
        let mut c = ctx_derived(&[("/ws-zc", uuid(0xD1))]);
        c.state_ids = vec![uuid(1)];
        c.foreign_ids.insert(uuid(1), "openclaw".into());

        let out = reconcile(&[], &[binding("default", "/ws-zc")], "zeroclaw", &c);
        assert_eq!(out.rows[0].entry.alf_agent_id, uuid(0xD1));
    }

    #[test]
    fn recheck_identical_discovery_all_existing_ids_unchanged_not_dirty() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("ALF_HOME");
        std::env::set_var("ALF_HOME", tmp.path());

        let existing = vec![entry("main", uuid(1), "/ws", true)];
        let out = reconcile(
            &existing,
            &[binding("main", "/ws")],
            "openclaw",
            &ctx_derived(&[("/ws", uuid(0xD1))]),
        );

        let mut config = Config {
            agents: existing.clone(),
            ..Default::default()
        };
        let dirty = persist(&mut config, &out).unwrap();

        match prev {
            Some(v) => std::env::set_var("ALF_HOME", v),
            None => std::env::remove_var("ALF_HOME"),
        }

        assert!(!out.first_run);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].status, RowStatus::Existing);
        assert_eq!(out.rows[0].entry, existing[0]);
        assert!(!dirty, "identical re-discovery must not rewrite the config");
        assert!(
            !tmp.path().join(".alf").join("config.toml").exists(),
            "no save must have happened"
        );
    }

    #[test]
    fn recheck_new_agent_recorded_disabled() {
        let existing = vec![entry("main", uuid(1), "/ws", true)];
        let out = reconcile(
            &existing,
            &[binding("main", "/ws"), binding("helper", "/ws-helper")],
            "openclaw",
            &ctx_derived(&[("/ws", uuid(0xD1)), ("/ws-helper", uuid(0xD2))]),
        );

        let new: Vec<_> = out
            .rows
            .iter()
            .filter(|r| r.status == RowStatus::New)
            .collect();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].entry.runtime_agent, "helper");
        assert!(
            !new[0].entry.enabled,
            "re-check is info-only: new agents are recorded disabled"
        );
    }

    #[test]
    fn recheck_never_flips_enabled() {
        // A disabled row stays disabled even though the binding says
        // default_enabled — that classification applies on first run only.
        let existing = vec![entry("main", uuid(1), "/ws", false)];
        let out = reconcile(
            &existing,
            &[binding("main", "/ws")],
            "openclaw",
            &ctx_derived(&[("/ws", uuid(0xD1))]),
        );
        assert_eq!(out.rows[0].status, RowStatus::Existing);
        assert!(!out.rows[0].entry.enabled);
    }

    #[test]
    fn removed_agent_reported_kept_in_mapping() {
        let existing = vec![
            entry("main", uuid(1), "/ws", true),
            entry("helper", uuid(2), "/ws-helper", true),
        ];
        let out = reconcile(
            &existing,
            &[binding("main", "/ws")],
            "openclaw",
            &ctx_derived(&[("/ws", uuid(0xD1))]),
        );

        let removed: Vec<_> = out
            .rows
            .iter()
            .filter(|r| r.status == RowStatus::Removed)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].entry, existing[1], "removed row is untouched");
    }

    #[test]
    fn sole_row_adoption_on_alias_change() {
        // WP0 wrote alias "default"; WP3+ discovery reports "main". The sole
        // row's alf_agent_id must survive the rename.
        let existing = vec![entry("default", uuid(1), "/ws-old", true)];
        let out = reconcile(
            &existing,
            &[binding("main", "/ws")],
            "openclaw",
            &ctx_derived(&[("/ws", uuid(0xD1))]),
        );

        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].status, RowStatus::Existing);
        assert_eq!(out.rows[0].entry.alf_agent_id, uuid(1), "id continuity");
        assert_eq!(out.rows[0].entry.runtime_agent, "main", "alias adopted");
        assert!(out.rows[0].entry.enabled, "enabled carried over");
    }

    #[test]
    fn openclaw_two_agent_reconcile_by_workspace_path() {
        // Shaped like adapter-openclaw's agents.list[]: `main` at `<root>/workspace`
        // and a named agent at its explicit `workspace-<name>`, both with
        // `runtime_agent_id: None`. To ISOLATE rule 2 (workspace path), discovery
        // reports different aliases than the mapping stored while the workspaces
        // stay put — so rule 1 (runtime id) and rule 3 (alias) both miss and only
        // workspace-path matching can preserve each alf_agent_id.
        let existing = vec![
            entry("main", uuid(1), "/home/u/.openclaw/workspace", true),
            entry(
                "researcher",
                uuid(2),
                "/home/u/.openclaw/workspace-researcher",
                true,
            ),
        ];
        let out = reconcile(
            &existing,
            &[
                binding("primary", "/home/u/.openclaw/workspace"),
                binding("researcher-v2", "/home/u/.openclaw/workspace-researcher"),
            ],
            "openclaw",
            &ctx_derived(&[
                ("/home/u/.openclaw/workspace", uuid(0xD1)),
                ("/home/u/.openclaw/workspace-researcher", uuid(0xD2)),
            ]),
        );

        assert_eq!(out.rows.len(), 2);
        assert!(out.rows.iter().all(|r| r.status == RowStatus::Existing));
        let by_ws = |ws: &str| {
            out.rows
                .iter()
                .find(|r| r.entry.workspace == ws)
                .expect("row for workspace")
        };
        // Matched by workspace path alone; each alf_agent_id preserved.
        assert_eq!(
            by_ws("/home/u/.openclaw/workspace").entry.alf_agent_id,
            uuid(1)
        );
        assert_eq!(
            by_ws("/home/u/.openclaw/workspace-researcher")
                .entry
                .alf_agent_id,
            uuid(2)
        );
        assert!(out.drift.is_empty());
    }

    #[test]
    fn hermes_profiles_reconcile_by_workspace_path() {
        // Hermes is profile-isolated: the default profile's workspace is the
        // home root (`~/.hermes`) and each named profile is `profiles/<name>/`,
        // all `runtime_agent_id: None` + `PerAgentDb`. The default alias is
        // stable ("default"), while a re-checked named profile is reported under
        // a different alias at the SAME workspace path — so for the named row
        // rule 1 (runtime id) and rule 3 (alias) both miss and only workspace-
        // path matching (rule 2) can preserve its alf_agent_id.
        let existing = vec![
            hermes_entry("default", uuid(1), "/home/u/.hermes"),
            hermes_entry("agent_a", uuid(2), "/home/u/.hermes/profiles/agent_a"),
        ];
        let out = reconcile(
            &existing,
            &[
                hermes_binding("default", "/home/u/.hermes"),
                hermes_binding("agent_a_renamed", "/home/u/.hermes/profiles/agent_a"),
            ],
            "hermes",
            &ctx_derived(&[
                ("/home/u/.hermes", uuid(0xD1)),
                ("/home/u/.hermes/profiles/agent_a", uuid(0xD2)),
            ]),
        );

        assert_eq!(out.rows.len(), 2);
        assert!(out.rows.iter().all(|r| r.status == RowStatus::Existing));
        let by_ws = |ws: &str| {
            out.rows
                .iter()
                .find(|r| r.entry.workspace == ws)
                .expect("row for workspace")
        };
        // Default profile at the home root: id preserved.
        assert_eq!(by_ws("/home/u/.hermes").entry.alf_agent_id, uuid(1));
        // Named profile: id preserved by workspace path despite the alias change.
        assert_eq!(
            by_ws("/home/u/.hermes/profiles/agent_a").entry.alf_agent_id,
            uuid(2)
        );
        assert!(out.drift.is_empty());
    }

    #[test]
    fn openclaw_main_id_survives_workspace_string_change() {
        // `main`'s stored workspace string differs from the discovered
        // `<root>/workspace` (e.g. an older mapping, or a default relocated via
        // `agents.defaults.workspace`). With `runtime_agent_id: None`, rule 2
        // (workspace) misses but rule 3 (alias "main") matches — the alf_agent_id
        // survives and the workspace refreshes to the discovered path.
        let existing = vec![entry(
            "main",
            uuid(1),
            "/home/u/.openclaw/workspace-legacy",
            true,
        )];
        let out = reconcile(
            &existing,
            &[binding("main", "/home/u/.openclaw/workspace")],
            "openclaw",
            &ctx_derived(&[("/home/u/.openclaw/workspace", uuid(0xD1))]),
        );

        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].status, RowStatus::Existing);
        assert_eq!(
            out.rows[0].entry.alf_agent_id,
            uuid(1),
            "id survives a workspace-string change via the alias match"
        );
        assert_eq!(
            out.rows[0].entry.workspace, "/home/u/.openclaw/workspace",
            "workspace refreshed to the discovered path"
        );
        assert!(out.drift.is_empty());
    }

    #[test]
    fn zeroclaw_shaped_bindings_reconcile_by_runtime_id() {
        // Shaped like adapter-zeroclaw/testkit/captured/: two agents sharing
        // one brain.db, partitioned by agent_id.
        let shared = MemorySource::SharedDb {
            path: PathBuf::from("/home/u/.zeroclaw/data/memory/brain.db"),
            filter_key: "agent_id".into(),
        };
        let mut b1 = binding("default", "/home/u/.zeroclaw/workspace");
        b1.runtime_agent_id = Some("8423010b-1111-4111-8111-111111111111".into());
        b1.memory_source = shared.clone();
        let mut b2 = binding("researcher", "/home/u/.zeroclaw/workspace-researcher");
        b2.runtime_agent_id = Some("8423010b-2222-4222-8222-222222222222".into());
        b2.memory_source = shared;

        // The mapping knows both, but the default agent's workspace moved —
        // rule 1 (runtime id) must still match it, and refresh the workspace.
        let mut e1 = entry("default", uuid(1), "/home/u/.zeroclaw/old-workspace", true);
        e1.runtime_agent_id = Some("8423010b-1111-4111-8111-111111111111".into());
        let mut e2 = entry(
            "researcher",
            uuid(2),
            "/home/u/.zeroclaw/workspace-researcher",
            false,
        );
        e2.runtime_agent_id = Some("8423010b-2222-4222-8222-222222222222".into());

        let out = reconcile(
            &[e1, e2],
            &[b1, b2],
            "zeroclaw",
            &ctx_derived(&[
                ("/home/u/.zeroclaw/workspace", uuid(0xD1)),
                ("/home/u/.zeroclaw/workspace-researcher", uuid(0xD2)),
            ]),
        );

        assert_eq!(out.rows.len(), 2);
        assert!(out.rows.iter().all(|r| r.status == RowStatus::Existing));
        assert_eq!(out.rows[0].entry.alf_agent_id, uuid(1));
        assert_eq!(out.rows[0].entry.workspace, "/home/u/.zeroclaw/workspace");
        assert_eq!(out.rows[1].entry.alf_agent_id, uuid(2));
        assert!(out.drift.is_empty());
    }

    #[test]
    fn reconcile_drift_on_mismatched_workspace_id() {
        let existing = vec![entry("main", uuid(1), "/ws", true)];
        let mut c = ctx_derived(&[("/ws", uuid(0xD1))]);
        // The workspace file says X, the mapping says Y.
        c.workspace_ids.insert(PathBuf::from("/ws"), uuid(9));

        let out = reconcile(&existing, &[binding("main", "/ws")], "openclaw", &c);

        assert_eq!(out.rows[0].status, RowStatus::Drift);
        assert_eq!(out.rows[0].entry, existing[0], "drift row is untouched");
        assert_eq!(out.drift.len(), 1);
        assert!(out.drift[0].message.contains(&uuid(9).to_string()));
        assert!(out.drift[0].message.contains(&uuid(1).to_string()));
        assert!(out.drift[0].remedy.contains("echo"));
    }

    #[test]
    fn reconcile_drift_on_changed_runtime_agent_id() {
        // Recreated agent: same alias + workspace, new runtime-native id.
        let mut e = entry("default", uuid(1), "/ws", true);
        e.runtime_agent_id = Some("old-runtime-id".into());
        let mut b = binding("default", "/ws");
        b.runtime_agent_id = Some("new-runtime-id".into());

        let out = reconcile(
            &[e.clone()],
            &[b],
            "zeroclaw",
            &ctx_derived(&[("/ws", uuid(0xD1))]),
        );

        assert_eq!(out.rows[0].status, RowStatus::Drift);
        assert_eq!(
            out.rows[0].entry, e,
            "the mapping keeps the original identity"
        );
        assert_eq!(out.drift.len(), 1);
        assert!(out.drift[0].message.contains("recreated"));
        assert!(out.drift[0].message.contains("old-runtime-id"));
        assert!(out.drift[0].message.contains("new-runtime-id"));
    }
}
