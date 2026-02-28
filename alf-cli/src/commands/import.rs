//! `alf import` — import an .alf archive into an agent workspace.

use crate::adapter;
use anyhow::{bail, Result};
use colored::Colorize;
use std::path::Path;

pub fn run(runtime: &str, alf_file: &Path, workspace: &Path) -> Result<()> {
    // Resolve adapter
    let adapter = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    // Validate ALF file exists
    if !alf_file.exists() {
        bail!("ALF file does not exist: {}", alf_file.display());
    }
    if !alf_file.is_file() {
        bail!("ALF path is not a file: {}", alf_file.display());
    }

    println!(
        "{} Importing into {} workspace...",
        "▸".blue().bold(),
        adapter.name()
    );
    println!("  ALF file:  {}", alf_file.display());
    println!("  Workspace: {}", workspace.display());
    println!();

    // Run import
    let report = adapter.import(alf_file, workspace)?;

    // Print summary
    println!("{} Import complete", "✓".green().bold());
    println!();
    println!("  Agent:       {}", report.agent_name);
    println!("  Memories:    {}", report.memory_records);

    if report.identity_imported {
        println!("  Identity:    imported");
    }
    if report.principals_count > 0 {
        println!("  Principals:  {}", report.principals_count);
    }
    if report.credentials_count > 0 {
        println!("  Credentials: {}", report.credentials_count);
    }

    if !report.warnings.is_empty() {
        println!();
        println!("  {} Warnings:", "⚠".yellow().bold());
        for w in &report.warnings {
            println!("    • {w}");
        }
    }

    Ok(())
}