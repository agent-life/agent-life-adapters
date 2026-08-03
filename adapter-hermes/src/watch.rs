//! Hermes `watch_paths` (design §11.1) — the watch surface the MCP loop
//! monitors for a Hermes profile.
//!
//! `workspace` IS the profile home: `~/.hermes` for the `default` profile,
//! `~/.hermes/profiles/<name>` for a named one. Export is an **allowlist** (D7),
//! so the watch surface mirrors it exactly — nothing under `.env`,
//! `checkpoints/`, `state-snapshots/`, `backups/`, `logs/`, `sessions/`, or a
//! sibling profile's content is ever watched:
//!
//! - **`SOUL.md`** (the single root file).
//! - **`memories/`, `skill-bundles/`, `cron/`, `skills/`** — recursive, when
//!   present. `skills/` is packed by a *separate* export path
//!   (`skills::collect_skill_artifacts` → Tier-2 attachments, not `enumerate_raw`),
//!   so the watch surface is the **union** of `enumerate_raw`'s dirs and the
//!   attachments path — keep this list matched to both (review C1).
//! - **`state.db` sidecar trio** (`state.db` + `-wal` + `-shm`) as one unit;
//!   sessions come from SQL, not the `sessions/` dir. The store is created lazily
//!   on first session, so watching its parent (the profile home) catches its
//!   creation. Marked `sqlite` (structural hint; inert in v1).
//! - **`config.yaml`**.
//! - **`profiles/`** (default profile only) — a **`rediscover`** spec: a new
//!   `profiles/<name>/` created mid-session re-runs discovery so the new agent
//!   surfaces in `alf_agents_list` (registration stays lazy; design §14).
//! - **include-list entries** (in-workspace + *verified* external — inert
//!   restored externals are not packed, so not watched) on `tracked-files`, the
//!   include/sync controls on `tracked-controls`, and `.alfignore` on untracked
//!   `export-controls`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use alf_core::include::{IncludeList, INCLUDE_FILE, SYNC_LOG_FILE};
use alf_core::WatchSpec;

/// Root file preserved at the profile top level (mirrors `export::ROOT_FILES`).
const ROOT_FILES: &[&str] = &["SOUL.md"];
/// Recursive content dirs export packs: `export::RAW_DIRS` (`memories`,
/// `skill-bundles`, `cron`) **plus** `skills/`, which export packs via the
/// separate attachments path (`skills::collect_skill_artifacts`, review C1).
/// Watched only when present at serve start — a dir first created mid-session is
/// not watched until the server restarts (the generic-G2 limitation; §D2).
const CONTENT_DIRS: &[&str] = &["memories", "skill-bundles", "cron", "skills"];

