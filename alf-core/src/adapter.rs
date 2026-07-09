//! Adapter trait and report types shared across all framework adapters.
//!
//! Each agent framework (OpenClaw, ZeroClaw, etc.) implements the [`Adapter`]
//! trait. The CLI dispatches to the correct adapter based on the `--runtime`
//! flag.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::crypto::VaultKey;

// ---------------------------------------------------------------------------
// Export / Import reports
// ---------------------------------------------------------------------------

/// Summary of an export operation.
#[derive(Debug)]
pub struct ExportReport {
    pub agent_name: String,
    pub alf_version: String,
    pub memory_records: u64,
    pub identity_version: Option<u32>,
    pub principals_count: u32,
    pub credentials_count: u32,
    pub attachments_count: u32,
    pub raw_sources: Vec<String>,
    pub output_path: String,
    pub output_size_bytes: u64,
    /// Number of workspace files dropped by a `.alfignore` filter (0 when no
    /// `.alfignore` is present).
    pub excluded_by_alfignore: u32,
    /// Paths in the agent's include list (`alf add`) that no longer exist on
    /// disk at export time. `alf sync` prunes these and logs the removal.
    pub missing_includes: Vec<String>,
    /// Non-fatal advisories surfaced to the user on export/sync — e.g. the
    /// Hermes adapter's "`~/.hermes/.env` has N keys not backed up; vault them
    /// with `alf vault add`" notice (D4). Empty for adapters that emit none.
    pub warnings: Vec<String>,
}

/// Summary of an import operation.
#[derive(Debug)]
pub struct ImportReport {
    pub agent_name: String,
    pub memory_records: u64,
    pub identity_imported: bool,
    pub principals_count: u32,
    pub credentials_count: u32,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Import options
// ---------------------------------------------------------------------------

/// How an adapter that restores into a live per-agent store should reconcile
/// the archive's records with rows already present for that agent.
///
/// Only adapters with a mutable per-agent store (WP3 ZeroClaw `brain.db`) act
/// on this; file/markdown adapters ignore it. `Total` is the default so a bare
/// restore reproduces the backup exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// Transactional delete-slice-then-insert: the agent's end state equals the
    /// archive. Rows written locally after the backup are dropped.
    #[default]
    Total,
    /// Per-agent upsert (`ON CONFLICT(agent_id, key) DO UPDATE`): archive rows
    /// win on conflict, but local-only rows written after the backup survive.
    Merge,
}

/// Optional inputs for an import run.
///
/// `vault_key`, when supplied, tells the adapter to decrypt
/// `CredentialRecord.encrypted_payload` entries and inject the resulting
/// plaintext into the target runtime's native credential storage. When
/// absent, adapters preserve the legacy behavior of reporting credentials
/// without writing them.
///
/// `mode` governs how a live per-agent store is reconciled with the archive
/// (WP3 ZeroClaw restore); adapters without a mutable store ignore it.
#[derive(Default)]
pub struct ImportOptions<'a> {
    pub vault_key: Option<&'a VaultKey>,
    pub mode: RestoreMode,
}

// ---------------------------------------------------------------------------
// Dry-run enumeration
// ---------------------------------------------------------------------------

/// A single file in an export or restore preview.
///
/// `path` is the path as it appears in the archive's `raw/{runtime}/` tree —
/// workspace-relative for files that live under the workspace, or a synthesized
/// name (e.g. ZeroClaw's redacted `config.toml`) for entries that do not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

/// Result of enumerating a workspace for an `export --dry-run` preview.
#[derive(Debug)]
pub struct WorkspaceEnumeration {
    pub agent_name: String,
    pub memory_records: u64,
    pub files: Vec<FileEntry>,
    pub excluded_by_alfignore: u32,
    pub total_size: u64,
    pub warnings: Vec<String>,
}

