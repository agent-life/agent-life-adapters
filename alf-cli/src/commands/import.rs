//! `alf import` — import an .alf archive into an agent workspace.

use crate::adapter;
use crate::config::Config;
use crate::output;
use crate::selector;
use crate::vault_key::{self, VaultKeyArgs};
use crate::vault_migrate;
use alf_core::{ImportOptions, RestoreMode};
use anyhow::{bail, Result};
use colored::Colorize;
use serde::Serialize;
use std::path::Path;
use uuid::Uuid;

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

pub fn run(
    runtime: &str,
    alf_file: &Path,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    mode: RestoreMode,
    key_args: &VaultKeyArgs,
) -> Result<()> {
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

    let mut config = Config::load()?;

    // WP0: resolve the selection against the existing mapping only. Import is
    // how a workspace gets created, so first contact must NOT seed a mapping
    // row from an empty target — and ad-hoc migration imports keep working.
    let selected = match (config.agents_for_runtime(runtime).is_empty(), agent) {
        (true, None) => None,
        (true, Some(sel)) => {
            // No mapping: an explicit UUID selector verifies the archive
            // directly (fail closed), then imports the legacy way.
            match Uuid::parse_str(sel) {
                Ok(id) => {
                    alf_core::verify_archive_agent(alf_file, id)?;
                    None
                }
                Err(_) => return Err(selector::agent_not_found(runtime, sel, &config).into()),
            }
        }
        // A UUID selector that is not in the mapping is the escape hatch
        // verify_archive_agent's own error advertises ("pass --agent
        // <archive-id> to import it as its own agent") — it must work on a
        // mapped host too: verify the archive directly, import the legacy way.
        (false, Some(sel))
            if config.find_agent(runtime, sel).is_none() && Uuid::parse_str(sel).is_ok() =>
        {
            let id = Uuid::parse_str(sel).expect("guard checked");
            alf_core::verify_archive_agent(alf_file, id)?;
            None
        }
        (false, _) => {
            let install =
                crate::commands::check::resolve_workspace(workspace_flag, &config, runtime).path;
            Some(selector::select_current_agent(
                &mut config,
                adapter.as_ref(),
                runtime,
                &install,
                agent,
            )?)
        }
    };

    // WP1: move any legacy vault/key to the per-agent layout before the
    // adapter restores Layer 4 — adapters have no legacy fallback, and an
    // unmigrated legacy file would survive as a shadow vault.
    vault_migrate::require_migrated_locked(&config, runtime)?;

    let (workspace, adhoc) = match &selected {
        Some(sel) => selector::effective_workspace(sel, workspace_flag),
        None => (
            config.resolve_workspace(workspace_flag.map(Path::to_path_buf))?,
            false,
        ),
    };
    let workspace = workspace.as_path();

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

    let resolved_key =
        vault_key::resolve(key_args, runtime, selected.as_ref().map(|s| s.alf_agent_id))?;
    if let Some((_, source)) = &resolved_key {
        output::progress(&format!(
            "Using vault key from {} — credentials will be decrypted and restored",
            source.label()
        ));
    }
    let options = ImportOptions {
        vault_key: resolved_key.as_ref().map(|(k, _)| k),
        mode,
        // `alf import` is an explicit live import, never a sandboxed preview.
        preview: false,
    };
    // Importing into the selected agent's own workspace fails closed on a
    // wrong-agent archive; an ad-hoc -w target keeps the legacy path.
    let report = match &selected {
        Some(sel) if !adhoc => {
            let mut binding = sel.binding.clone();
            binding.workspace = workspace.to_path_buf();
            adapter.import_agent(&binding, sel.alf_agent_id, alf_file, options)?
        }
        _ => adapter.import_with_options(alf_file, workspace, options)?,
    };

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
