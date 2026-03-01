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

pub use alf_core::adapter::{ExportReport, ImportReport};
pub use alf_core::Adapter;

pub mod config_parser;
pub mod credential_map;
pub mod export;
pub mod identity_parser;
pub mod import;
pub mod markdown_parser;
pub mod principals_parser;
pub mod sqlite_extractor;

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

    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        import::import(alf_file, workspace)
    }
}