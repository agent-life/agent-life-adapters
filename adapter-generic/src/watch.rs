//! Generic `watch_paths` (design §11.1) — the watch surface the MCP loop
//! monitors for a `.alf-map.json`-driven workspace.
//!
//! The whole-workspace default the trait provides would silently miss the two
//! things generic can track outside a single recursive root: **external**
//! include-list entries (absolute paths under a blessed root) and the exact set
//! of files that matter (map globs + control files). So generic overrides it to
//! yield, per design §11.1:
//!
//! - one spec per `memory_sources[]` entry (the glob's literal base dir, watched
//!   recursively; a metachar-free glob is a single file),
//! - the optional `identity_file`,
//! - a single **tracked-files** spec (§6.1 rollover channel) carrying every
//!   in-workspace include-list path **and** every *verified* external entry's
//!   absolute source (inert restored externals are not packed, so not watched),
//! - a tracked **tracked-controls** spec for `.alf-include.json` and
//!   `.alf-sync-log.md` (both participate in tracked-file snapshot rollover),
//! - an untracked **export-controls** spec for `.alf-map.json` and `.alfignore`
//!   (editing either changes extraction or the watch set).
//!
//! `watch_paths` returns a bare `Vec` (no `Result`), so every fallible read here
//! degrades gracefully: a missing/broken map falls back to one recursive
//! workspace spec plus whatever include-list/sentinel specs still resolve.

use std::path::{Path, PathBuf};

use alf_core::include::{IncludeList, INCLUDE_FILE, SYNC_LOG_FILE};
use alf_core::WatchSpec;

use crate::map::{reject_unsafe_relpath, MemoryMap, MAP_FILE};

/// Build the generic watch surface for `workspace`.
///
/// The map is **validated** before any root is derived (WP-M3 review F1): a
/// hand-edited `.alf-map.json` with a `../`/absolute glob or identity path must
/// not register an inotify watch outside the workspace. An invalid map (or an
/// unsafe individual glob) is skipped, matching the export side's refusal.
pub fn watch_paths(workspace: &Path) -> Vec<WatchSpec> {
    let mut specs = Vec::new();

    match MemoryMap::load(&workspace.join(MAP_FILE)).and_then(|map| {
        map.validate()?; // rejects bad version / non-canonical types / mid-`**` globs / unsafe identity
        Ok(map)
    }) {
        Ok(map) => {
            for src in &map.memory_sources {
                // `validate` does not reject a `..`/absolute glob (only mid-`**`),
                // so guard each glob here before turning it into a watch root.
                if reject_unsafe_relpath(&src.glob).is_ok() {
                    specs.push(source_spec(
                        workspace,
                        &src.id,
                        &src.glob,
                        src.chunking.is_sqlite(),
                    ));
                }
            }
            if let Some(rel) = &map.identity_file {
                // `validate` already rejected an unsafe identity, so this is safe;
                // re-guard defensively.
                if reject_unsafe_relpath(rel).is_ok() {
                    specs.push(WatchSpec::file("identity", workspace.join(rel)));
                }
            }
        }
        Err(_) => {
            // No usable map — fall back to the whole workspace so the loop still
            // reacts to changes; control sources below let a later
            // `alf_configure` be noticed. Exclude `.git/` (its churn would
            // spuriously dirty).
            specs.push(
                WatchSpec::dir("workspace", workspace.to_path_buf())
                    .excluding([workspace.join(".git")]),
            );
        }
    }

    // Tracked-file channel: every in-workspace include-list path + every
    // VERIFIED external entry's absolute source, as one §6.1 rollover unit.
    // Inert (restored, unverified) externals are skipped by the export, so
    // they must not be watch roots either (MAJ-4 / D3).
    if let Ok(list) = IncludeList::load(workspace) {
        let mut roots: Vec<PathBuf> = list.paths().iter().map(|p| workspace.join(p)).collect();
        for ext in list.verified_externals() {
            if let Some(source) = &ext.source {
                roots.push(PathBuf::from(source));
            }
        }
        if !roots.is_empty() {
            specs.push(WatchSpec {
                id: "tracked-files".into(),
                roots,
                recursive: false,
                exclude: Vec::new(),
                tracked: true,
                sqlite: false,
                rediscover: false,
                resurface: false,
            });
        }
    }

    // read_tracked_map compares both of these files for EVERY runtime. Their
    // edits must take the tracked-files cadence and re-derive this surface.
    specs.push(WatchSpec {
        id: "tracked-controls".into(),
        roots: vec![workspace.join(INCLUDE_FILE), workspace.join(SYNC_LOG_FILE)],
        recursive: false,
        exclude: Vec::new(),
        tracked: true,
        sqlite: false,
        rediscover: false,
        resurface: true, // surface-defining (manual §4.3)
    });

    // These controls change extraction or the watch set, but are NOT members
    // of read_tracked_map, so they retain normal cadence.
    specs.push(WatchSpec {
        id: "export-controls".into(),
        roots: vec![workspace.join(MAP_FILE), workspace.join(".alfignore")],
        recursive: false,
        exclude: Vec::new(),
        tracked: false,
        sqlite: false,
        rediscover: false,
        resurface: true, // surface-defining (manual §4.3)
    });

    specs
}

