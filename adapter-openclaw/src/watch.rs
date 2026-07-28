//! OpenClaw `watch_paths` (design §11.1) — the watch surface the MCP loop
//! monitors for an OpenClaw workspace.
//!
//! OpenClaw's export surface is essentially the *whole workspace*: `ROOT_FILES`
//! (SOUL/IDENTITY/AGENTS/USER/TOOLS/HEARTBEAT/BOOTSTRAP/MEMORY .md), everything
//! under `memory/`, every workspace `*.md` (the scatter-capture rule), the
//! include-list, and the sentinels (`.alf-include.json`, `.alf-sync-log.md`).
//! All of those live *inside* the workspace, so a single recursive workspace
//! watch already covers them — the design's "one recursive workspace watch +
//! the export-time exclusion filter, not per-glob watches".
//!
//! **Exclusion is coarser than export's, deliberately (review D1).** The watch
//! prunes only top-level `.git/`; export additionally honors `.alfignore` and
//! skips *nested* `.git/`. The watch layer's `exclude` is a static prefix list
//! and cannot express `.alfignore`'s dynamic gitignore patterns, so an
//! `.alfignore`'d (or nested-VCS) write does dirty this spec and fire a sync —
//! but **export still honors `.alfignore`**, so that sync finds no real change
//! and returns `no_changes` (a cheap no-op at the delta floor, never a bad
//! sync). Replicating the full gitignore matcher in the notify layer is not
//! worth it for a bounded no-op; matching the two filters exactly is a possible
//! future refinement, not a correctness fix.
//!
//! Two things the export reads live *outside* that recursive root, so they get
//! their own specs (the whole-workspace default would silently miss them):
//!
//! - **`~/.openclaw/openclaw.json`** — the agent set + per-agent workspace +
//!   version. A change here means the discovered topology moved, so it must
//!   re-sync (and, on the CLI, be re-discovered by `alf check`).
//!
//! External include-list entries are deliberately NOT watched: the openclaw
//! export never packs them (`alf add --external` refuses this runtime), so a
//! root for one would fire tracked syncs that capture nothing (MAJ-4/MIN-8).
//!
//! In-workspace tracked files are *already* covered by the recursive workspace
//! watch; the engine governs their §6.1 rollover on the delta cadence (the
//! documented WP-M3 tracked/delta coupling — a whole-workspace export cannot
//! rate-limit one in-workspace file independently), so they need no extra spec.

use std::path::{Path, PathBuf};

use alf_core::WatchSpec;

/// Build the OpenClaw watch surface for `workspace`.
pub fn watch_paths(workspace: &Path) -> Vec<WatchSpec> {
    let mut specs = vec![
        // The whole workspace, recursively, with VCS churn pruned (matches the
        // export's scatter-capture `.git/` skip).
        WatchSpec::dir("workspace", workspace.to_path_buf()).excluding([workspace.join(".git")]),
    ];

    // Out-of-workspace: the OpenClaw config drives agent-set/workspace/version.
    if let Some(config) = openclaw_config_path(workspace) {
        specs.push(WatchSpec::file("openclaw-config", config).resurfacing());
    }

    // No external roots: the openclaw export never packs external include
    // entries (`alf add --external` refuses this runtime), so an external
    // watch root would only fire tracked syncs that capture nothing
    // (MAJ-4/MIN-8). In-workspace tracked files are covered by the recursive
    // workspace watch above.

    specs
}

/// Locate the `openclaw.json` whose agent set this workspace belongs to.
///
/// Install-relative first (walk up to the dir holding `openclaw.json`) — this
/// matches how discovery/version-detection resolve it. If the walk-up finds
/// nothing, it falls back to the real user install `~/.openclaw/openclaw.json`
/// (honoring `ALF_HOME`) **when that file exists** — so on a developer machine
/// with a real OpenClaw install and a workspace that isn't install-relative, the
/// returned spec can point at the real user config (review D3). That is the
/// intended best-effort behavior for a real serve, and harmless: it only adds a
/// read-only watch on a config the server would legitimately want to notice.
fn openclaw_config_path(workspace: &Path) -> Option<PathBuf> {
    for anc in workspace.ancestors().take(6) {
        let p = anc.join("openclaw.json");
        if p.is_file() {
            return Some(p);
        }
    }
    let p = alf_core::home_dir()?
        .join(".openclaw")
        .join("openclaw.json");
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn by_id<'a>(specs: &'a [WatchSpec], id: &str) -> Option<&'a WatchSpec> {
        specs.iter().find(|s| s.id == id)
    }

    #[test]
    fn watch_paths_watches_whole_workspace_excluding_git() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        let specs = watch_paths(ws);
        let workspace = by_id(&specs, "workspace").expect("workspace spec");
        assert!(workspace.recursive);
        assert_eq!(workspace.roots, vec![ws.to_path_buf()]);
        assert!(workspace.exclude.contains(&ws.join(".git")));
        assert!(!workspace.tracked);
    }

    #[test]
    fn watch_paths_includes_out_of_workspace_openclaw_json() {
        // A workspace nested under an install root holding openclaw.json: the
        // walk-up resolves the config, and it is watched as its own file spec.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("openclaw.json"), "{}").unwrap();
        let ws = root.join("workspace");
        fs::create_dir_all(&ws).unwrap();

        let specs = watch_paths(&ws);
        let cfg = by_id(&specs, "openclaw-config").expect("openclaw-config spec");
        assert!(!cfg.recursive);
        assert_eq!(cfg.roots, vec![root.join("openclaw.json")]);
    }

    #[test]
    fn externals_never_yield_a_tracked_spec() {
        // The openclaw export never packs external entries (`alf add
        // --external` refuses this runtime), so externals — however they got
        // into the list (restored archive, hand edit) — must not be watched:
        // a root here would fire tracked syncs that capture nothing
        // (MAJ-4/MIN-8). Even a verified entry stays unwatched.
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join(alf_core::include::INCLUDE_FILE),
            r#"{"files":[
                {"path":"notes.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"AGENTS.md","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/proj/AGENTS.md","verified":true}
            ]}"#,
        )
        .unwrap();

        let specs = watch_paths(ws);
        assert!(by_id(&specs, "tracked-files").is_none());
    }

    #[test]
    fn bare_workspace_yields_the_recursive_workspace_spec_and_no_tracked() {
        // Review D3: a bare workspace (no install-relative openclaw.json, no
        // include file) always yields the recursive workspace spec first and no
        // tracked spec. Whether an `openclaw-config` spec is present depends on the
        // real environment (the documented best-effort `~/.openclaw` fallback), so
        // that is deliberately not asserted here — the env mutation to force it
        // would race the other tests in this binary.
        let tmp = TempDir::new().unwrap();
        let specs = watch_paths(tmp.path());
        assert_eq!(specs[0].id, "workspace");
        assert!(specs[0].recursive);
        assert!(specs[0].exclude.contains(&tmp.path().join(".git")));
        assert!(by_id(&specs, "tracked-files").is_none());
        // Any config spec that did resolve must be a real openclaw.json path, never
        // a workspace-escaping surprise.
        if let Some(cfg) = by_id(&specs, "openclaw-config") {
            assert!(cfg.roots.iter().all(|r| r.ends_with("openclaw.json")));
        }
    }

    #[test]
    fn watch_paths_no_tracked_spec_without_externals() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join(alf_core::include::INCLUDE_FILE),
            r#"{"files":[{"path":"notes.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true}]}"#,
        )
        .unwrap();
        let specs = watch_paths(ws);
        assert!(by_id(&specs, "tracked-files").is_none());
    }
}
