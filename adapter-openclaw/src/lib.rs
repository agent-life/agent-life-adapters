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
pub use alf_core::adapter::{ExportReport, ImportReport};
pub use alf_core::Adapter;

pub mod credential_map;
pub mod export;
pub mod identity_parser;
pub mod import;
pub mod memory_parser;
pub mod principals_parser;

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

    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        import::import(alf_file, workspace)
    }
}