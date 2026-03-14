//! `alf import` — import an .alf archive into an agent workspace.

use crate::adapter;
use crate::output;
use anyhow::{bail, Result};
use colored::Colorize;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct ImportResult {
    ok: bool,
    workspace: String,
    agent_name: String,
    memory_records: u64,
    identity_imported: bool,
    principals_count: u32,
    credentials_count: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

pub fn run(runtime: &str, alf_file: &Path, workspace: &Path) -> Result<()> {
    let human = output::human_mode();

    let adapter = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    if !alf_file.exists() {
        bail!("ALF file does not exist: {}", alf_file.display());
    }
    if !alf_file.is_file() {
        bail!("ALF path is not a file: {}", alf_file.display());
    }

    if human {
        println!(
            "{} Importing into {} workspace...",
            "▸".blue().bold(),
            adapter.name()
        );
        println!("  ALF file:  {}", alf_file.display());
        println!("  Workspace: {}", workspace.display());
        println!();
    } else {
        output::progress(&format!("Importing into {} workspace...", adapter.name()));
    }

    let report = adapter.import(alf_file, workspace)?;

    if human {
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
    } else {
        output::json(&ImportResult {
            ok: true,
            workspace: workspace.to_string_lossy().into(),
            agent_name: report.agent_name.clone(),
            memory_records: report.memory_records,
            identity_imported: report.identity_imported,
            principals_count: report.principals_count,
            credentials_count: report.credentials_count,
            warnings: report.warnings.clone(),
        });
    }

    Ok(())
}