/// Result of enumerating an archive for a `restore --dry-run` preview.
#[derive(Debug)]
pub struct ArchiveEnumeration {
    pub files: Vec<FileEntry>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Watch surface (WP-M3)
// ---------------------------------------------------------------------------

/// One logical "dirty unit" the MCP watch loop should monitor (design §11.1).
///
/// A change to **any** path in [`roots`](WatchSpec::roots) marks the whole spec
/// dirty. This is how a SQLite `db` + `-wal` + `-shm` sidecar trio is expressed
/// as a single unit: the three sidecars share one spec, so a WAL-only write
/// (which touches only `-wal`) still dirties the store.
///
/// Roots may be **absolute** (out-of-workspace tracked files, an external
/// identity file such as ZeroClaw's AIEOS `identity.json`, or
/// `~/.openclaw/openclaw.json`) or inside the workspace. A directory root with
/// `recursive: true` is watched recursively with the `exclude` prefixes pruned;
/// a file root is watched as itself.
///
/// The trait method returning these — [`Adapter::watch_paths`] — is additive
/// with a default of "the whole workspace, recursively", so no existing adapter
/// (or the service, which never calls it) is affected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSpec {
    /// Stable identifier — the map source id, `"tracked-files"`, `"sentinels"`,
    /// `"brain.db"`, … Used as the scheduler's debounce/interval key and the
    /// `alf_status` source label.
    pub id: String,
    /// The concrete files or directory roots whose change dirties this spec.
    pub roots: Vec<PathBuf>,
    /// When true, `roots` are directory roots watched recursively.
    pub recursive: bool,
    /// Path prefixes under a recursive root to ignore (e.g. `.git/`).
    pub exclude: Vec<PathBuf>,
    /// This spec is the §6.1 **tracked-file channel** — a change here triggers a
    /// full-snapshot rollover, so the scheduler governs it with
    /// `tracked_files_interval` (floor 15 min) rather than the 1-minute delta
    /// floor.
    pub tracked: bool,
    /// Informational hint that this spec's roots are (or may become) a SQLite
    /// store. **Inert in v1** (WP-M3 review A2): it does NOT bypass the quiesce
    /// gate — a live raw `.db` is captured by a plain single-file read, so it must
    /// wait for quiescence like any file rather than ship torn bytes. Reserved for
    /// the v2 consistent-capture / DB-row-extraction path (design §10).
    pub sqlite: bool,
    /// A change under these roots means the *agent set* may have changed, not just
    /// one agent's memory — so the watch loop must re-run discovery before syncing
    /// (WP-M5). The Hermes adapter sets this on the `profiles/` directory so a new
    /// `profiles/<name>/` created mid-session persists a new `[[agents]]` row and
    /// surfaces in `alf_agents_list` (design §14, §11.1). Default `false`: an
    /// ordinary source change only dirties that source.
    pub rediscover: bool,
}

impl WatchSpec {
    /// A single non-recursive file spec (the common case: one tracked file,
    /// a sentinel, an external identity file).
    pub fn file(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            roots: vec![path.into()],
            recursive: false,
            exclude: Vec::new(),
            tracked: false,
            sqlite: false,
            rediscover: false,
        }
    }

    /// A recursive directory spec.
    pub fn dir(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            roots: vec![root.into()],
            recursive: true,
            exclude: Vec::new(),
            tracked: false,
            sqlite: false,
            rediscover: false,
        }
    }

    /// Mark this spec as the tracked-file channel (§6.1 rollover cadence).
    pub fn as_tracked(mut self) -> Self {
        self.tracked = true;
        self
    }

    /// Mark this spec as a SQLite store (structural hint; inert in v1).
    pub fn as_sqlite(mut self) -> Self {
        self.sqlite = true;
        self
    }

    /// Mark this spec as an agent-set boundary: a change here re-runs discovery
    /// before the next sync (WP-M5 — Hermes `profiles/`).
    pub fn rediscovering(mut self) -> Self {
        self.rediscover = true;
        self
    }

    /// Add exclusion prefixes to a recursive spec.
    pub fn excluding(mut self, prefixes: impl IntoIterator<Item = PathBuf>) -> Self {
        self.exclude.extend(prefixes);
        self
    }
}

// ---------------------------------------------------------------------------
// Multi-agent discovery (WP0)
// ---------------------------------------------------------------------------

/// Name of the per-workspace agent-identity file (`{workspace}/.alf-agent-id`).
pub const AGENT_ID_FILE: &str = ".alf-agent-id";

/// Where one agent's memory physically lives. Adapter-owned topology;
/// re-derived at every discovery — never persisted to config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemorySource {
    /// Files inside the binding's workspace (OpenClaw MEMORY.md, memory/*.md).
    InWorkspaceFiles,
    /// A DB owned exclusively by this agent (Hermes profile state.db). The
    /// path is a descriptor, not an existence guarantee (stores are lazy).
    PerAgentDb { path: PathBuf },
    /// A store shared by all agents, partitioned by a filter column
    /// (ZeroClaw data/memory/brain.db + agent_id).
    SharedDb { path: PathBuf, filter_key: String },
}

