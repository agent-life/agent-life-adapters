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

pub use alf_core::adapter::{
    ArchiveEnumeration, ExportReport, FileEntry, ImportOptions, ImportReport, WorkspaceEnumeration,
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
        import::import(alf_file, workspace, options.vault_key)
    }

    fn enumerate_workspace(&self, workspace: &Path) -> Result<WorkspaceEnumeration> {
        export::enumerate_workspace(workspace)
    }

    fn enumerate_archive(&self, alf_file: &Path) -> Result<ArchiveEnumeration> {
        import::enumerate_archive(alf_file)
    }
}
