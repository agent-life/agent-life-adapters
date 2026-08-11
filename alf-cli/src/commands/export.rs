//! `alf export` — export an agent workspace to an .alf archive.

use crate::adapter;
use crate::config::Config;
use crate::output;
use crate::selector;
use alf_core::{Adapter, FileEntry};
use anyhow::{bail, Result};
use colored::Colorize;
use schemars::JsonSchema;
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
    excluded_by_alfignore: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// The `alf export --dry-run` preview. Also the `alf_export_dry_run` MCP tool
/// result — hence the `JsonSchema` derive (the FileEntry field is schema-shimmed,
/// as `alf_core::FileEntry` cannot derive `JsonSchema`).
#[derive(Serialize, JsonSchema)]
pub(crate) struct ExportDryRunResult {
    ok: bool,
    dry_run: bool,
    agent_name: String,
    memory_records: u64,
    #[schemars(with = "Vec<crate::schema::FileEntrySchema>")]
    files: Vec<FileEntry>,
    excluded_by_alfignore: u32,
    total_size: u64,
    // Skipped when empty but declared on a non-Option, so `#[serde(default)]` is
    // required or schemars over-requires it in the outputSchema (M2a §2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

pub fn run(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    output_arg: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    let human = output::human_mode();

    let adapter = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;

    let mut config = Config::load()?;

    // --dry-run writes nothing (CLI-1/IN-2) — including the mapping and the
    // workspace's .alf-agent-id — but must preview the same workspace the real
    // export would use. With a mapping present the selector is read-only
    // (lazy init only fires on an empty mapping), so honor --agent/ALF_AGENT
    // and the selection's workspace; only an empty mapping keeps the legacy
    // selector-free path.
    if dry_run {
        let workspace = resolve_dry_run_workspace(
            &mut config,
            adapter.as_ref(),
            runtime,
            workspace_flag,
            agent,
        )?;
        return run_dry_run(adapter.as_ref(), &workspace, human);
    }

    // Selection (lazy init applies): the mapping's id becomes the archive
    // identity. An explicit -w pointing elsewhere is an ad-hoc export and
    // keeps the legacy id-less path.
    let install =
        crate::commands::check::resolve_workspace_required(workspace_flag, &config, runtime)?;
    let selected =
        selector::select_current_agent(&mut config, adapter.as_ref(), runtime, &install, agent)?;

    // WP1: move any legacy vault/key to the per-agent layout before export —
    // the adapter reads only the per-agent vault path (no legacy fallback).
    crate::vault_migrate::require_migrated_locked(&config, &selected.runtime)?;

    let (workspace, adhoc) = selector::effective_workspace(&selected, workspace_flag);
    let workspace = workspace.as_path();

    // A mapped per-agent workspace is adapter-owned and may not exist yet —
    // `export_agent` creates it. Only validate an explicit -w target.
    if adhoc {
        check_workspace_dir(workspace)?;
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

    let report = if adhoc {
        adapter.export(workspace, output_path)?
    } else {
        let mut binding = selected.binding.clone();
        binding.workspace = workspace.to_path_buf();
        adapter.export_agent(&binding, selected.alf_agent_id, output_path)?
    };

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

        if report.excluded_by_alfignore > 0 {
            println!(
                "  Excluded:    {} file(s) by .alfignore",
                report.excluded_by_alfignore
            );
        }

        let size = format_size(report.output_size_bytes);
        println!("  File size:   {size}");
        for w in &report.warnings {
            println!("  {} {}", "!".yellow().bold(), w);
        }
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
            excluded_by_alfignore: report.excluded_by_alfignore,
            warnings: report.warnings.clone(),
        });
    }

    Ok(())
}

fn check_workspace_dir(workspace: &Path) -> Result<()> {
    if !workspace.exists() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }
    if !workspace.is_dir() {
        bail!("Workspace path is not a directory: {}", workspace.display());
    }
    Ok(())
}