/// One runtime agent as discovered in an install. In-memory only — the
/// persisted form is the `[[agents]]` row in ~/.alf/config.toml (alf-cli).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBinding {
    /// Runtime alias (openclaw agents.list[].id / zeroclaw [agents.<alias>] / hermes profile).
    pub runtime_agent: String,
    /// Runtime-native agent id where one exists (ZeroClaw agents.id).
    pub runtime_agent_id: Option<String>,
    /// The agent's file workspace (present-but-empty in practice for ZeroClaw).
    pub workspace: PathBuf,
    pub memory_source: MemorySource,
    /// First-run enablement classification (design §10 / Z3 nuance).
    /// Applied only when the mapping for this runtime is empty; never re-applied.
    pub default_enabled: bool,
}

/// Write-through of the mapping's `alf_agent_id` into `{workspace}/.alf-agent-id`.
///
/// Absent → write it. Present+equal → Ok. Present+different → error with cause
/// + remedy (drift; the mapping is never silently rebound). Unparseable → error.
pub fn ensure_workspace_agent_id(workspace: &Path, alf_agent_id: Uuid) -> Result<()> {
    let id_file = workspace.join(AGENT_ID_FILE);
    if !id_file.is_file() {
        fs::write(&id_file, alf_agent_id.to_string())
            .with_context(|| format!("Failed to write {}", id_file.display()))?;
        return Ok(());
    }
    let raw = fs::read_to_string(&id_file)
        .with_context(|| format!("Failed to read {}", id_file.display()))?;
    let existing = Uuid::parse_str(raw.trim())
        .with_context(|| format!("Invalid UUID in {}", id_file.display()))?;
    if existing != alf_agent_id {
        bail!(
            "Agent identity drift: {} contains {} but the mapping expects {}. \
             To keep the mapped history run: echo {} > {}. \
             Run `alf check` after intentional identity changes.",
            id_file.display(),
            existing,
            alf_agent_id,
            alf_agent_id,
            id_file.display()
        );
    }
    Ok(())
}

