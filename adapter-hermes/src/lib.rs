//! # adapter-hermes
//!
//! Hermes (Nous Research) framework adapter for the Agent Life Format (ALF).
//! Translates between a Hermes profile (`HERMES_HOME`, default `~/.hermes`) and
//! the ALF archive format.
//!
//! ## What maps where
//!
//! - `memories/MEMORY.md` (`§`-entries) → semantic memory records (`curated`).
//! - `state.db` sessions → episodic memory records, with the full session in
//!   `raw_source_format`. On import the DB is rebuilt from those records (the
//!   binary is never archived); see [`session_rebuilder`].
//! - `SOUL.md` + `config.yaml` personalities → identity (prose).
//! - `memories/USER.md` → the human principal.
//! - The agent's `~/.alf/vault` → Layer 4 credentials (runtime-agnostic).
//!
//! One profile = one agent = one `.alf`.

use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

pub use alf_core::adapter::{
    AgentBinding, ArchiveEnumeration, ExportReport, FileEntry, ImportOptions, ImportReport,
    WorkspaceEnumeration,
};
pub use alf_core::Adapter;

pub mod config_parser;
pub mod curated_parser;
pub mod export;
pub mod identity_parser;
pub mod import;
pub mod principals_parser;
pub mod session_extractor;
pub mod session_rebuilder;
pub mod skills;
pub mod watch;

// Dry-run enumeration entry points.
pub use export::{enumerate, enumerate_workspace, EnumerationResult};
pub use import::enumerate_archive;

/// Hermes framework adapter.
pub struct HermesAdapter;

impl Adapter for HermesAdapter {
    fn name(&self) -> &str {
        "hermes"
    }

    fn description(&self) -> &str {
        "Hermes framework (Nous Research) — local-first agent profile"
    }

    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
        export::export(workspace, output)
    }

    fn import_with_options(
        &self,
        alf_file: &Path,
        workspace: &Path,
        options: ImportOptions<'_>,
    ) -> Result<ImportReport> {
        import::import(alf_file, workspace, options.vault_key, options.preview)
    }

    fn enumerate_workspace(&self, workspace: &Path) -> Result<WorkspaceEnumeration> {
        export::enumerate_workspace(workspace)
    }

    fn enumerate_archive(&self, alf_file: &Path) -> Result<ArchiveEnumeration> {
        import::enumerate_archive(alf_file)
    }

    fn resolve_agent_id(&self, workspace: &Path) -> Result<Uuid> {
        export::resolve_agent_id_readonly(workspace)
    }

    /// WP-M5: the MCP watch surface — the allowlisted content dirs, `SOUL.md`,
    /// the `state.db` sidecar trio, `config.yaml`, the include-list/sentinels,
    /// and (default profile only) a `profiles/` rediscover boundary.
    fn watch_paths(&self, workspace: &Path) -> Vec<alf_core::WatchSpec> {
        watch::watch_paths(workspace)
    }

    /// WP5: enumerate the Hermes profiles in an install — the default profile
    /// (`~/.hermes` itself) plus each `profiles/<name>/` — one `PerAgentDb`
    /// binding per profile. Overrides the WP0 single-agent fallback. The WP0
    /// default `export_agent`/`import_agent` already retarget `binding.workspace`
    /// and Hermes's export allowlist excludes the shared runtime by construction,
    /// so no override of those is needed — Hermes's profile isolation makes them
    /// correct (the OpenClaw posture).
    fn discover_agents(&self, install: &Path) -> Result<Vec<AgentBinding>> {
        export::discover_agents(install)
    }
}