/// Resolve the workspace an `export --dry-run` should preview. `--dry-run` writes
/// nothing (CLI-1/IN-2) — including the mapping and `.alf-agent-id` — but must
/// preview the same workspace the real export would use. With a mapping present
/// the selector is read-only (lazy init only fires on an empty mapping), so honor
/// `--agent`/`ALF_AGENT` and the selection's workspace; only an empty mapping
/// keeps the legacy selector-free path. Shared by the CLI dry-run branch and the
/// MCP `alf_export_dry_run` tool.
fn resolve_dry_run_workspace(
    config: &mut Config,
    adapter: &dyn Adapter,
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
) -> Result<std::path::PathBuf> {
    let (workspace, adhoc) = if config.agents_for_runtime(runtime).is_empty() {
        (
            config.resolve_workspace(workspace_flag.map(Path::to_path_buf))?,
            true,
        )
    } else {
        let install =
            crate::commands::check::resolve_workspace_required(workspace_flag, config, runtime)?;
        let selected = selector::select_current_agent(config, adapter, runtime, &install, agent)?;
        selector::effective_workspace(&selected, workspace_flag)
    };
    // Only validate an explicit -w target. A mapped per-agent workspace is
    // adapter-owned and may not exist on disk (shared-store runtimes derive the
    // install root from it); the adapter's dry-run tolerates that.
    if adhoc {
        check_workspace_dir(&workspace)?;
    }
    Ok(workspace)
}

/// Enumerate the upload set for a resolved workspace and build the dry-run
/// result — no printing, no writes. The seam both the CLI JSON path and the MCP
/// `alf_export_dry_run` tool build their output from.
fn build_dry_run_result(adapter: &dyn Adapter, workspace: &Path) -> Result<ExportDryRunResult> {
    let preview = adapter.enumerate_workspace(workspace)?;
    Ok(ExportDryRunResult {
        ok: true,
        dry_run: true,
        agent_name: preview.agent_name,
        memory_records: preview.memory_records,
        files: preview.files,
        excluded_by_alfignore: preview.excluded_by_alfignore,
        total_size: preview.total_size,
        warnings: preview.warnings,
    })
}

/// MCP `alf_export_dry_run` seam: resolve the workspace and build the preview
/// (no stdout — the caller renders). Never writes.
pub(crate) fn dry_run_result(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
) -> Result<ExportDryRunResult> {
    let mut config = Config::load()?;
    let adapter = adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{}'. Supported: {}",
            runtime,
            adapter::supported_runtimes()
        )
    })?;
    let workspace = resolve_dry_run_workspace(
        &mut config,
        adapter.as_ref(),
        runtime,
        workspace_flag,
        agent,
    )?;
    build_dry_run_result(adapter.as_ref(), &workspace)
}

/// `alf export --dry-run` — enumerate the upload set, write nothing.
fn run_dry_run(adapter: &dyn Adapter, workspace: &Path, human: bool) -> Result<()> {
    if human {
        println!(
            "{} Previewing {} export (dry run — nothing will be written)...",
            "▸".blue().bold(),
            adapter.name()
        );
        println!("  Workspace: {}", workspace.display());
        println!();
    } else {
        output::progress(&format!(
            "Previewing {} export (dry run)...",
            adapter.name()
        ));
    }

    let result = build_dry_run_result(adapter, workspace)?;

    if human {
        println!(
            "{} Dry run complete — no archive written",
            "✓".green().bold()
        );
        println!();
        println!("  Agent:     {}", result.agent_name);
        println!("  Memories:  {}", result.memory_records);
        println!("  Files:     {}", result.files.len());
        for f in &result.files {
            println!("    {} ({})", f.path, format_size(f.size));
        }
        if result.excluded_by_alfignore > 0 {
            println!(
                "  Excluded:  {} file(s) by .alfignore",
                result.excluded_by_alfignore
            );
        }
        println!("  Total:     {}", format_size(result.total_size));
        if !result.warnings.is_empty() {
            println!();
            println!("  {} Warnings:", "⚠".yellow().bold());
            for w in &result.warnings {
                println!("    • {w}");
            }
        }
    } else {
        output::json(&result);
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