/// Fail-closed guard for per-agent import: reads the archive manifest and
/// errors unless `manifest.agent.id == alf_agent_id`.
pub fn verify_archive_agent(alf_file: &Path, alf_agent_id: Uuid) -> Result<()> {
    let file = fs::File::open(alf_file)
        .with_context(|| format!("Failed to open {}", alf_file.display()))?;
    let reader = crate::archive::AlfReader::new(file)
        .with_context(|| format!("Failed to read {}", alf_file.display()))?;
    let archive_id = reader.manifest().agent.id;
    if archive_id != alf_agent_id {
        bail!(
            "Archive {} belongs to agent {} but agent {} was selected. \
             Refusing to import another agent's archive; pass --agent {} to \
             import it as its own agent.",
            alf_file.display(),
            archive_id,
            alf_agent_id,
            archive_id
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Adapter trait
// ---------------------------------------------------------------------------

/// Trait that each runtime adapter must implement.
///
/// An adapter knows how to read a framework's native workspace format and
/// translate it to/from an ALF archive.
pub trait Adapter {
    /// Runtime identifier (e.g., `"openclaw"`, `"zeroclaw"`).
    fn name(&self) -> &str;

    /// Human-readable description of the adapter.
    fn description(&self) -> &str;

    /// Export a workspace to an .alf file.
    ///
    /// `workspace` is the path to the framework's workspace directory;
    /// `output` is the path to write the .alf file. Layer 4 (credentials)
    /// comes from the agent's explicit ALF vault (`~/.alf/vault/`) — export
    /// never reads a vault key.
    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport>;

    /// Import an .alf file into a workspace with no options.
    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        self.import_with_options(alf_file, workspace, ImportOptions::default())
    }

    /// Import an .alf file into a workspace with caller-supplied options.
    fn import_with_options(
        &self,
        alf_file: &Path,
        workspace: &Path,
        options: ImportOptions<'_>,
    ) -> Result<ImportReport>;

    /// Enumerate the files an `export` would archive, without writing anything.
    ///
    /// Backs `alf export --dry-run`. Adapters that support dry-run override
    /// this; the default rejects the call so the CLI surfaces a clear error.
    fn enumerate_workspace(&self, _workspace: &Path) -> Result<WorkspaceEnumeration> {
        bail!("dry-run not supported for this runtime")
    }

    /// Enumerate the files an `import` would write from an archive, without
    /// touching the filesystem.
    ///
    /// Backs `alf restore --dry-run`. Adapters that support dry-run override
    /// this; the default rejects the call so the CLI surfaces a clear error.
    fn enumerate_archive(&self, _alf_file: &Path) -> Result<ArchiveEnumeration> {
        bail!("dry-run not supported for this runtime")
    }

    /// The filesystem surface the MCP watch loop should monitor (design §11.1).
    ///
    /// Default: watch the whole workspace recursively as one spec. Adapters that
    /// track files **outside** the workspace (generic's external include-list
    /// roots, ZeroClaw's AIEOS `identity.json`, OpenClaw's `~/.openclaw/…`) MUST
    /// override this — the whole-workspace default silently misses them. Purely
    /// additive: the default keeps every current caller (and the service, which
    /// never calls it) unaffected.
    fn watch_paths(&self, workspace: &Path) -> Vec<WatchSpec> {
        vec![WatchSpec::dir("workspace", workspace.to_path_buf())]
    }

    /// Enumerate the agents in an install. `install` is the CLI-resolved
    /// workspace/install root.
    ///
    /// Default: the single-agent fallback (M=1) — one binding treating
    /// `install` as the sole agent's workspace. Adapters with real multi-agent
    /// topologies (WP3–5) override this to read the runtime's own agent
    /// registry; overrides must report every agent in the install, carry the
    /// runtime-native id where one exists, and set `default_enabled` per the
    /// runtime's first-run classification (design §10). Discovery must never
    /// write to the install.
    fn discover_agents(&self, install: &Path) -> Result<Vec<AgentBinding>> {
        let alias = if self.name() == "openclaw" {
            "main"
        } else {
            "default"
        };
        Ok(vec![AgentBinding {
            runtime_agent: alias.into(),
            runtime_agent_id: None,
            workspace: install.to_path_buf(),
            memory_source: MemorySource::InWorkspaceFiles,
            default_enabled: true,
        }])
    }

    /// Read-only agent-id resolution for `workspace`: `{workspace}/.alf-agent-id`
    /// if present, else the adapter's deterministic derivation. Never persists.
    ///
    /// REQUIRED — each adapter's derivation namespace differs, so the CLI must
    /// mint new ids through the adapter to stay convergent with a bare export.
    fn resolve_agent_id(&self, workspace: &Path) -> Result<Uuid>;

    /// Export one agent, stamping `alf_agent_id` as the archive identity.
    ///
    /// Postcondition (the WP0 seam): `manifest.agent.id == alf_agent_id`. The
    /// default writes the id through to `{workspace}/.alf-agent-id` (which
    /// every adapter's export reads first) and delegates to [`export`].
    /// WP3 (ZeroClaw) overrides for per-slice extraction.
    ///
    /// [`export`]: Adapter::export
    fn export_agent(
        &self,
        binding: &AgentBinding,
        alf_agent_id: Uuid,
        output: &Path,
    ) -> Result<ExportReport> {
        ensure_workspace_agent_id(&binding.workspace, alf_agent_id)?;
        self.export(&binding.workspace, output)
    }

    /// Import/restore one agent. Fails closed on a wrong-agent archive.
    fn import_agent(
        &self,
        binding: &AgentBinding,
        alf_agent_id: Uuid,
        alf_file: &Path,
        options: ImportOptions<'_>,
    ) -> Result<ImportReport> {
        verify_archive_agent(alf_file, alf_agent_id)?;
        self.import_with_options(alf_file, &binding.workspace, options)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::AlfWriter;
    use crate::manifest::{AgentMetadata, LayerInventory, Manifest};
    use std::collections::HashMap;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    /// Write a minimal .alf whose manifest carries `agent_id`.
    fn write_archive(path: &Path, agent_id: Uuid) {
        let manifest = Manifest {
            alf_version: "1.0.0".into(),
            created_at: chrono::Utc::now(),
            agent: AgentMetadata {
                id: agent_id,
                name: "Test Agent".into(),
                source_runtime: "test".into(),
                source_runtime_version: None,
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                attachments: None,
                extra: HashMap::new(),
            },
            runtime_hints: None,
            sync: None,
            raw_sources: vec![],
            checksum: None,
            extra: HashMap::new(),
        };
        let writer = AlfWriter::new(Cursor::new(Vec::new()), manifest).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        fs::write(path, bytes).unwrap();
    }

    /// Minimal adapter that mimics the real ones: export reads
    /// `{workspace}/.alf-agent-id` first, exactly like every adapter crate.
    struct DummyAdapter {
        name: &'static str,
    }

    impl Adapter for DummyAdapter {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "dummy adapter for WP0 contract tests"
        }

        fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
            let raw = fs::read_to_string(workspace.join(AGENT_ID_FILE))?;
            let id = Uuid::parse_str(raw.trim())?;
            write_archive(output, id);
            Ok(ExportReport {
                agent_name: "Test Agent".into(),
                alf_version: "1.0.0".into(),
                memory_records: 0,
                identity_version: None,
                principals_count: 0,
                credentials_count: 0,
                attachments_count: 0,
                raw_sources: vec![],
                output_path: output.to_string_lossy().into(),
                output_size_bytes: 0,
                excluded_by_alfignore: 0,
                missing_includes: vec![],
                warnings: vec![],
            })
        }

        fn import_with_options(
            &self,
            _alf_file: &Path,
            _workspace: &Path,
            _options: ImportOptions<'_>,
        ) -> Result<ImportReport> {
            Ok(ImportReport {
                agent_name: "Test Agent".into(),
                memory_records: 0,
                identity_imported: false,
                principals_count: 0,
                credentials_count: 0,
                warnings: vec![],
            })
        }

        fn resolve_agent_id(&self, workspace: &Path) -> Result<Uuid> {
            let id_file = workspace.join(AGENT_ID_FILE);
            if id_file.is_file() {
                let raw = fs::read_to_string(&id_file)?;
                return Ok(Uuid::parse_str(raw.trim())?);
            }
            Ok(uuid(0xEE))
        }
    }

    #[test]
    fn ensure_workspace_agent_id_writes_when_absent() {
        let tmp = TempDir::new().unwrap();
        ensure_workspace_agent_id(tmp.path(), uuid(1)).unwrap();
        let raw = fs::read_to_string(tmp.path().join(AGENT_ID_FILE)).unwrap();
        assert_eq!(raw.trim(), uuid(1).to_string());
    }

    #[test]
    fn ensure_workspace_agent_id_ok_when_equal() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(AGENT_ID_FILE), uuid(2).to_string()).unwrap();
        ensure_workspace_agent_id(tmp.path(), uuid(2)).unwrap();
    }

    #[test]
    fn ensure_workspace_agent_id_fails_closed_on_drift() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(AGENT_ID_FILE), uuid(3).to_string()).unwrap();
        let err = ensure_workspace_agent_id(tmp.path(), uuid(4)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&uuid(3).to_string()),
            "must name the file id: {msg}"
        );
        assert!(
            msg.contains(&uuid(4).to_string()),
            "must name the mapping id: {msg}"
        );
        assert!(msg.contains("echo"), "must give the heal command: {msg}");
        // Fail closed: the file must not have been rewritten.
        let raw = fs::read_to_string(tmp.path().join(AGENT_ID_FILE)).unwrap();
        assert_eq!(raw.trim(), uuid(3).to_string());
    }

    #[test]
    fn ensure_workspace_agent_id_rejects_unparseable_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(AGENT_ID_FILE), "not-a-uuid").unwrap();
        let err = ensure_workspace_agent_id(tmp.path(), uuid(5)).unwrap_err();
        assert!(format!("{err:#}").contains("Invalid UUID"));
    }

    #[test]
    fn verify_archive_agent_passes_on_matching_id() {
        let tmp = TempDir::new().unwrap();
        let alf = tmp.path().join("a.alf");
        write_archive(&alf, uuid(6));
        verify_archive_agent(&alf, uuid(6)).unwrap();
    }

    #[test]
    fn verify_archive_agent_fails_closed_on_mismatch() {
        let tmp = TempDir::new().unwrap();
        let alf = tmp.path().join("a.alf");
        write_archive(&alf, uuid(7));
        let err = verify_archive_agent(&alf, uuid(8)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&uuid(7).to_string()),
            "must name the archive agent: {msg}"
        );
        assert!(
            msg.contains(&uuid(8).to_string()),
            "must name the selected agent: {msg}"
        );
    }

    #[test]
    fn default_discover_agents_openclaw_alias_main() {
        let tmp = TempDir::new().unwrap();
        let adapter = DummyAdapter { name: "openclaw" };
        let bindings = adapter.discover_agents(tmp.path()).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].runtime_agent, "main");
        assert_eq!(bindings[0].workspace, tmp.path());
        assert_eq!(bindings[0].memory_source, MemorySource::InWorkspaceFiles);
        assert!(bindings[0].default_enabled);
    }

    #[test]
    fn default_discover_agents_other_runtimes_alias_default() {
        let tmp = TempDir::new().unwrap();
        for name in ["zeroclaw", "hermes"] {
            let adapter = DummyAdapter { name };
            let bindings = adapter.discover_agents(tmp.path()).unwrap();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].runtime_agent, "default");
        }
    }

    #[test]
    fn default_export_agent_stamps_given_id() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let adapter = DummyAdapter { name: "openclaw" };
        let binding = &adapter.discover_agents(&ws).unwrap()[0];

        let out = tmp.path().join("out.alf");
        adapter.export_agent(binding, uuid(9), &out).unwrap();

        // The write-through stamped the id before export read it.
        let reader = crate::archive::AlfReader::new(fs::File::open(&out).unwrap()).unwrap();
        assert_eq!(reader.manifest().agent.id, uuid(9));
        let raw = fs::read_to_string(ws.join(AGENT_ID_FILE)).unwrap();
        assert_eq!(raw.trim(), uuid(9).to_string());
    }

    #[test]
    fn default_export_agent_fails_closed_on_drift() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join(AGENT_ID_FILE), uuid(10).to_string()).unwrap();
        let adapter = DummyAdapter { name: "openclaw" };
        let binding = &adapter.discover_agents(&ws).unwrap()[0];

        let out = tmp.path().join("out.alf");
        let err = adapter.export_agent(binding, uuid(11), &out).unwrap_err();
        assert!(format!("{err:#}").contains("drift"));
        assert!(
            !out.exists(),
            "drift must abort before export writes anything"
        );
    }

    #[test]
    fn default_import_agent_fails_closed_on_wrong_agent_archive() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let alf = tmp.path().join("a.alf");
        write_archive(&alf, uuid(12));

        let adapter = DummyAdapter { name: "openclaw" };
        let binding = &adapter.discover_agents(&ws).unwrap()[0];
        let err = adapter
            .import_agent(binding, uuid(13), &alf, ImportOptions::default())
            .unwrap_err();
        assert!(format!("{err:#}").contains("Refusing to import"));

        adapter
            .import_agent(binding, uuid(12), &alf, ImportOptions::default())
            .expect("matching archive must import");
    }

    #[test]
    fn default_watch_paths_is_the_whole_workspace() {
        let adapter = DummyAdapter { name: "openclaw" };
        let ws = Path::new("/tmp/ws");
        let specs = adapter.watch_paths(ws);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].roots, vec![ws.to_path_buf()]);
        assert!(specs[0].recursive);
        assert!(!specs[0].tracked);
        assert!(!specs[0].sqlite);
        assert!(!specs[0].rediscover);
    }

    #[test]
    fn watch_spec_builders() {
        let f = WatchSpec::file("sentinels", "/ws/.alf-map.json");
        assert!(!f.recursive && !f.tracked && !f.sqlite && !f.rediscover);
        assert_eq!(f.roots, vec![PathBuf::from("/ws/.alf-map.json")]);

        let d =
            WatchSpec::dir("memory", "/ws/memory").excluding([PathBuf::from("/ws/memory/.git")]);
        assert!(d.recursive);
        assert_eq!(d.exclude, vec![PathBuf::from("/ws/memory/.git")]);

        let t = WatchSpec::file("tracked-files", "/ws/config.toml").as_tracked();
        assert!(t.tracked);

        let s = WatchSpec::file("brain.db", "/ws/data/brain.db").as_sqlite();
        assert!(s.sqlite);

        let r = WatchSpec::dir("profiles", "/ws/profiles").rediscovering();
        assert!(r.rediscover);
    }
}
