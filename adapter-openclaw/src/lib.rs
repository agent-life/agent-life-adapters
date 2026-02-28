//! # adapter-openclaw
//!
//! OpenClaw framework adapter for the Agent Life Format (ALF). Translates
//! between OpenClaw's native file-based workspace and the ALF archive format.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use adapter_openclaw::OpenClawAdapter;
//! use alf_cli::adapter::Adapter;
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

pub mod credential_map;
pub mod export;
pub mod identity_parser;
pub mod import;
pub mod memory_parser;
pub mod principals_parser;

// ---------------------------------------------------------------------------
// Report types (shared with alf-cli adapter trait)
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
// Adapter implementation
// ---------------------------------------------------------------------------

/// OpenClaw framework adapter.
///
/// Implements export (workspace → ALF archive) and import (ALF archive →
/// workspace) for real OpenClaw installations.
pub struct OpenClawAdapter;

impl OpenClawAdapter {
    /// Runtime identifier.
    pub fn name(&self) -> &str {
        "openclaw"
    }

    /// Human-readable description.
    pub fn description(&self) -> &str {
        "OpenClaw framework — file-based Markdown agent workspace"
    }

    /// Export an OpenClaw workspace to an `.alf` archive.
    pub fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
        export::export(workspace, output)
    }

    /// Import an `.alf` archive into an OpenClaw workspace.
    pub fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        import::import(alf_file, workspace)
    }
}