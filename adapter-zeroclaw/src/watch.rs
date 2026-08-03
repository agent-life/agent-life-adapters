//! ZeroClaw `watch_paths` (design §11.1) — the watch surface the MCP loop
//! monitors for a ZeroClaw install.
//!
//! ZeroClaw has no single recursive root: memory lives in a shared SQLite store
//! (`data/memory/brain.db`) *or* markdown under `memory/`, and identity can be
//! an AIEOS file outside the install. So the surface is enumerated per source,
//! mirroring the export (`export.rs`, `brain_db.rs`):
//!
//! - **`brain.db` as a sidecar trio** (`brain.db` + `-wal` + `-shm`) — one spec
//!   with three roots, so a WAL-mode write that touches only the sidecar still
//!   dirties the store (design §11.1; export copy-reads all three). Marked
//!   `sqlite` (a structural hint; inert in v1 — the capture still waits for the
//!   quiesce window).
//! - **`memory/`** (markdown backend) — recursive, `.git/` pruned.
//! - **`ROOT_FILES`** (SOUL/IDENTITY/AGENTS/USER/TOOLS/HEARTBEAT .md).
//! - **`config.toml`** at the install root.
//! - **AIEOS `identity.json`** — path from `config.toml`; may be absolute /
//!   outside the install.
//! - **include-list entries** on `tracked-files`, the include/sync controls on
//!   `tracked-controls`, and `.alfignore` on untracked `export-controls`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use alf_core::include::{IncludeList, INCLUDE_FILE, SYNC_LOG_FILE};
use alf_core::WatchSpec;

use crate::config_parser;
use crate::export::{brain_db_path, resolve_brain_db, zeroclaw_home, ROOT_FILES};

