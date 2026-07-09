//! # adapter-openclaw
//!
//! OpenClaw framework adapter for the Agent Life Format (ALF). Translates
//! between OpenClaw's native file-based workspace and the ALF archive format.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use adapter_openclaw::OpenClawAdapter;
//! use alf_core::Adapter;
//!
//! let adapter = OpenClawAdapter;
//! let report = adapter.export(workspace_path, output_path)?;
//! ```
//!
//! ## Architecture
//!
//! See `README.md` for the full mapping specification between OpenClaw memory
//! structures and ALF types.

use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

// Re-export the shared types so `crate::ExportReport` / `crate::ImportReport`
// continue to resolve in export.rs and import.rs without changes.
pub use alf_core::adapter::{
    ArchiveEnumeration, ExportReport, FileEntry, ImportOptions, ImportReport, WorkspaceEnumeration,
};
pub use alf_core::{Adapter, AgentBinding};

pub mod export;
pub mod identity_parser;
pub mod import;
pub mod memory_parser;
pub mod principals_parser;
pub mod watch;

// Dry-run enumeration entry points.
pub use export::{enumerate, enumerate_workspace, EnumerationResult};
pub use import::enumerate_archive;

// Agent-managed include list (`alf add`) now lives in alf-core (runtime-agnostic
// and shared with the ZeroClaw adapter). Re-exported here so existing
// `adapter_openclaw::IncludeList` / `INCLUDE_FILE` call sites keep resolving.
pub use alf_core::include::{
    normalize_include_path, prune_and_log_missing, IncludeEntry, IncludeList, INCLUDE_FILE,
    SYNC_LOG_FILE,
};

// ---------------------------------------------------------------------------
// Adapter implementation
// ---------------------------------------------------------------------------

/// OpenClaw framework adapter.
///
/// Implements export (workspace → ALF archive) and import (ALF archive →
/// workspace) for real OpenClaw installations.
pub struct OpenClawAdapter;

impl Adapter for OpenClawAdapter {
    fn name(&self) -> &str {
        "openclaw"
    }

    fn description(&self) -> &str {
        "OpenClaw framework — file-based Markdown agent workspace"
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
        import::import(alf_file, workspace, options.vault_key)
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

    /// WP-M5: the MCP watch surface — one recursive workspace watch plus the
    /// out-of-workspace `~/.openclaw/openclaw.json` and external tracked files.
    fn watch_paths(&self, workspace: &Path) -> Vec<alf_core::WatchSpec> {
        watch::watch_paths(workspace)
    }

    /// WP4: enumerate agents from `openclaw.json` `agents.list[]`, one
    /// `InWorkspaceFiles` binding per per-agent workspace dir (`<root>/workspace`
    /// for `main`, the entry's explicit `workspace` for named agents) — overrides
    /// the WP0 single-agent fallback. The WP0 default `export_agent`/`import_agent`
    /// already retarget `binding.workspace`, so no override of those is needed —
    /// OpenClaw's dir-isolation makes them correct.
    fn discover_agents(&self, install: &Path) -> Result<Vec<AgentBinding>> {
        export::discover_agents(install)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alf_core::AGENT_ID_FILE;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_agent_id_reads_workspace_file_first() {
        let tmp = TempDir::new().unwrap();
        let id = uuid::Uuid::parse_str("cfef1150-0000-4000-8000-000000000001").unwrap();
        fs::write(tmp.path().join(AGENT_ID_FILE), id.to_string()).unwrap();
        assert_eq!(OpenClawAdapter.resolve_agent_id(tmp.path()).unwrap(), id);
        // And it never persists a derivation.
        fs::remove_file(tmp.path().join(AGENT_ID_FILE)).unwrap();
        let derived = OpenClawAdapter.resolve_agent_id(tmp.path()).unwrap();
        assert!(!tmp.path().join(AGENT_ID_FILE).exists());
        // Deterministic: the same workspace derives the same id.
        assert_eq!(
            OpenClawAdapter.resolve_agent_id(tmp.path()).unwrap(),
            derived
        );
    }

    /// The WP0 seam against the unmodified adapter export: the default
    /// `export_agent` write-through makes `manifest.agent.id` equal the given
    /// mapping id.
    #[test]
    fn default_export_agent_stamps_given_id() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("SOUL.md"), "# Test Agent\n\nsoul").unwrap();

        let id = uuid::Uuid::parse_str("cfef1150-0000-4000-8000-000000000002").unwrap();
        let binding = &OpenClawAdapter.discover_agents(&ws).unwrap()[0];
        let out = tmp.path().join("out.alf");
        OpenClawAdapter.export_agent(binding, id, &out).unwrap();

        let reader =
            alf_core::AlfReader::new(fs::File::open(&out).unwrap()).expect("readable archive");
        assert_eq!(reader.manifest().agent.id, id);
    }
}
