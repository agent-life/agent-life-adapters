//! `alf export` — export an agent workspace to an .alf archive.

use crate::adapter;
use anyhow::{bail, Result};
use colored::Colorize;
use std::path::Path;

pub fn run(runtime: &str, workspace: &Path, output: Option<&Path>) -> Result<()> {
    // Resolve adapter
    let adapter = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    // Validate workspace exists
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

    // Determine output path
    let default_output;
    let output_path = match output {
        Some(p) => p,
        None => {
            // Default: ./<workspace-dir-name>.alf
            let dir_name = workspace
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "agent".into());
            default_output = Path::new(".").join(format!("{dir_name}.alf"));
            &default_output
        }
    };

    println!(
        "{} Exporting {} workspace...",
        "▸".blue().bold(),
        adapter.name()
    );
    println!("  Workspace: {}", workspace.display());
    println!("  Output:    {}", output_path.display());
    println!();

    // Run export
    let report = adapter.export(workspace, output_path)?;

    // Print summary
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