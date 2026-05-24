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

// Re-export the shared types so `crate::ExportReport` / `crate::ImportReport`
// continue to resolve in export.rs and import.rs without changes.
pub use alf_core::adapter::{
    ArchiveEnumeration, ExportReport, FileEntry, ImportOptions, ImportReport, WorkspaceEnumeration,
};
pub use alf_core::Adapter;

pub mod export;
pub mod identity_parser;
pub mod import;
pub mod include;
pub mod memory_parser;
pub mod principals_parser;

// Dry-run enumeration entry points.
pub use export::{enumerate, enumerate_workspace, EnumerationResult};
pub use import::enumerate_archive;

// Agent-managed include list (`alf add`).
pub use include::{
    normalize_include_path, prune_and_log_missing, IncludeList, INCLUDE_FILE, SYNC_LOG_FILE,
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
}