/// A watch spec for one memory source: a metachar-free glob is a literal file;
/// otherwise watch the glob's literal base directory recursively (with `.git/`
/// excluded so its churn doesn't spuriously dirty a root-level glob — G3). A
/// `sqlite_rows` source is flagged `as_sqlite`, and a *literal-glob* sqlite
/// source watches the `.db` **and** its `-wal`/`-shm` sidecars as one spec
/// (WP-G.4): a WAL-mode write often touches only the sidecar, which would
/// otherwise never dirty the source. A glob sqlite source is already covered
/// by its recursive dir root.
fn source_spec(workspace: &Path, id: &str, glob: &str, sqlite: bool) -> WatchSpec {
    let spec = if glob.contains(['*', '?', '[']) {
        let base = glob_base_dir(workspace, glob);
        let git = base.join(".git");
        WatchSpec::dir(id, base).excluding([git])
    } else if sqlite {
        let db = workspace.join(glob);
        WatchSpec {
            id: id.into(),
            roots: vec![
                db.clone(),
                with_suffix(&db, "-wal"),
                with_suffix(&db, "-shm"),
            ],
            recursive: false,
            exclude: Vec::new(),
            tracked: false,
            sqlite: false, // set by `.as_sqlite()` below
            rediscover: false,
            resurface: false,
        }
    } else {
        WatchSpec::file(id, workspace.join(glob))
    };
    if sqlite {
        spec.as_sqlite()
    } else {
        spec
    }
}

