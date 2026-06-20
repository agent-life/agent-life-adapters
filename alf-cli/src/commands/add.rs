//! `alf add` — track a file so sync includes it.
//!
//! In-workspace files are recorded in `<workspace>/.alf-include.json` and packed
//! under `raw/{runtime}/` on the next `alf sync`. With `--external` (D3), a file
//! *outside* the workspace can be tracked too — but only under a host-blessed
//! root (`--allow-root`), never on the sensitive denylist, behind a human gate,
//! and packed under a sanitized `raw/{runtime}/external/` name. The include-list
//! machinery is runtime-agnostic (`alf_core::include`).

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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    external: bool,
}

#[derive(Serialize)]
struct BlessResult {
    ok: bool,
    blessed_root: String,
}

pub fn run(
    runtime: &str,
    workspace: Option<&Path>,
    path: Option<&str>,
    external: bool,
    allow_root: Option<&Path>,
    yes_external: bool,
) -> Result<()> {
    if crate::adapter::get_adapter(runtime).is_none() {
        bail!(
            "Unknown runtime '{runtime}'. Supported runtimes: {}",
            crate::adapter::supported_runtimes()
        );
    }
    let human = output::human_mode();

    // `--allow-root` blesses a host-local external root (policy never archived).
    if let Some(root) = allow_root {
        let blessed = alf_core::include::add_allowed_root(root)?;
        if human {
            println!(
                "{} Blessed external root {}",
                "✓".green().bold(),
                blessed.display()
            );
        } else if path.is_none() {
            output::json(&BlessResult {
                ok: true,
                blessed_root: blessed.to_string_lossy().to_string(),
            });
        }
        if path.is_none() {
            return Ok(()); // allow-root-only invocation
        }
    }

    let path = path.ok_or_else(|| {
        anyhow::anyhow!("a file path is required (or pass --allow-root <dir> on its own)")
    })?;
    let workspace = workspace.ok_or_else(|| {
        anyhow::anyhow!("no workspace specified — pass -w <path> or set a default")
    })?;

    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }

    if external {
        add_external(runtime, workspace, path, yes_external, human)
    } else {
        add_in_workspace(runtime, workspace, path, human)
    }
}

fn add_in_workspace(runtime: &str, workspace: &Path, path: &str, human: bool) -> Result<()> {
    // Rejects missing files, paths outside the workspace, and the sentinels.
    let rel = normalize_include_path(workspace, path)?;
    let mut list = IncludeList::load(workspace)?;
    let added = list.add(&rel);
    if added {
        list.save(workspace)?;
    }
    report(runtime, workspace, &rel, added, false, human);
    Ok(())
}

fn add_external(
    runtime: &str,
    workspace: &Path,
    path: &str,
    yes_external: bool,
    human: bool,
) -> Result<()> {
    // The reusable validation/denylist core is runtime-agnostic, but the
    // export-side packing of `external/` entries is currently wired for Hermes
    // (the motivating AGENTS.md case). Refuse elsewhere rather than silently
    // recording an entry no export will pack.
    if runtime != "hermes" {
        bail!(
            "--external is currently supported only for the hermes runtime; \
             {runtime} external-file tracking is not yet wired"
        );
    }
    let roots = alf_core::include::load_allowed_roots();
    if roots.is_empty() {
        bail!(
            "no external roots are blessed — run `alf add --allow-root <dir>` first \
             (the file must live under a blessed directory)"
        );
    }
    // Canonicalize + symlink-resolve, enforce denylist + allowed-root.
    let canon = alf_core::include::validate_external_source(Path::new(path), &roots)?;

    // Human gate: a trust-boundary crossing must not be silently agent-invokable.
    if !yes_external && !confirm_external(&canon)? {
        bail!("aborted: external add not confirmed");
    }

    let sanitized = alf_core::include::sanitized_external_name(&canon);
    let mut list = IncludeList::load(workspace)?;
    let added = list.add_external(&sanitized, &canon.to_string_lossy());
    if added {
        list.save(workspace)?;
    }
    report(
        runtime,
        workspace,
        &canon.to_string_lossy(),
        added,
        true,
        human,
    );
    Ok(())
}

fn confirm_external(canon: &Path) -> Result<bool> {
    use std::io::{BufRead, Write};
    eprint!(
        "Track EXTERNAL file {} for this agent's sync? It reaches outside the workspace. [y/N] ",
        canon.display()
    );
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

fn report(runtime: &str, workspace: &Path, label: &str, added: bool, external: bool, human: bool) {
    if human {
        let kind = if external { "external file" } else { "" };
        if added {
            println!("{} Tracking {kind} {label} for sync", "✓".green().bold());
        } else {
            println!("{} {label} is already tracked", "✓".green().bold());
        }
        println!(
            "  Included in the next: alf sync -r {runtime} -w {}",
            workspace.display()
        );
    } else {
        output::json(&AddResult {
            ok: true,
            added,
            path: label.to_string(),
            external,
        });
    }
}
