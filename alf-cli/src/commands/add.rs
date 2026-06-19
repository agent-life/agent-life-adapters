//! `alf add` — track an arbitrary workspace file so sync includes it.
//!
//! ALF never auto-discovers arbitrary files; the agent opts each one in
//! explicitly. This records the file in `<workspace>/.alf-include.json`, which
//! the next `alf sync` reads and includes in `raw/{runtime}/`. The include-list
//! machinery is runtime-agnostic (alf_core::include), so this works for any
//! adapter whose `export` packs the tracked files — both openclaw and zeroclaw.

use crate::output;
use alf_core::{normalize_include_path, IncludeList};
use anyhow::{bail, Result};
use colored::Colorize;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct AddResult {
    ok: bool,
    /// True if newly added; false if it was already tracked.
    added: bool,
    path: String,
}

pub fn run(runtime: &str, workspace: &Path, path: &str) -> Result<()> {
    // Validate the runtime so an unknown one fails clearly here rather than at
    // sync time. The include list itself is runtime-agnostic.
    if crate::adapter::get_adapter(runtime).is_none() {
        bail!(
            "Unknown runtime '{runtime}'. Supported runtimes: {}",
            crate::adapter::supported_runtimes()
        );
    }
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    // Validate + normalize to a workspace-relative path (rejects missing files,
    // paths outside the workspace, and the alf-managed sentinel files).
    let rel = normalize_include_path(workspace, path)?;

    let mut list = IncludeList::load(workspace)?;
    let added = list.add(&rel);
    if added {
        list.save(workspace)?;
    }

    if output::human_mode() {
        if added {
            println!("{} Tracking {} for sync", "✓".green().bold(), rel);
        } else {
            println!("{} {} is already tracked", "✓".green().bold(), rel);
        }
        println!(
            "  Included in the next: alf sync -r {runtime} -w {}",
            workspace.display()
        );
    } else {
        output::json(&AddResult {
            ok: true,
            added,
            path: rel,
        });
    }
    Ok(())
}
