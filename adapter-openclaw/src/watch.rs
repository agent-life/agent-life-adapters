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
//! In-workspace tracked files use their own tracked source so the engine applies
//! `tracked_files_interval` before the export chooses its §6.1 full-snapshot
//! rollover. The recursive workspace source retains the normal cadence for the
//! rest of OpenClaw's broad Markdown surface.

use std::fs;
use std::path::{Component, Path, PathBuf};

use alf_core::include::{
    is_denylisted, safe_include_path, IncludeList, INCLUDE_FILE, SYNC_LOG_FILE,
};
use alf_core::WatchSpec;

/// Build the OpenClaw watch surface for `workspace`.
pub fn watch_paths(workspace: &Path) -> Vec<WatchSpec> {
    let tracked_roots = tracked_roots(workspace);
    let control_roots = vec![workspace.join(INCLUDE_FILE), workspace.join(SYNC_LOG_FILE)];

    // The whole workspace stays responsible for ordinary Markdown, but it must
    // not also classify explicit tracked inputs as untracked.
    let mut workspace_spec =
        WatchSpec::dir("workspace", workspace.to_path_buf()).excluding([workspace.join(".git")]);
    workspace_spec.exclude.extend(tracked_roots.iter().cloned());
    workspace_spec.exclude.extend(control_roots.iter().cloned());
    let mut specs = vec![workspace_spec];

    if !tracked_roots.is_empty() {
        specs.push(WatchSpec {
            id: "tracked-files".into(),
            roots: tracked_roots,
            recursive: false,
            exclude: Vec::new(),
            tracked: true,
            sqlite: false,
            rediscover: false,
            resurface: true,
        });
    }

    // The include list changes the tracked surface; the sync log is also an
    // exported tracked input. Both follow the tracked-file cadence.
    specs.push(WatchSpec {
        id: "tracked-controls".into(),
        roots: control_roots,
        recursive: false,
        exclude: Vec::new(),
        tracked: true,
        sqlite: false,
        rediscover: false,
        resurface: true,
    });

    // Out-of-workspace: the OpenClaw config drives agent-set/workspace/version.
    if let Some(config) = openclaw_config_path(workspace) {
        specs.push(WatchSpec::file("openclaw-config", config).resurfacing());
    }

    specs
}

/// Return the optional, in-workspace tracked roots that OpenClaw can export.
/// Missing entries remain roots so their later creation/deletion is observable;
/// entries which could escape the workspace or never be exported are ignored.
fn tracked_roots(workspace: &Path) -> Vec<PathBuf> {
    IncludeList::load(workspace)
        .map(|list| {
            list.paths()
                .into_iter()
                .filter_map(|path| optional_tracked_root(workspace, &path))
                .collect()
        })
        .unwrap_or_default()
}

/// Validate an include entry for watching without requiring the optional file
/// to exist yet. Existing paths reuse export's full validation; a missing path
/// is accepted only when its lexical path and nearest existing ancestor both
/// remain inside the workspace.
fn optional_tracked_root(workspace: &Path, path: &str) -> Option<PathBuf> {
    let workspace_canon = workspace.canonicalize().ok()?;
    let mut candidate = workspace.to_path_buf();
    let mut has_normal_component = false;
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => {
                candidate.push(part);
                has_normal_component = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if !has_normal_component
        || candidate == workspace.join(INCLUDE_FILE)
        || candidate == workspace.join(SYNC_LOG_FILE)
        || is_denylisted(&candidate)
    {
        return None;
    }

    match fs::symlink_metadata(&candidate) {
        // `safe_include_path` resolves symlinks, rejects non-files and enforces
        // the same sentinels/denylist/containment policy used by export.
        Ok(_) => safe_include_path(workspace, path).ok().map(|_| candidate),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let ancestor = candidate
                .parent()?
                .ancestors()
                .find_map(|parent| parent.canonicalize().ok())?;
            (ancestor.is_dir() && ancestor.starts_with(workspace_canon)).then_some(candidate)
        }
        Err(_) => None,
    }
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
    fn tracked_inputs_get_the_tracked_channel_and_leave_workspace_untracked() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        fs::create_dir_all(ws.join("notes")).unwrap();
        fs::write(ws.join("notes/selected.md"), "selected").unwrap();
        fs::write(
            ws.join(alf_core::include::INCLUDE_FILE),
            r#"{"files":[
                {"path":"notes/selected.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"notes/later.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"../outside.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"AGENTS.md","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/proj/AGENTS.md","verified":true}
            ]}"#,
        )
        .unwrap();

        let specs = watch_paths(ws);
        let tracked = by_id(&specs, "tracked-files").expect("tracked spec");
        assert!(tracked.tracked);
        assert!(tracked.resurface, "optional roots must refresh the surface");
        assert!(tracked.roots.contains(&ws.join("notes/selected.md")));
        assert!(tracked.roots.contains(&ws.join("notes/later.md")));
        assert!(
            !tracked
                .roots
                .iter()
                .any(|root| root.ends_with("outside.md")),
            "an escaping include entry must not create a watch root"
        );
        assert!(
            !tracked
                .roots
                .contains(&PathBuf::from("/etc/proj/AGENTS.md")),
            "OpenClaw does not export external entries, so it must not watch them"
        );

        let controls = by_id(&specs, "tracked-controls").expect("tracked controls");
        assert!(controls.tracked);
        assert!(controls.resurface);
        assert_eq!(
            controls.roots,
            vec![
                ws.join(alf_core::include::INCLUDE_FILE),
                ws.join(alf_core::include::SYNC_LOG_FILE),
            ]
        );

        let workspace = by_id(&specs, "workspace").expect("workspace spec");
        for root in tracked.roots.iter().chain(&controls.roots) {
            assert!(
                workspace.exclude.contains(root),
                "the broad workspace spec must not also dirty {root:?}"
            );
        }
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
    fn externals_never_become_openclaw_tracked_roots() {
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
        let tracked = by_id(&specs, "tracked-files").expect("in-workspace tracked spec");
        assert!(tracked.roots.contains(&ws.join("notes.md")));
        assert!(
            !tracked
                .roots
                .contains(&PathBuf::from("/etc/proj/AGENTS.md")),
            "OpenClaw does not export external entries, so it must not watch them"
        );
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
    fn in_workspace_include_entry_gets_a_tracked_spec() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join(alf_core::include::INCLUDE_FILE),
            r#"{"files":[{"path":"notes.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true}]}"#,
        )
        .unwrap();
        let specs = watch_paths(ws);
        let tracked = by_id(&specs, "tracked-files").expect("tracked spec");
        assert!(tracked.tracked);
        assert_eq!(tracked.roots, vec![ws.join("notes.md")]);
    }
}