/// Append a sidecar suffix (`-wal`/`-shm`) to a `.db` path (the
/// `adapter-zeroclaw` sidecar-trio precedent).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: std::ffi::OsString = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// The leading run of glob components with no metachar, joined under the
/// workspace — the directory whose recursive watch covers the glob.
fn glob_base_dir(workspace: &Path, glob: &str) -> PathBuf {
    let mut base = workspace.to_path_buf();
    for comp in glob.split('/') {
        if comp.is_empty() {
            continue;
        }
        if comp.contains(['*', '?', '[']) {
            break;
        }
        base.push(comp);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_map(ws: &Path, json: &str) {
        fs::write(ws.join(MAP_FILE), json).unwrap();
    }

    #[test]
    fn glob_base_dir_stops_at_first_metachar() {
        let ws = Path::new("/ws");
        assert_eq!(
            glob_base_dir(ws, "memories/*.md"),
            PathBuf::from("/ws/memories")
        );
        assert_eq!(
            glob_base_dir(ws, "knowledge/**/*.md"),
            PathBuf::from("/ws/knowledge")
        );
        assert_eq!(glob_base_dir(ws, "*.md"), PathBuf::from("/ws"));
    }

    #[test]
    fn source_spec_literal_glob_is_a_file() {
        let s = source_spec(Path::new("/ws"), "id", "IDENTITY.md", false);
        assert!(!s.recursive);
        assert!(!s.sqlite);
        assert_eq!(s.roots, vec![PathBuf::from("/ws/IDENTITY.md")]);
    }

    #[test]
    fn sqlite_source_spec_includes_wal_and_shm_roots() {
        // WP-G.4: a literal-glob sqlite source is the sidecar trio in ONE spec —
        // a WAL-mode write that touches only `-wal` must dirty the source.
        let s = source_spec(Path::new("/ws"), "brain", "data/brain.db", true);
        assert!(s.sqlite);
        assert!(!s.recursive);
        assert_eq!(
            s.roots,
            vec![
                PathBuf::from("/ws/data/brain.db"),
                PathBuf::from("/ws/data/brain.db-wal"),
                PathBuf::from("/ws/data/brain.db-shm"),
            ]
        );

        // A *glob* sqlite source keeps its recursive dir root (already covers
        // the sidecars).
        let g = source_spec(Path::new("/ws"), "dbs", "data/*.db", true);
        assert!(g.sqlite);
        assert!(g.recursive);
        assert_eq!(g.roots, vec![PathBuf::from("/ws/data")]);
    }

    #[test]
    fn watch_paths_covers_sources_identity_tracked_and_controls() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        write_map(
            ws,
            r#"{
                "version": 1,
                "identity_file": "IDENTITY.md",
                "memory_sources": [
                    {"id":"journal","glob":"memories/*.md","memory_type":"episodic","namespace":"daily","chunking":"by_heading"},
                    {"id":"kb","glob":"knowledge/**/*.md","memory_type":"semantic","namespace":"curated","chunking":"per_file"}
                ]
            }"#,
        );
        // Two in-workspace tracked files + one external.
        fs::write(
            ws.join(INCLUDE_FILE),
            r#"{"files":[
                {"path":"config.toml","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"secret.txt","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/host/secret.txt","verified":true},
                {"path":"inert.txt","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/host/inert.txt","verified":false}
            ]}"#,
        )
        .unwrap();

        let specs = watch_paths(ws);
        let by_id = |id: &str| specs.iter().find(|s| s.id == id).cloned();

        let journal = by_id("journal").expect("journal source spec");
        assert!(journal.recursive);
        assert_eq!(journal.roots, vec![ws.join("memories")]);

        let kb = by_id("kb").expect("kb source spec");
        assert_eq!(kb.roots, vec![ws.join("knowledge")]);

        let identity = by_id("identity").expect("identity spec");
        assert_eq!(identity.roots, vec![ws.join("IDENTITY.md")]);

        let tracked = by_id("tracked-files").expect("tracked spec");
        assert!(tracked.tracked);
        assert!(tracked.roots.contains(&ws.join("config.toml")));
        assert!(tracked
            .roots
            .contains(&PathBuf::from("/etc/host/secret.txt")));
        // A restored (inert, verified=false) external is skipped by export —
        // it must not be a watch root either (MAJ-4 / D3).
        assert!(
            !tracked
                .roots
                .contains(&PathBuf::from("/etc/host/inert.txt")),
            "inert externals must not be watched"
        );

        let tracked_controls = by_id("tracked-controls").expect("tracked controls spec");
        assert!(
            tracked_controls.resurface,
            "tracked controls must be surface-defining (manual §4.3)"
        );
        assert!(tracked_controls.tracked);
        assert!(tracked_controls.roots.contains(&ws.join(INCLUDE_FILE)));
        assert!(tracked_controls.roots.contains(&ws.join(SYNC_LOG_FILE)));

        let export_controls = by_id("export-controls").expect("export controls spec");
        assert!(export_controls.resurface);
        assert!(!export_controls.tracked);
        assert!(export_controls.roots.contains(&ws.join(MAP_FILE)));
        // WP-E.3: editing `.alfignore` changes the export surface, so it must
        // dirty the watch loop too.
        assert!(export_controls.roots.contains(&ws.join(".alfignore")));
        assert!(by_id("sentinels").is_none());
    }

    #[test]
    fn watch_paths_without_map_falls_back_to_workspace() {
        let tmp = TempDir::new().unwrap();
        let specs = watch_paths(tmp.path());
        let ws = tmp.path();
        let workspace = specs
            .iter()
            .find(|s| s.id == "workspace" && s.recursive)
            .expect("workspace fallback spec");
        // G3: the fallback recursive spec excludes `.git/`.
        assert!(workspace.exclude.contains(&ws.join(".git")));
        // Control sources are always present so a later configure is noticed.
        assert!(specs.iter().any(|s| s.id == "tracked-controls"));
        assert!(specs.iter().any(|s| s.id == "export-controls"));
    }

    #[test]
    fn root_level_glob_excludes_git() {
        let s = source_spec(Path::new("/ws"), "root", "*.md", false);
        assert!(s.recursive);
        assert_eq!(s.roots, vec![PathBuf::from("/ws")]);
        assert!(s.exclude.contains(&PathBuf::from("/ws/.git")));
    }

    #[test]
    fn unsafe_glob_is_skipped_but_the_map_still_yields_safe_sources() {
        // Review F1: `validate()` accepts `../` globs (it only rejects mid-`**`),
        // so the per-glob guard in watch_paths must skip the escaping one while
        // keeping the safe source — no watch registered outside the workspace.
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        write_map(
            ws,
            r#"{
                "version": 1,
                "memory_sources": [
                    {"id":"escape","glob":"../secrets/*.md","memory_type":"episodic","namespace":"daily","chunking":"per_file"},
                    {"id":"ok","glob":"memories/*.md","memory_type":"episodic","namespace":"daily","chunking":"per_file"}
                ]
            }"#,
        );
        let specs = watch_paths(ws);
        assert!(
            specs.iter().all(|s| s.id != "escape"),
            "unsafe glob skipped"
        );
        assert!(specs.iter().any(|s| s.id == "ok"), "safe glob kept");
        for spec in &specs {
            for root in &spec.roots {
                assert!(
                    root.starts_with(ws),
                    "root {root:?} escapes the workspace {ws:?}"
                );
            }
        }
    }

    #[test]
    fn unsafe_identity_makes_validate_fail_and_falls_back() {
        // An absolute/`..` identity is a hard validate() failure → whole map
        // skipped → workspace fallback (no out-of-workspace identity watch).
        let tmp = TempDir::new().unwrap();
        write_map(
            tmp.path(),
            r#"{"version":1,"identity_file":"../../etc/passwd","memory_sources":[]}"#,
        );
        let specs = watch_paths(tmp.path());
        assert!(specs.iter().all(|s| s.id != "identity"));
        assert!(specs.iter().any(|s| s.id == "workspace"));
    }

    #[test]
    fn invalid_map_falls_back_to_workspace() {
        // A non-canonical memory_type (no escape hatch) makes validate() fail →
        // the whole map is skipped, and the loop still watches the workspace.
        let tmp = TempDir::new().unwrap();
        write_map(
            tmp.path(),
            r#"{"version":1,"memory_sources":[
                {"id":"x","glob":"m/*.md","memory_type":"bogus","namespace":"daily","chunking":"per_file"}]}"#,
        );
        let specs = watch_paths(tmp.path());
        assert!(specs.iter().any(|s| s.id == "workspace"));
        assert!(specs.iter().all(|s| s.id != "x"));
    }
}
