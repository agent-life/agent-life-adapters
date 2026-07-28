//! # adapter-generic
//!
//! A map-driven adapter for the Agent Life Format (ALF). Unlike the built-in
//! adapters, `adapter-generic` has no hardcoded knowledge of any runtime's
//! layout: a `.alf-map.json` file in the workspace declares which files become
//! memory records, how they are chunked, and how they are tagged/dated.
//!
//! It produces records shaped exactly like the dashboard expects (canonical
//! `memory_type`/`namespace`, `origin_file`, `raw_source_format`) so a generic
//! agent renders pixel-identically to a supported runtime.
//!
//! See [`map`] for the schema/validation and [`export`]/[`import`] for the
//! archive translation.

use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

pub use alf_core::adapter::{
    ArchiveEnumeration, ExportReport, FileEntry, ImportOptions, ImportReport, WorkspaceEnumeration,
};
pub use alf_core::{Adapter, AgentBinding};

pub mod export;
pub mod import;
pub mod map;
pub mod sqlite;
pub mod watch;

pub use map::{MemoryMap, MemorySourceSpec, MAP_FILE};

/// Error-message marker for a hard-failed SQLite extraction (WP-G.1).
///
/// Export prefixes every `sqlite_rows` extraction failure with this string so
/// callers can classify it. **Warning:** `alf-cli`'s watch-loop `classify()`
/// matches on this exact (lowercase) text to treat the failure as transient
/// (bounded backoff, no mass delete) — do not reword one without the other.
pub const SQLITE_EXTRACTION_FAILED: &str = "sqlite extraction failed";

/// Generic map-driven adapter.
pub struct GenericAdapter;

impl Adapter for GenericAdapter {
    fn name(&self) -> &str {
        "generic"
    }

    fn description(&self) -> &str {
        "Generic map-driven runtime — .alf-map.json describes the workspace"
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

    fn resolve_agent_id(&self, workspace: &Path) -> Result<Uuid> {
        export::resolve_agent_id_readonly(workspace)
    }

    fn enumerate_workspace(&self, workspace: &Path) -> Result<WorkspaceEnumeration> {
        export::enumerate_workspace(workspace)
    }

    fn enumerate_archive(&self, alf_file: &Path) -> Result<ArchiveEnumeration> {
        import::enumerate_archive(alf_file)
    }

    fn watch_paths(&self, workspace: &Path) -> Vec<alf_core::WatchSpec> {
        watch::watch_paths(workspace)
    }

    // `discover_agents` (single-agent default), `export_agent`, and
    // `import_agent` use the WP0 trait defaults: generic is directory-isolated
    // and single-agent, so the default write-through / fail-closed behavior is
    // correct without an override.
}

#[cfg(test)]
mod tests {
    use super::*;
    use alf_core::AGENT_ID_FILE;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn name_and_description() {
        assert_eq!(GenericAdapter.name(), "generic");
        assert!(!GenericAdapter.description().is_empty());
    }

    #[test]
    fn resolve_agent_id_reads_workspace_file_first() {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::parse_str("cfef1150-0000-4000-8000-0000000000f1").unwrap();
        fs::write(tmp.path().join(AGENT_ID_FILE), id.to_string()).unwrap();
        assert_eq!(GenericAdapter.resolve_agent_id(tmp.path()).unwrap(), id);
    }

    #[test]
    fn resolve_agent_id_derivation_is_deterministic_and_unpersisted() {
        let tmp = TempDir::new().unwrap();
        let derived = GenericAdapter.resolve_agent_id(tmp.path()).unwrap();
        assert!(!tmp.path().join(AGENT_ID_FILE).exists());
        assert_eq!(
            GenericAdapter.resolve_agent_id(tmp.path()).unwrap(),
            derived
        );
    }

    #[test]
    fn discover_agents_defaults_to_single_agent() {
        let tmp = TempDir::new().unwrap();
        let bindings = GenericAdapter.discover_agents(tmp.path()).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].runtime_agent, "default");
        assert_eq!(bindings[0].workspace, tmp.path());
    }
}
