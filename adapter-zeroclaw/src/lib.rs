//! # adapter-zeroclaw
//!
//! ZeroClaw framework adapter for the Agent Life Format (ALF). Translates
//! between ZeroClaw's native workspace (SQLite or Markdown backend) and the
//! ALF archive format.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use adapter_zeroclaw::ZeroClawAdapter;
//! use alf_core::Adapter;
//!
//! let adapter = ZeroClawAdapter;
//! let report = adapter.export(workspace_path, output_path)?;
//! ```
//!
//! ## Architecture
//!
//! See `README.md` for the full mapping specification between ZeroClaw memory
//! structures and ALF types.

use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

pub use alf_core::adapter::{
    ArchiveEnumeration, ExportReport, FileEntry, ImportOptions, ImportReport, WorkspaceEnumeration,
};
pub use alf_core::{Adapter, AgentBinding};

pub mod brain_db;
pub mod config_parser;
pub mod export;
pub mod identity_parser;
pub mod import;
pub mod markdown_parser;
pub mod principals_parser;
pub mod sqlite_extractor;
pub mod watch;

// Dry-run enumeration entry points.
pub use export::{enumerate, enumerate_workspace, EnumerationResult};
pub use import::enumerate_archive;

// ---------------------------------------------------------------------------
// Adapter implementation
// ---------------------------------------------------------------------------

/// ZeroClaw framework adapter.
///
/// Implements export (workspace → ALF archive) and import (ALF archive →
/// workspace) for ZeroClaw installations. Supports both SQLite and Markdown
/// memory backends.
pub struct ZeroClawAdapter;

impl Adapter for ZeroClawAdapter {
    fn name(&self) -> &str {
        "zeroclaw"
    }

    fn description(&self) -> &str {
        "ZeroClaw framework — configurable backend agent workspace"
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
        import::import(
            alf_file,
            workspace,
            options.vault_key,
            options.mode,
            options.preview,
        )
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

    /// WP-M5: the MCP watch surface — the `brain.db` sidecar trio, markdown
    /// `memory/`, root files, `config.toml`, the AIEOS identity file, and the
    /// include-list/sentinels.
    fn watch_paths(&self, workspace: &Path) -> Vec<alf_core::WatchSpec> {
        watch::watch_paths(workspace)
    }

    /// WP3: enumerate agents from the shared `brain.db` + `[agents.*]` config
    /// (overrides the WP0 single-agent fallback).
    fn discover_agents(&self, install: &Path) -> Result<Vec<AgentBinding>> {
        export::discover_agents(install)
    }

    /// WP3: export one agent's per-`agent_id` slice of the shared `brain.db`,
    /// stamping the mapping's `alf_agent_id` as the archive identity.
    fn export_agent(
        &self,
        binding: &AgentBinding,
        alf_agent_id: Uuid,
        output: &Path,
    ) -> Result<ExportReport> {
        export::export_agent(binding, alf_agent_id, output)
    }
}
