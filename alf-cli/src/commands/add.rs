//! `alf add` — track a file so sync includes it.
//!
//! In-workspace files are recorded in `<workspace>/.alf-include.json` and packed
//! under `raw/{runtime}/` on the next `alf sync`. With `--external` (D3), a file
//! *outside* the workspace can be tracked too — but only under a host-blessed
//! root (`--allow-root`), never on the sensitive denylist, behind a human gate,
//! and packed under a sanitized `raw/{runtime}/external/` name. The include-list
//! machinery is runtime-agnostic (`alf_core::include`).

use crate::config::Config;
use crate::output;
use crate::selector;
use alf_core::{normalize_include_path, Adapter, IncludeList};
use anyhow::{bail, Result};
use colored::Colorize;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The `alf add` result. Also the `alf_track` MCP tool result (hence
/// `JsonSchema`).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AddResult {
    ok: bool,
    /// True if newly added; false if it was already tracked.
    added: bool,
    path: String,
    // Skipped when false but declared on a non-Option, so `#[serde(default)]` is
    // required or schemars over-requires it in the outputSchema (M2a §2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    external: bool,
}

#[derive(Serialize)]
struct BlessResult {
    ok: bool,
    blessed_root: String,
}

pub fn run(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    path: Option<&str>,
    external: bool,
    allow_root: Option<&Path>,
    yes_external: bool,
) -> Result<()> {
    let adapt = crate::adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{runtime}'. Supported runtimes: {}",
            crate::adapter::supported_runtimes()
        )
    })?;
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

    let workspace = resolve_workspace(runtime, workspace_flag, agent, adapt.as_ref())?;
    let workspace = workspace.as_path();

    let result = if external {
        add_external(runtime, workspace, path, yes_external)?
    } else {
        add_in_workspace(workspace, path)?
    };
    report(&result, runtime, workspace, human);
    Ok(())
}

/// MCP `alf_track` seam: track a workspace file (or, with `external`, a blessed
/// external file) and return the result — no stdout, no `--allow-root` blessing
/// (that trust-boundary expansion stays a CLI/human ceremony, design L10).
/// `external: true` carries its own consent (acts as the CLI's `--yes-external`),
/// but every other safety property holds: a pre-blessed root is still required
/// and the denylist still applies.
pub(crate) fn track(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    path: &str,
    external: bool,
) -> Result<AddResult> {
    let adapt = crate::adapter::get_adapter(runtime).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown runtime '{runtime}'. Supported runtimes: {}",
            crate::adapter::supported_runtimes()
        )
    })?;
    let workspace = resolve_workspace(runtime, workspace_flag, agent, adapt.as_ref())?;
    if external {
        add_external(runtime, &workspace, path, /* yes_external: */ true)
    } else {
        add_in_workspace(&workspace, path)
    }
}

/// Resolve the workspace an add targets: `-w` flag → the selection's workspace
/// (lazy init applies). Shared by the CLI `run` and the MCP `track` seam.
fn resolve_workspace(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    adapt: &dyn Adapter,
) -> Result<PathBuf> {
    let workspace: PathBuf = match workspace_flag {
        Some(w) => w.to_path_buf(),
        None => {
            let mut config = Config::load()?;
            let install = crate::commands::check::resolve_workspace(None, &config, runtime).path;
            selector::select_current_agent(&mut config, adapt, runtime, &install, agent)?.workspace
        }
    };

    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }
    Ok(workspace)
}

fn add_in_workspace(workspace: &Path, path: &str) -> Result<AddResult> {
    // Rejects missing files, paths outside the workspace, and the sentinels.
    let rel = normalize_include_path(workspace, path)?;
    let mut list = IncludeList::load(workspace)?;
    let added = list.add(&rel);
    if added {
        list.save(workspace)?;
    }
    Ok(AddResult {
        ok: true,
        added,
        path: rel,
        external: false,
    })
}

fn add_external(
    runtime: &str,
    workspace: &Path,
    path: &str,
    yes_external: bool,
) -> Result<AddResult> {
    // The reusable validation/denylist core is runtime-agnostic, and the
    // export-side packing of `external/` entries is wired for Hermes (the
    // motivating AGENTS.md case) and the generic runtime (which packs externals
    // under raw/generic/external/). Refuse elsewhere rather than silently
    // recording an entry no export will pack.
    if runtime != "hermes" && runtime != "generic" {
        bail!(
            "--external is currently supported only for the hermes and generic runtimes; \
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
    // The MCP `track` seam passes `yes_external: true` — the agent's `external:
    // true` argument IS its consent (the CLI's `--yes-external` equivalent).
    if !yes_external && !confirm_external(&canon)? {
        bail!("aborted: external add not confirmed");
    }

    let sanitized = alf_core::include::sanitized_external_name(&canon);
    let mut list = IncludeList::load(workspace)?;
    let added = list.add_external(&sanitized, &canon.to_string_lossy());
    if added {
        list.save(workspace)?;
    }
    Ok(AddResult {
        ok: true,
        added,
        path: canon.to_string_lossy().into_owned(),
        external: true,
    })
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

fn report(result: &AddResult, runtime: &str, workspace: &Path, human: bool) {
    if human {
        let kind = if result.external { "external file" } else { "" };
        if result.added {
            println!(
                "{} Tracking {kind} {} for sync",
                "✓".green().bold(),
                result.path
            );
        } else {
            println!("{} {} is already tracked", "✓".green().bold(), result.path);
        }
        println!(
            "  Included in the next: alf sync -r {runtime} -w {}",
            workspace.display()
        );
    } else {
        output::json(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The external-tracking gate (D3): only hermes and generic are wired for
    /// `raw/{runtime}/external/` packing. Other runtimes bail at the runtime gate
    /// before touching the include list; hermes and generic pass it (and then
    /// fail later on the blessed-root requirement, which proves they got past the
    /// gate itself). Pins that extending the gate to generic did not open it to
    /// every runtime — hermes stays exactly as before.
    #[test]
    fn external_gate_allows_only_hermes_and_generic() {
        let ws = TempDir::new().unwrap();
        let gate_phrase = "hermes and generic";

        // A non-wired runtime bails at the gate with the gate message.
        for unwired in ["openclaw", "zeroclaw"] {
            let err = add_external(unwired, ws.path(), "/etc/hosts", true).unwrap_err();
            assert!(
                format!("{err:#}").contains(gate_phrase),
                "{unwired} external add must bail at the runtime gate: {err:#}"
            );
        }

        // Wired runtimes pass the gate — they fail later (no blessed roots / the
        // denylist), never with the gate message.
        for wired in ["hermes", "generic"] {
            let err = add_external(wired, ws.path(), "/etc/hosts", true).unwrap_err();
            assert!(
                !format!("{err:#}").contains(gate_phrase),
                "{wired} must pass the runtime gate, not bail on it: {err:#}"
            );
        }
    }
}