/// Build the Hermes watch surface for the profile rooted at `workspace`.
pub fn watch_paths(workspace: &Path) -> Vec<WatchSpec> {
    let mut specs = Vec::new();

    // Root files.
    specs.push(WatchSpec {
        id: "root-files".into(),
        roots: ROOT_FILES.iter().map(|n| workspace.join(n)).collect(),
        recursive: false,
        exclude: Vec::new(),
        tracked: false,
        sqlite: false,
        rediscover: false,
        resurface: false,
    });

    // Allowlisted content dirs are logical roots even when absent. The watch
    // loop temporarily watches their nearest existing ancestor, then upgrades
    // to this recursive root when it appears.
    for dir in CONTENT_DIRS {
        let root = workspace.join(dir);
        specs.push(
            WatchSpec::dir(*dir, root.clone())
                .excluding([root.join(".git")])
                .resurfacing(),
        );
    }

    // state.db + WAL/SHM sidecars, one dirty unit.
    let db = workspace.join("state.db");
    specs.push(WatchSpec {
        id: "state.db".into(),
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

    // config.yaml.
    specs.push(WatchSpec::file("config", workspace.join("config.yaml")).resurfacing());

    // profiles/ — only for the default profile (the home itself). A named
    // profile's workspace is `<home>/profiles/<name>`, which has no `profiles/`
    // child. A new entry here means a new agent → re-run discovery.
    if !is_named_profile(workspace) {
        specs.push(WatchSpec::dir("profiles", workspace.join("profiles")).rediscovering());
    }

    // Include list: in-workspace tracked paths + VERIFIED external sources,
    // one §6.1 rollover unit. Inert (restored,
    // unverified) externals are skipped by the export, so they must not be
    // watch roots either (MAJ-4 / D3).
    if let Ok(list) = IncludeList::load(workspace) {
        let mut roots: Vec<PathBuf> = list.paths().iter().map(|p| workspace.join(p)).collect();
        roots.extend(
            list.verified_externals()
                .filter_map(|e| e.source.as_ref().map(PathBuf::from)),
        );
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
        roots: vec![workspace.join(INCLUDE_FILE), workspace.join(SYNC_LOG_FILE)],
        recursive: false,
        exclude: Vec::new(),
        tracked: true,
        sqlite: false,
        rediscover: false,
        resurface: true, // surface-defining (manual §4.3)
    });
    specs.push(WatchSpec::file(
        "export-controls",
        workspace.join(".alfignore"),
    ));

    specs
}

/// A named profile's home is `.../profiles/<name>`. The `default` profile's home
/// is the Hermes install root, which never has `profiles` as its parent.
fn is_named_profile(workspace: &Path) -> bool {
    workspace.parent().and_then(Path::file_name) == Some(OsStr::new("profiles"))
}

/// Append a sidecar suffix (`-wal`/`-shm`) to a `state.db` path.
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
    fn inert_externals_are_not_watched() {
        // D3: a restored archive's externals come back verified=false and the
        // export skips them — watching them would fire tracked syncs that
        // capture nothing (and let a hostile archive pick watch roots).
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        fs::write(
            ws.join(alf_core::include::INCLUDE_FILE),
            r#"{"files":[
                {"path":"kb.md","added_at":"2026-01-01T00:00:00Z","external":false,"verified":true},
                {"path":"AGENTS.md","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/proj/AGENTS.md","verified":true},
                {"path":"cursorrules","added_at":"2026-01-01T00:00:00Z","external":true,"source":"/etc/proj/.cursorrules","verified":false}
            ]}"#,
        )
        .unwrap();
        let specs = watch_paths(ws);
        let tracked = by_id(&specs, "tracked-files").expect("tracked spec");
        assert!(tracked.roots.contains(&ws.join("kb.md")));
        assert!(
            tracked
                .roots
                .contains(&PathBuf::from("/etc/proj/AGENTS.md")),
            "verified externals stay watched"
        );
        assert!(
            !tracked
                .roots
                .contains(&PathBuf::from("/etc/proj/.cursorrules")),
            "inert (unverified) externals must not be watched"
        );
    }

    #[test]
    fn state_db_is_the_sidecar_trio() {
        let tmp = TempDir::new().unwrap();
        let specs = watch_paths(tmp.path());
        let db = by_id(&specs, "state.db").expect("state.db spec");
        assert!(db.sqlite);
        assert_eq!(
            db.roots,
            vec![
                tmp.path().join("state.db"),
                tmp.path().join("state.db-wal"),
                tmp.path().join("state.db-shm"),
            ]
        );
    }

    #[test]
    fn default_profile_watches_profiles_dir_for_rediscovery() {
        let tmp = TempDir::new().unwrap();
        // A home whose parent is not `profiles/` → default profile.
        let specs = watch_paths(tmp.path());
        let profiles = by_id(&specs, "profiles").expect("profiles spec");
        assert!(profiles.rediscover, "profiles is an agent-set boundary");
        assert_eq!(profiles.roots, vec![tmp.path().join("profiles")]);
    }

    #[test]
    fn named_profile_has_no_profiles_spec() {
        let tmp = TempDir::new().unwrap();
        let named = tmp.path().join("profiles").join("scout");
        fs::create_dir_all(&named).unwrap();
        let specs = watch_paths(&named);
        assert!(
            by_id(&specs, "profiles").is_none(),
            "a named profile does not watch a nested profiles/"
        );
    }

    #[test]
    fn allowlisted_dirs_are_resurfacing_roots_when_absent() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let specs = watch_paths(home);
        for dir in CONTENT_DIRS {
            let spec = by_id(&specs, dir).expect("allowlisted content spec");
            assert!(spec.recursive, "{dir} must be recursive after it appears");
            assert!(
                spec.resurface,
                "{dir} must refresh the watch surface on creation"
            );
            assert_eq!(spec.roots, vec![home.join(dir)]);
            assert_eq!(spec.exclude, vec![home.join(dir).join(".git")]);
        }
    }

    #[test]
    fn skills_dir_is_watched_when_present() {
        // Review C1: `skills/` is a real export surface (Tier-2 attachments) that
        // must be watched so a mid-serve skill edit auto-syncs.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        fs::create_dir_all(home.join("skills").join("custom").join("deploy")).unwrap();
        let specs = watch_paths(home);
        let skills = by_id(&specs, "skills").expect("skills spec");
        assert!(skills.recursive);
        assert_eq!(skills.roots, vec![home.join("skills")]);
    }

    #[test]
    fn config_root_and_control_sources_have_the_right_cadence() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let specs = watch_paths(home);
        assert_eq!(
            by_id(&specs, "config").unwrap().roots,
            vec![home.join("config.yaml")]
        );
        assert!(by_id(&specs, "root-files")
            .unwrap()
            .roots
            .contains(&home.join("SOUL.md")));
        let tracked = by_id(&specs, "tracked-controls").unwrap();
        assert!(tracked.tracked);
        assert!(tracked.resurface);
        assert_eq!(
            tracked.roots,
            vec![home.join(INCLUDE_FILE), home.join(SYNC_LOG_FILE)]
        );
        let export = by_id(&specs, "export-controls").unwrap();
        assert!(!export.tracked);
        assert!(!export.resurface);
        assert_eq!(export.roots, vec![home.join(".alfignore")]);
    }

    #[test]
    fn never_watches_forbidden_dirs() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        for d in [
            "logs",
            "sessions",
            "checkpoints",
            "backups",
            "state-snapshots",
        ] {
            fs::create_dir_all(home.join(d)).unwrap();
        }
        fs::write(home.join(".env"), "SECRET=x").unwrap();
        let specs = watch_paths(home);
        for spec in &specs {
            for root in &spec.roots {
                for forbidden in [
                    "logs",
                    "sessions",
                    "checkpoints",
                    "backups",
                    "state-snapshots",
                    ".env",
                ] {
                    assert!(
                        root != &home.join(forbidden),
                        "{forbidden} must never be a watch root"
                    );
                }
            }
        }
    }
}