/// Build the ZeroClaw watch surface for `workspace`.
pub fn watch_paths(workspace: &Path) -> Vec<WatchSpec> {
    let install = zeroclaw_home(workspace);
    let mut specs = Vec::new();

    // brain.db + its WAL/SHM sidecars, as one dirty unit. Watch the resolved
    // store if present, else the canonical path (its parent gets watched so a
    // lazily-created store is still noticed).
    let db = resolve_brain_db(&install).unwrap_or_else(|| brain_db_path(&install));
    specs.push(WatchSpec {
        id: "brain.db".into(),
        roots: vec![
            db.clone(),
            with_suffix(&db, "-wal"),
            with_suffix(&db, "-shm"),
        ],
        recursive: false,
        exclude: Vec::new(),
        tracked: false,
        sqlite: true,
        rediscover: false,
        resurface: false,
    });

    // Markdown backend: memory/ is a logical root even when absent. The loop
    // temporarily watches its nearest existing ancestor and upgrades to the
    // recursive root when it appears.
    let memory = install.join("memory");
    specs.push(
        WatchSpec::dir("memory", memory.clone())
            .excluding([memory.join(".git")])
            .resurfacing(),
    );

    // Root-level structural files. Every candidate is a root so the rescan
    // backstop tracks it (and catches a later-created file); notify watches the
    // present ones.
    specs.push(WatchSpec {
        id: "root-files".into(),
        roots: ROOT_FILES.iter().map(|n| install.join(n)).collect(),
        recursive: false,
        exclude: Vec::new(),
        tracked: false,
        sqlite: false,
        rediscover: false,
        resurface: false,
    });

    // config.toml at the install root.
    specs.push(WatchSpec::file("config", install.join("config.toml")).resurfacing());

    // AIEOS identity.json — path declared in config.toml; may be outside the
    // install. Degrade gracefully if the config is missing/unparseable.
    if let Ok(Some(config)) = config_parser::parse_config(&install.join("config.toml")) {
        if let Some(aieos) = config.aieos_path {
            let path = if Path::new(&aieos).is_absolute() {
                PathBuf::from(&aieos)
            } else {
                install.join(&aieos)
            };
            specs.push(WatchSpec::file("identity", path));
        }
    }

    // Include list: in-workspace tracked paths only, as one §6.1 rollover
    // unit. No external roots: the zeroclaw
    // export never packs external entries (`alf add --external` refuses this
    // runtime), so a root here would fire tracked syncs that capture nothing
    // (MAJ-4/MIN-8).
    if let Ok(list) = IncludeList::load(&install) {
        let roots: Vec<PathBuf> = list.paths().iter().map(|p| install.join(p)).collect();
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
    specs.push(WatchSpec {
        id: "tracked-controls".into(),
        roots: vec![install.join(INCLUDE_FILE), install.join(SYNC_LOG_FILE)],
        recursive: false,
        exclude: Vec::new(),
        tracked: true,
        sqlite: false,
        rediscover: false,
        resurface: true, // surface-defining (manual §4.3)
    });
    specs.push(WatchSpec::file(
        "export-controls",
        install.join(".alfignore"),
    ));

    specs
}

/// Append a sidecar suffix (`-wal`/`-shm`) to a `brain.db` path.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
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
    fn with_suffix_appends_to_the_db_filename() {
        assert_eq!(
            with_suffix(Path::new("/a/data/memory/brain.db"), "-wal"),
            PathBuf::from("/a/data/memory/brain.db-wal")
        );
    }

    #[test]
    fn brain_db_spec_is_the_sidecar_trio_grouped() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        // Make it look like an install so zeroclaw_home resolves here.
        fs::write(install.join("config.toml"), "").unwrap();
        let specs = watch_paths(install);

        let db = by_id(&specs, "brain.db").expect("brain.db spec");
        assert!(db.sqlite, "brain.db carries the sqlite structural hint");
        assert!(!db.recursive);
        let canonical = install.join("data").join("memory").join("brain.db");
        assert_eq!(
            db.roots,
            vec![
                canonical.clone(),
                install.join("data").join("memory").join("brain.db-wal"),
                install.join("data").join("memory").join("brain.db-shm"),
            ]
        );
    }

    #[test]
    fn root_files_config_and_control_sources_have_the_right_cadence() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        fs::write(install.join("config.toml"), "").unwrap();
        let specs = watch_paths(install);

        let root = by_id(&specs, "root-files").expect("root-files spec");
        assert!(root.roots.contains(&install.join("SOUL.md")));
        assert!(root.roots.contains(&install.join("HEARTBEAT.md")));

        let config = by_id(&specs, "config").expect("config spec");
        assert_eq!(config.roots, vec![install.join("config.toml")]);

        let tracked = by_id(&specs, "tracked-controls").expect("tracked controls");
        assert!(tracked.tracked);
        assert!(tracked.resurface);
        assert_eq!(
            tracked.roots,
            vec![install.join(INCLUDE_FILE), install.join(SYNC_LOG_FILE)]
        );

        let export = by_id(&specs, "export-controls").expect("export controls");
        assert!(!export.tracked);
        assert!(!export.resurface);
        assert_eq!(export.roots, vec![install.join(".alfignore")]);
    }

    #[test]
    fn aieos_identity_outside_the_install_is_watched() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        fs::write(
            install.join("config.toml"),
            "[identity]\naieos_path = \"/opt/aieos/identity.json\"\n",
        )
        .unwrap();
        let specs = watch_paths(install);
        let identity = by_id(&specs, "identity").expect("identity spec");
        assert_eq!(
            identity.roots,
            vec![PathBuf::from("/opt/aieos/identity.json")]
        );
    }

    #[test]
    fn markdown_memory_is_a_resurfacing_root_when_absent() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        fs::write(install.join("config.toml"), "").unwrap();
        let specs = watch_paths(install);
        let memory = by_id(&specs, "memory").expect("memory spec");
        assert!(memory.recursive);
        assert!(memory.resurface);
        assert_eq!(memory.roots, vec![install.join("memory")]);
        assert_eq!(memory.exclude, vec![install.join("memory").join(".git")]);
    }

    #[test]
    fn tracked_files_in_workspace_only_externals_never_watched() {
        // The zeroclaw export never packs external entries (`alf add
        // --external` refuses this runtime), so only in-workspace tracked
        // paths are watched — an external root would fire tracked syncs that
        // capture nothing (MAJ-4/MIN-8). Even a verified entry stays unwatched.
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        fs::write(install.join("config.toml"), "").unwrap();
        fs::write(
            install.join(INCLUDE_FILE),
            r#"{"files":[
                {"path":"kb.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"AGENTS.md","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/proj/AGENTS.md","verified":true}
            ]}"#,
        )
        .unwrap();
        let specs = watch_paths(install);
        let tracked = by_id(&specs, "tracked-files").expect("tracked spec");
        assert!(tracked.tracked);
        assert_eq!(tracked.roots, vec![install.join("kb.md")]);
    }
}
