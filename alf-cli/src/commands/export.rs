//! `alf export` — export an agent workspace to an .alf archive.

use crate::adapter;
use crate::output;
use anyhow::{bail, Result};
use colored::Colorize;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct ExportResult {
    ok: bool,
    output: String,
    agent_name: String,
    alf_version: String,
    memory_records: u64,
    file_size: u64,
}

pub fn run(runtime: &str, workspace: &Path, output_arg: Option<&Path>) -> Result<()> {
    let human = output::human_mode();

    let adapter = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    if !workspace.exists() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }
    if !workspace.is_dir() {
        bail!(
            "Workspace path is not a directory: {}",
            workspace.display()
        );
    }

    let default_output;
    let output_path = match output_arg {
        Some(p) => p,
        None => {
            let dir_name = workspace
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "agent".into());
            default_output = Path::new(".").join(format!("{dir_name}.alf"));
            &default_output
        }
    };

    if human {
        println!(
            "{} Exporting {} workspace...",
            "▸".blue().bold(),
            adapter.name()
        );
        println!("  Workspace: {}", workspace.display());
        println!("  Output:    {}", output_path.display());
        println!();
    } else {
        output::progress(&format!("Exporting {} workspace...", adapter.name()));
    }

    let report = adapter.export(workspace, output_path)?;

    if human {
        println!("{} Export complete", "✓".green().bold());
        println!();
        println!("  Agent:       {}", report.agent_name);
        println!("  ALF version: {}", report.alf_version);
        println!("  Memories:    {}", report.memory_records);

        if let Some(v) = report.identity_version {
            println!("  Identity:    v{v}");
        }
        if report.principals_count > 0 {
            println!("  Principals:  {}", report.principals_count);
        }
        if report.credentials_count > 0 {
            println!("  Credentials: {}", report.credentials_count);
        }
        if report.attachments_count > 0 {
            println!("  Attachments: {}", report.attachments_count);
        }
        if !report.raw_sources.is_empty() {
            println!("  Raw sources: {}", report.raw_sources.join(", "));
        }

        let size = format_size(report.output_size_bytes);
        println!("  File size:   {size}");
        println!();
        println!("  {}", report.output_path);
    } else {
        output::json(&ExportResult {
            ok: true,
            output: report.output_path.clone(),
            agent_name: report.agent_name.clone(),
            alf_version: report.alf_version.clone(),
            memory_records: report.memory_records,
            file_size: report.output_size_bytes,
        });
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
