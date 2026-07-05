//! Export a ZeroClaw workspace to an `.alf` archive.
//!
//! Orchestrates: detect backend from `config.toml` → extract memory (SQLite
//! or Markdown) → build identity/principals/credentials → write archive.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::{
    AgentMetadata, AlfWriter, CredentialsLayerInfo, FileEntry, IdentityLayerInfo, LayerInventory,
    Manifest, MemoryInventory, MemoryPartitionInfo, PartitionAssigner, PrincipalsLayerInfo,
    WorkspaceEnumeration,
};

use alf_core::{AgentBinding, MemorySource};

use crate::brain_db;
use crate::config_parser::{self, MemoryBackend, ZeroClawConfig};
use crate::identity_parser;
use crate::markdown_parser;
use crate::principals_parser;
use crate::sqlite_extractor;
use crate::ExportReport;

// ---------------------------------------------------------------------------
// Agent ID persistence
// ---------------------------------------------------------------------------

/// UUID v5 namespace for deriving agent IDs from workspace paths.
const AGENT_ID_NS: Uuid = Uuid::from_bytes([
    0x61, 0x6c, 0x66, 0x2d, 0x7a, 0x63, 0x6c, 0x77, // "alf-zclw"
    0x2d, 0x61, 0x67, 0x65, 0x6e, 0x74, 0x2d, 0x31, // "-agent-1"
]);

/// Resolve the agent UUID without writing anything.
///
/// Reads `{workspace}/.alf-agent-id` if present, otherwise derives a
/// deterministic UUID v5 from the canonical workspace path. A freshly-derived
/// id is **not** persisted — the read-only path used by `export --dry-run` and
/// the adapter's `Adapter::resolve_agent_id` (WP0 selector/discovery).
pub(crate) fn resolve_agent_id_readonly(workspace: &Path) -> Result<Uuid> {
    let id_file = workspace.join(".alf-agent-id");
    if id_file.is_file() {
        let raw = fs::read_to_string(&id_file).context("Failed to read .alf-agent-id")?;
        let id = Uuid::parse_str(raw.trim()).context("Invalid UUID in .alf-agent-id")?;
        return Ok(id);
    }
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    Ok(Uuid::new_v5(
        &AGENT_ID_NS,
        canonical.to_string_lossy().as_bytes(),
    ))
}

/// Read or generate the agent UUID, persisting a freshly-derived id.
fn resolve_agent_id(workspace: &Path) -> Result<Uuid> {
    let id = resolve_agent_id_readonly(workspace)?;
    let id_file = workspace.join(".alf-agent-id");
    if !id_file.is_file() {
        let _ = fs::write(&id_file, id.to_string());
    }
    Ok(id)
}

// ---------------------------------------------------------------------------
// ZeroClaw directory detection
// ---------------------------------------------------------------------------

/// The real shared store location relative to the install root.
pub(crate) fn brain_db_path(install: &Path) -> PathBuf {
    install.join("data").join("memory").join("brain.db")
}

/// Locate the ZeroClaw install root from any workspace path.
///
/// The install root is the directory that holds `config.toml` (and/or the
/// shared `data/memory/brain.db`). It is found by walking up from `workspace`
/// — this resolves the flat layout (`workspace == install root`, the real and
/// harness install), the multi-agent per-agent binding
/// (`<root>/agents/<alias>/workspace`), and the legacy `<root>/workspace/`
/// subdir uniformly. Falls back to `workspace` itself.
pub(crate) fn zeroclaw_home(workspace: &Path) -> std::path::PathBuf {
    for anc in workspace.ancestors().take(6) {
        if anc.join("config.toml").is_file() || brain_db_path(anc).is_file() {
            return anc.to_path_buf();
        }
    }
    workspace.to_path_buf()
}

/// Locate the shared `brain.db` under an install root (capture plan D9): the
/// canonical `data/memory/brain.db`, then the older `memory/brain.db`, then the
/// fictional flat `memory.db`, then a shallow search for any `brain.db`.
fn resolve_brain_db(install: &Path) -> Option<PathBuf> {
    let candidates = [
        brain_db_path(install),
        install.join("memory").join("brain.db"),
        install.join("memory.db"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    // Shallow bounded search (depth ≤ 3) for a stray brain.db.
    WalkDir::new(install)
        .max_depth(3)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().is_file() && e.file_name() == "brain.db")
        .map(|e| e.path().to_path_buf())
}

/// Try to detect ZeroClaw version from workspace files or environment.
fn detect_zeroclaw_version(zc_home: &Path) -> Option<String> {
    // Check config.toml for a version field
    let config_path = zc_home.join("config.toml");
    if let Ok(content) = fs::read_to_string(&config_path) {
        if let Ok(val) = content.parse::<toml::Value>() {
            if let Some(v) = val.get("version").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
    }
    // 0.8.2's config.toml carries `schema_version` but no top-level `version`,
    // so fall back to the installed binary: `zeroclaw --version` prints e.g.
    // "zeroclaw 0.8.2". Host unit tests (no binary on PATH) yield None; live
    // runs execute `alf` inside the container where `zeroclaw` resolves.
    detect_zeroclaw_version_from_binary()
}

/// `zeroclaw --version` → first `X.Y.Z`, or `None` when the binary is absent.
fn detect_zeroclaw_version_from_binary() -> Option<String> {
    let output = std::process::Command::new("zeroclaw")
        .arg("--version")
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stdout.trim().is_empty() {
        String::from_utf8_lossy(&output.stderr).into_owned()
    } else {
        stdout.into_owned()
    };
    extract_semver(&text)
}

/// First `<digits>.<digits>.<digits>` run in `text`, if any. Hand-rolled to
/// avoid a regex dependency.
fn extract_semver(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            let parts: Vec<&str> = run.split('.').collect();
            if parts.len() >= 3
                && parts[..3]
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                return Some(format!("{}.{}.{}", parts[0], parts[1], parts[2]));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Best-effort top-level `schema_version` from ZeroClaw `config.toml` (=3 on
/// 0.8.2), or `None` when absent. Recorded alongside `source_runtime_version`.
fn detect_config_schema_version(zc_home: &Path) -> Option<i64> {
    let content = fs::read_to_string(zc_home.join("config.toml")).ok()?;
    content
        .parse::<toml::Value>()
        .ok()?
        .get("schema_version")?
        .as_integer()
}

// ---------------------------------------------------------------------------
// Raw source collection
// ---------------------------------------------------------------------------

/// Root-level workspace files to preserve.
const ROOT_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "TOOLS.md",
    "HEARTBEAT.md",
];

/// The set of files an export would archive, before their contents are read.
pub struct EnumerationResult {
    pub files: Vec<FileEntry>,
    pub excluded_by_alfignore: u32,
    pub alfignore_warnings: Vec<String>,
    /// Paths in the agent's `alf add` include list that no longer exist on disk.
    pub missing_includes: Vec<String>,
}

/// Where a raw-source entry's bytes come from.
enum RawContent {
    /// Read verbatim from this absolute path.
    Disk(PathBuf),
    /// Synthesized bytes — ZeroClaw's redacted `config.toml`.
    Inline(Vec<u8>),
}

/// What `enumerate_raw` returns: the raw entries (each paired with where its
/// bytes come from), the `.alfignore` exclusion count, any warnings, and the
/// tracked-but-missing include paths.
type RawEnumeration = (Vec<(FileEntry, RawContent)>, u32, Vec<String>, Vec<String>);

/// Load `<workspace>/.alfignore` into a gitignore matcher.
///
/// A missing file yields an empty matcher (nothing excluded) and no warning.
/// A malformed file also yields an empty matcher plus a warning — filtering is
/// skipped rather than failing the export.
fn load_alfignore(workspace: &Path) -> (Gitignore, Vec<String>) {
    let path = workspace.join(".alfignore");
    if !path.is_file() {
        return (Gitignore::empty(), Vec::new());
    }
    let mut warnings = Vec::new();
    let mut builder = GitignoreBuilder::new(workspace);
    if let Some(err) = builder.add(&path) {
        warnings.push(format!(
            ".alfignore could not be read ({err}); continuing without filtering"
        ));
        return (Gitignore::empty(), warnings);
    }
    match builder.build() {
        Ok(matcher) => (matcher, warnings),
        Err(err) => {
            warnings.push(format!(
                ".alfignore is unparseable ({err}); continuing without filtering"
            ));
            (Gitignore::empty(), warnings)
        }
    }
}

/// Whether a workspace-relative path is excluded by the `.alfignore` matcher.
fn is_alfignored(matcher: &Gitignore, rel: &str) -> bool {
    matcher
        .matched_path_or_any_parents(rel, /* is_dir = */ false)
        .is_ignore()
}

/// Enumerate every raw-source entry alongside the location of its bytes.
///
/// Two buckets, deliberately treated differently by `.alfignore`:
/// - **Filterable** — the workspace-relative [`ROOT_FILES`] and `memory/**`.
///   `.alfignore` patterns (which are workspace-relative) apply to these.
/// - **Unfilterable** — the synthesized, redacted `config.toml` and the AIEOS
///   `identity.json` (which may live outside the workspace entirely). A
///   workspace-relative `.alfignore` pattern cannot meaningfully address these,
///   so they are always included, never matched, and never counted.
fn enumerate_raw(install: &Path, config: &ZeroClawConfig) -> RawEnumeration {
    let workspace = install;
    let (matcher, mut warnings) = load_alfignore(workspace);
    let mut entries: Vec<(FileEntry, RawContent)> = Vec::new();
    let mut excluded: u32 = 0;
    let mut missing_includes: Vec<String> = Vec::new();

    // config.toml — synthesized & redacted; unfilterable.
    let redacted = config_parser::redact_secrets(&config.raw_toml).into_bytes();
    entries.push((
        FileEntry {
            path: "config.toml".to_string(),
            size: redacted.len() as u64,
        },
        RawContent::Inline(redacted),
    ));

    // brain.db schema sidecar — synthesized & unfilterable, like config.toml.
    // Consumed by restore to bootstrap a lazily-absent store; skipped by name on
    // restore so it never lands in the workspace (mirrors Hermes).
    if let Some(bytes) = capture_schema_sidecar(install) {
        entries.push((
            FileEntry {
                path: brain_db::SCHEMA_SIDECAR.to_string(),
                size: bytes.len() as u64,
            },
            RawContent::Inline(bytes),
        ));
    }

    // Root-level workspace files — `.alfignore` applies.
    for name in ROOT_FILES {
        let path = workspace.join(name);
        if !path.is_file() {
            continue;
        }
        if is_alfignored(&matcher, name) {
            excluded += 1;
            warnings.push(format!(".alfignore excludes the structural file {name}"));
            continue;
        }
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        entries.push((
            FileEntry {
                path: name.to_string(),
                size,
            },
            RawContent::Disk(path),
        ));
    }

    // identity.json (AIEOS) — may be absolute / outside the workspace;
    // unfilterable.
    if let Some(ref aieos_path) = config.aieos_path {
        let path = if Path::new(aieos_path).is_absolute() {
            Path::new(aieos_path).to_path_buf()
        } else {
            workspace.join(aieos_path)
        };
        if path.is_file() {
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            entries.push((
                FileEntry {
                    path: "identity.json".to_string(),
                    size,
                },
                RawContent::Disk(path),
            ));
        }
    }

    // memory/ directory — collected sorted; `.alfignore` applies.
    let memory_dir = workspace.join("memory");
    if memory_dir.is_dir() {
        let mut walked: Vec<(String, PathBuf)> = WalkDir::new(&memory_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| {
                let rel = e
                    .path()
                    .strip_prefix(workspace)
                    .unwrap_or(e.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                (rel, e.path().to_path_buf())
            })
            .collect();
        walked.sort();
        for (rel, abs) in walked {
            if is_alfignored(&matcher, &rel) {
                excluded += 1;
                continue;
            }
            let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            entries.push((FileEntry { path: rel, size }, RawContent::Disk(abs)));
        }
    }

    // Agent-managed include list — arbitrary files the agent opted into via
    // `alf add` (runtime-agnostic; shared with OpenClaw via alf_core::include).
    // ALF never auto-discovers; only explicitly-tracked files are added (raw
    // only — no semantic parse). A malformed list degrades to empty with a
    // warning rather than blocking the backup (same posture as `.alfignore`).
    let mut seen: HashSet<String> = entries.iter().map(|(fe, _)| fe.path.clone()).collect();
    let include = match alf_core::include::IncludeList::load(workspace) {
        Ok(list) => list,
        Err(err) => {
            warnings.push(format!(
                "{} could not be read ({err}); tracked files not synced this run",
                alf_core::include::INCLUDE_FILE
            ));
            alf_core::include::IncludeList::default()
        }
    };
    for rel in include.paths() {
        if seen.contains(&rel) {
            continue; // already captured (e.g. a ROOT_FILE or memory/ file)
        }
        let abs = workspace.join(&rel);
        if !abs.is_file() {
            missing_includes.push(rel);
            continue;
        }
        // A4.2: re-validate that a (possibly restored/edited) stored entry still
        // resolves inside the workspace before packing it.
        if let Err(e) = alf_core::include::safe_include_path(workspace, &rel) {
            warnings.push(format!("ignoring tracked path {rel}: {e}"));
            continue;
        }
        if is_alfignored(&matcher, &rel) {
            excluded += 1;
            continue;
        }
        let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
        entries.push((
            FileEntry {
                path: rel.clone(),
                size,
            },
            RawContent::Disk(abs),
        ));
        seen.insert(rel);
    }

    // The include list and sync log themselves travel as raw so the agent's
    // sync config and removal history persist across machines on restore.
    for sentinel in [
        alf_core::include::INCLUDE_FILE,
        alf_core::include::SYNC_LOG_FILE,
    ] {
        if seen.contains(sentinel) {
            continue;
        }
        let abs = workspace.join(sentinel);
        if abs.is_file() {
            let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            entries.push((
                FileEntry {
                    path: sentinel.to_string(),
                    size,
                },
                RawContent::Disk(abs),
            ));
            seen.insert(sentinel.to_string());
        }
    }

    (entries, excluded, warnings, missing_includes)
}

/// Enumerate the workspace files an export would preserve as raw sources.
///
/// The single source of truth for the export file list — both the real
/// `export` and `export --dry-run` go through it.
pub fn enumerate(workspace: &Path) -> Result<EnumerationResult> {
    let zc_home = zeroclaw_home(workspace);
    let config = load_config(&zc_home)?;
    let (entries, excluded, warnings, missing_includes) = enumerate_raw(&zc_home, &config);
    Ok(EnumerationResult {
        files: entries.into_iter().map(|(fe, _)| fe).collect(),
        excluded_by_alfignore: excluded,
        alfignore_warnings: warnings,
        missing_includes,
    })
}

/// Build the `export --dry-run` preview: the enumerated file list plus the
/// agent name and memory-record count.
///
/// Strictly read-only — writes no archive and, unlike a real export, never
/// persists `.alf-agent-id`.
pub fn enumerate_workspace(workspace: &Path) -> Result<WorkspaceEnumeration> {
    // A per-agent binding workspace (`<root>/agents/<alias>/workspace`) may not
    // exist on disk — for ZeroClaw's shared store the data lives at the install
    // root, which is what must exist. Validate that, not the per-agent leaf.
    let zc_home = zeroclaw_home(workspace);
    if !zc_home.is_dir() {
        bail!(
            "ZeroClaw install root does not exist: {}",
            zc_home.display()
        );
    }

    let config = load_config(&zc_home)?;
    let (entries, excluded, mut warnings, missing_includes) = enumerate_raw(&zc_home, &config);
    let files: Vec<FileEntry> = entries.into_iter().map(|(fe, _)| fe).collect();
    let total_size = files.iter().map(|f| f.size).sum();

    let agent_id = resolve_agent_id_readonly(&zc_home)?;
    let detected_name = identity_parser::detect_agent_name(&zc_home, &config);
    let runtime_version = detect_zeroclaw_version(&zc_home);
    let target = slice_target_for(workspace, &zc_home);
    let slice = read_memory_slice(
        &zc_home,
        &config,
        agent_id,
        &target,
        runtime_version.as_deref(),
    )?;
    // Match the real export path: the per-agent alias is the unique name a sync
    // would register (see the WP6 note in `export`), so a dry-run preview shows it.
    let agent_name = slice.provenance.alias.clone().unwrap_or(detected_name);
    warnings.extend(slice.warnings);

    // Surface (but do not prune — this is read-only) tracked files that have
    // gone missing, so a dry-run preview shows what `alf sync` would drop.
    for rel in &missing_includes {
        warnings.push(format!(
            "tracked file {rel} no longer exists (will be removed from sync on next `alf sync`)"
        ));
    }

    Ok(WorkspaceEnumeration {
        agent_name,
        memory_records: slice.records.len() as u64,
        files,
        excluded_by_alfignore: excluded,
        total_size,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Partition helpers
// ---------------------------------------------------------------------------

fn quarter_start(year: i32, quarter: u32) -> NaiveDate {
    let month = (quarter - 1) * 3 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).unwrap()
}

fn quarter_end(year: i32, quarter: u32) -> NaiveDate {
    let end_month = quarter * 3;
    let last_day = match end_month {
        3 => 31,
        6 => 30,
        9 => 30,
        12 => 31,
        _ => 30,
    };
    NaiveDate::from_ymd_opt(year, end_month, last_day).unwrap()
}

// ---------------------------------------------------------------------------
// Agent vault (Layer 4)
// ---------------------------------------------------------------------------

/// Best-effort home directory (honors `ALF_HOME`).
fn dirs_home() -> Option<std::path::PathBuf> {
    alf_core::home_dir()
}

/// Load the agent-managed ALF vault — the `CredentialsDocument` the agent
/// builds explicitly with `alf vault add`.
///
/// This is the ONLY source of the archive's Layer 4. ALF deliberately does
/// not capture a runtime's own keystore (e.g. ZeroClaw `config.toml`
/// `[secrets]`): the agent chooses what to back up. Vault records are already
/// AEAD-encrypted, so they enter the archive verbatim. Returns `None` when the
/// vault file is missing, unreadable, or has no records.
fn load_agent_vault(vault_path: Option<&Path>) -> Result<Option<alf_core::CredentialsDocument>> {
    let path = match vault_path {
        Some(p) if p.is_file() => p,
        _ => return Ok(None),
    };
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    match serde_json::from_str::<alf_core::CredentialsDocument>(&content) {
        Ok(doc) if doc.credentials.is_empty() => Ok(None),
        Ok(doc) => Ok(Some(doc)),
        Err(_) => Ok(None), // graceful degradation
    }
}

// ---------------------------------------------------------------------------
// Config + memory helpers (shared by export and dry-run enumeration)
// ---------------------------------------------------------------------------

/// Parse `config.toml` from the ZeroClaw home, falling back to defaults with
/// heuristic backend detection when no config file exists.
fn load_config(zc_home: &Path) -> Result<ZeroClawConfig> {
    let config_path = zc_home.join("config.toml");
    Ok(
        config_parser::parse_config(&config_path)?.unwrap_or_else(|| {
            let backend = config_parser::detect_backend_heuristic(zc_home);
            ZeroClawConfig {
                memory_backend: backend,
                auto_save: true,
                embedding_provider: "none".into(),
                vector_weight: 0.7,
                keyword_weight: 0.3,
                identity_format: config_parser::IdentityFormat::OpenClaw,
                aieos_path: None,
                aieos_inline: None,
                secrets_encrypt: true,
                credential_hints: Vec::new(),
                raw_toml: String::new(),
            }
        }),
    )
}

/// Which agent's slice of the shared `brain.db` to export.
enum SliceTarget {
    /// The bound agent (from the mapping/binding): alias + optional runtime id.
    Bound { alias: String, id: Option<String> },
    /// Adhoc `-w` export: resolve the sole `agents` row; error on ambiguity.
    Sole,
}

/// ZeroClaw agent provenance stamped into `manifest.agent.extra` so restore can
/// resolve the target agent (by alias) and preserve the archived id.
#[derive(Default)]
struct SliceProvenance {
    zeroclaw_agent_id: Option<String>,
    alias: Option<String>,
}

/// An exported memory slice: the ALF records plus the source-agent provenance.
struct MemorySlice {
    records: Vec<alf_core::MemoryRecord>,
    provenance: SliceProvenance,
    warnings: Vec<String>,
}

/// Read one agent's memory slice for the configured backend. For the SQLite
/// backend this is a per-`agent_id` slice of the shared `brain.db` (WAL
/// copy-read); markdown backends fall back to the markdown collector.
fn read_memory_slice(
    install: &Path,
    config: &ZeroClawConfig,
    alf_agent_id: Uuid,
    target: &SliceTarget,
    runtime_version: Option<&str>,
) -> Result<MemorySlice> {
    let brain_db = match config.memory_backend {
        MemoryBackend::Sqlite => resolve_brain_db(install),
        _ => None,
    };

    let Some(brain_db) = brain_db else {
        // No brain.db (markdown backend, or SQLite configured but store not
        // materialized yet). Fall back to the markdown collector for markdown/
        // sqlite; None/unsupported produce nothing.
        let records = match config.memory_backend {
            MemoryBackend::Sqlite | MemoryBackend::Markdown => {
                markdown_parser::collect_markdown_memory(install, alf_agent_id, runtime_version)?
            }
            MemoryBackend::None | MemoryBackend::Unsupported => Vec::new(),
        };
        return Ok(MemorySlice {
            records,
            provenance: SliceProvenance::default(),
            warnings: Vec::new(),
        });
    };

    let copy = brain_db::open_readonly_copy(&brain_db)?;
    let agents = brain_db::read_agents(&copy.conn)?;

    // Resolve (alias, zeroclaw agent id).
    let (alias, zc_id): (Option<String>, Option<String>) = match target {
        SliceTarget::Bound { alias, id } => {
            let id = id.clone().or_else(|| {
                agents
                    .iter()
                    .find(|(_, a)| a == alias)
                    .map(|(i, _)| i.clone())
            });
            (Some(alias.clone()), id)
        }
        SliceTarget::Sole => match agents.as_slice() {
            [] => (None, None),
            [(id, alias)] => (Some(alias.clone()), Some(id.clone())),
            many => {
                let names: Vec<String> = many.iter().map(|(_, a)| a.clone()).collect();
                anyhow::bail!(
                    "brain.db holds {} agents ({}); pass --agent <alias> to export one",
                    many.len(),
                    names.join(", ")
                );
            }
        },
    };

    let mut warnings = Vec::new();
    let records = match &zc_id {
        Some(id) => sqlite_extractor::records_from_conn(
            &copy.conn,
            config,
            alf_agent_id,
            id,
            runtime_version,
        )?,
        None => {
            if let Some(a) = &alias {
                warnings.push(format!(
                    "agent '{a}' has no rows in brain.db yet — exporting an empty memory slice"
                ));
            }
            Vec::new()
        }
    };

    Ok(MemorySlice {
        records,
        provenance: SliceProvenance {
            zeroclaw_agent_id: zc_id,
            alias,
        },
        warnings,
    })
}

/// Infer the slice target from a workspace path. A per-agent binding workspace
/// is `<root>/agents/<alias>/workspace`, so dry-run (which has no binding) can
/// still scope to that agent; anything else resolves the sole agent.
fn slice_target_for(workspace: &Path, install: &Path) -> SliceTarget {
    if workspace.file_name().and_then(|n| n.to_str()) == Some("workspace") {
        if let Some(alias_dir) = workspace.parent() {
            let under_agents = alias_dir.parent() == Some(install.join("agents").as_path());
            if let (true, Some(alias)) =
                (under_agents, alias_dir.file_name().and_then(|n| n.to_str()))
            {
                return SliceTarget::Bound {
                    alias: alias.to_string(),
                    id: None,
                };
            }
        }
    }
    SliceTarget::Sole
}

/// Serialize the `brain.db` schema sidecar (`.alf-brain-db-schema.json`) when a
/// store exists. Restore replays this DDL to bootstrap a lazily-absent store.
fn capture_schema_sidecar(install: &Path) -> Option<Vec<u8>> {
    let db = resolve_brain_db(install)?;
    let copy = brain_db::open_readonly_copy(&db).ok()?;
    let schema = brain_db::capture_schema(&copy.conn).ok()?;
    serde_json::to_vec_pretty(&schema).ok()
}

// ---------------------------------------------------------------------------
// Export entry point
// ---------------------------------------------------------------------------

/// Export a ZeroClaw workspace to an `.alf` archive (adhoc `-w` / the sole
/// agent). Resolves the install root, then delegates to [`export_impl`] with the
/// sole-agent slice target.
///
/// Layer 4 (credentials) is the agent's explicit ALF vault — see
/// [`load_agent_vault`]. Its records are already AEAD-encrypted; export never
/// touches a vault key.
pub fn export(workspace: &Path, output: &Path) -> Result<ExportReport> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }
    let install = zeroclaw_home(workspace);
    let alf_agent_id = resolve_agent_id(&install)?;
    export_impl(&install, output, alf_agent_id, SliceTarget::Sole)
}

/// Export one agent's slice of a shared install (WP0 `export_agent` seam).
///
/// The mapping's `alf_agent_id` is authoritative and becomes the archive
/// identity; the ZeroClaw slice is filtered by `binding.runtime_agent_id`.
/// Writes the id through to the per-agent workspace pin (fail-closed on drift,
/// like the default seam), then delegates to [`export_impl`].
pub fn export_agent(
    binding: &AgentBinding,
    alf_agent_id: Uuid,
    output: &Path,
) -> Result<ExportReport> {
    let _ = fs::create_dir_all(&binding.workspace);
    alf_core::ensure_workspace_agent_id(&binding.workspace, alf_agent_id)?;
    let install = zeroclaw_home(&binding.workspace);
    let target = SliceTarget::Bound {
        alias: binding.runtime_agent.clone(),
        id: binding.runtime_agent_id.clone(),
    };
    export_impl(&install, output, alf_agent_id, target)
}

/// Discover the agents in a ZeroClaw install (WP3 override of the WP0 single-
/// agent fallback).
///
/// Enumerates the union of the shared `brain.db` `agents` table and the declared
/// `[agents.<alias>]` config blocks. Each agent maps to a per-agent workspace
/// (for its `.alf-agent-id` pin) over the shared `SharedDb` memory source
/// (`agent_id` filter). `default_enabled` follows the Z3 rule: declared agents
/// on; the system `default` off when declared agents exist, but a bare install's
/// sole agent (even `default`) is enabled. Read-only — never writes the install.
pub fn discover_agents(install: &Path) -> Result<Vec<AgentBinding>> {
    let zc_home = zeroclaw_home(install);
    let config = load_config(&zc_home)?;
    let declared = config_parser::discovered_config_agents(&config.raw_toml);
    let declared_set: HashSet<&str> = declared.iter().map(|s| s.as_str()).collect();

    let resolved_db = resolve_brain_db(&zc_home);
    let db_agents = match &resolved_db {
        Some(db) => brain_db::read_agents_from_path(db).unwrap_or_default(),
        None => Vec::new(),
    };
    // The canonical shared-store path even when the store is lazily absent.
    let db_path = resolved_db.unwrap_or_else(|| brain_db_path(&zc_home));

    // Union of aliases: brain.db agents (oldest first) then declared-only ones.
    let mut id_by_alias: HashMap<String, String> = HashMap::new();
    let mut aliases: Vec<String> = Vec::new();
    for (id, alias) in &db_agents {
        id_by_alias
            .entry(alias.clone())
            .or_insert_with(|| id.clone());
        aliases.push(alias.clone());
    }
    for alias in &declared {
        aliases.push(alias.clone());
    }
    let mut seen = HashSet::new();
    aliases.retain(|a| seen.insert(a.clone()));

    let has_declared = !declared.is_empty();
    let sole = aliases.len() == 1;

    let mut bindings: Vec<AgentBinding> = aliases
        .into_iter()
        .map(|alias| {
            let is_declared = declared_set.contains(alias.as_str());
            // Z3 rule (design §10): declared agents on; the system `default` is
            // off when declared agents exist, but a bare install's sole agent —
            // even `default` — is the user's agent and is enabled. A DB-only
            // non-default agent is enabled only when it is the sole agent.
            let default_enabled = if is_declared {
                true
            } else if alias == "default" {
                !has_declared || sole
            } else {
                sole
            };
            AgentBinding {
                runtime_agent_id: id_by_alias.get(&alias).cloned(),
                workspace: zc_home.join("agents").join(&alias).join("workspace"),
                memory_source: MemorySource::SharedDb {
                    path: db_path.clone(),
                    filter_key: "agent_id".to_string(),
                },
                default_enabled,
                runtime_agent: alias,
            }
        })
        .collect();

    // Never return empty: a fresh install (no config agents, no DB) still maps
    // one `default` agent — the WP0 zero-friction promise.
    if bindings.is_empty() {
        bindings.push(AgentBinding {
            runtime_agent: "default".to_string(),
            runtime_agent_id: None,
            workspace: zc_home.join("agents").join("default").join("workspace"),
            memory_source: MemorySource::SharedDb {
                path: db_path,
                filter_key: "agent_id".to_string(),
            },
            default_enabled: true,
        });
    }
    Ok(bindings)
}

/// Shared export core. `install` is the resolved install root (config.toml,
/// `data/memory/brain.db`, identity files, `memory/` all live directly under
/// it); `alf_agent_id` stamps the archive identity; `target` selects the memory
/// slice.
fn export_impl(
    install: &Path,
    output: &Path,
    alf_agent_id: Uuid,
    target: SliceTarget,
) -> Result<ExportReport> {
    if !install.is_dir() {
        bail!("Workspace directory does not exist: {}", install.display());
    }
    let agent_id = alf_agent_id;

    // 1. Parse config
    let config = load_config(install)?;

    // 2. Name + version
    let detected_name = identity_parser::detect_agent_name(install, &config);
    let runtime_version = detect_zeroclaw_version(install);

    // 3. Extract the agent's memory slice (per-agent for the shared brain.db)
    let slice = read_memory_slice(
        install,
        &config,
        agent_id,
        &target,
        runtime_version.as_deref(),
    )?;
    let export_warnings = slice.warnings;
    let provenance = slice.provenance;
    // ALF agent names must be unique per tenant (service `agents_tenant_name_unique`).
    // Every agent in the shared brain.db exports from the same install root, so the
    // detected name (SOUL.md H1 / install dir) is IDENTICAL across agents and a second
    // agent's registration 409s. Use the per-agent alias as the name — it is the
    // agent's own identity in the brain.db and is unique per install (WP6).
    let agent_name = provenance.alias.clone().unwrap_or(detected_name);
    let records = slice.records;
    let total_records = records.len() as u64;

    // Check for embeddings in the record set
    let has_embeddings = records.iter().any(|r| !r.embeddings.is_empty());

    // 4. Group records into partitions
    let mut partition_groups: BTreeMap<String, Vec<alf_core::MemoryRecord>> = BTreeMap::new();
    for record in records {
        let label = PartitionAssigner::partition_for_record(&record);
        partition_groups.entry(label).or_default().push(record);
    }

    let mut partition_infos: Vec<(MemoryPartitionInfo, Vec<alf_core::MemoryRecord>)> = Vec::new();
    for (file_path, group_records) in &partition_groups {
        let label = file_path
            .trim_start_matches("memory/")
            .trim_end_matches(".jsonl");
        let parts: Vec<&str> = label.split("-Q").collect();
        let (from, to) = if parts.len() == 2 {
            let year: i32 = parts[0].parse().unwrap_or(2026);
            let quarter: u32 = parts[1].parse().unwrap_or(1);
            (
                quarter_start(year, quarter),
                Some(quarter_end(year, quarter)),
            )
        } else {
            (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), None)
        };

        let info = MemoryPartitionInfo {
            file: file_path.clone(),
            from,
            to,
            record_count: group_records.len() as u64,
            sealed: false,
            extra: std::collections::HashMap::new(),
        };
        partition_infos.push((info, group_records.clone()));
    }

    // 5. Build other layers
    let identity = identity_parser::parse_identity(install, &config, agent_id)?;
    let principals = principals_parser::parse_principals(install, agent_id)?;

    // Layer 4 = the agent's explicit ALF vault ONLY. ALF never captures a
    // runtime's own keystore (e.g. ZeroClaw `config.toml [secrets]`) — the
    // agent chooses what to back up via `alf vault add`. Vault records are
    // already AEAD-encrypted, so they enter the archive verbatim. Per-agent
    // path (WP1): the CLI migrates any legacy install-scoped vault before
    // export, so there is deliberately no legacy fallback here.
    let vault_path = dirs_home().map(|h| alf_core::agent_vault_path(&h, agent_id));
    let credentials = load_agent_vault(vault_path.as_deref())?;

    let has_identity = identity.is_some();
    let identity_version = identity.as_ref().map(|i| i.version);
    let principals_count = principals
        .as_ref()
        .map(|p| p.principals.len() as u32)
        .unwrap_or(0);
    let credentials_count = credentials
        .as_ref()
        .map(|c| c.credentials.len() as u32)
        .unwrap_or(0);

    // ZeroClaw slice provenance → manifest.agent.extra. Restore reads
    // `zeroclaw_alias` to resolve the target agent and prefers the archived
    // `zeroclaw_agent_id` when creating a fresh row.
    let mut agent_extra = std::collections::HashMap::new();
    if let Some(id) = &provenance.zeroclaw_agent_id {
        agent_extra.insert("zeroclaw_agent_id".to_string(), serde_json::json!(id));
    }
    if let Some(alias) = &provenance.alias {
        agent_extra.insert("zeroclaw_alias".to_string(), serde_json::json!(alias));
    }
    if let Some(sv) = detect_config_schema_version(install) {
        agent_extra.insert("schema_version".to_string(), serde_json::json!(sv));
    }

    // 6. Build manifest
    let manifest = Manifest {
        alf_version: "1.0.0".to_string(),
        created_at: Utc::now(),
        agent: AgentMetadata {
            id: agent_id,
            name: agent_name.clone(),
            source_runtime: "zeroclaw".to_string(),
            source_runtime_version: runtime_version,
            extra: agent_extra,
        },
        layers: LayerInventory {
            identity: if has_identity {
                Some(IdentityLayerInfo {
                    version: identity_version.unwrap_or(1),
                    file: "identity/identity.json".to_string(),
                    extra: std::collections::HashMap::new(),
                })
            } else {
                None
            },
            principals: if principals_count > 0 {
                Some(PrincipalsLayerInfo {
                    count: principals_count,
                    file: "principals/principals.json".to_string(),
                    extra: std::collections::HashMap::new(),
                })
            } else {
                None
            },
            credentials: if credentials_count > 0 {
                Some(CredentialsLayerInfo {
                    count: credentials_count,
                    file: "credentials/credentials.json".to_string(),
                    extra: std::collections::HashMap::new(),
                })
            } else {
                None
            },
            memory: Some(MemoryInventory {
                record_count: total_records,
                index_file: "memory/index.json".to_string(),
                partitions: partition_infos
                    .iter()
                    .map(|(info, _)| info.clone())
                    .collect(),
                has_embeddings: Some(has_embeddings),
                has_raw_source: Some(true),
                extra: std::collections::HashMap::new(),
            }),
            attachments: None,
            extra: std::collections::HashMap::new(),
        },
        runtime_hints: None,
        sync: None,
        raw_sources: vec!["zeroclaw".to_string()],
        checksum: None,
        extra: std::collections::HashMap::new(),
    };

    // 7. Write archive
    let file = File::create(output)
        .with_context(|| format!("Failed to create output file: {}", output.display()))?;
    let writer = BufWriter::new(file);
    let mut alf_writer = AlfWriter::new(writer, manifest)?;

    if let Some(ref id) = identity {
        alf_writer.set_identity(id)?;
    }
    if let Some(ref p) = principals {
        alf_writer.set_principals(p)?;
    }
    if let Some(ref c) = credentials {
        alf_writer.set_credentials(c)?;
    }

    for (info, group_records) in &partition_infos {
        alf_writer.add_memory_partition(info.clone(), group_records)?;
    }

    // Raw sources — `enumerate_raw` is the single source of truth for the set.
    let (raw_entries, excluded_by_alfignore, _warnings, missing_includes) =
        enumerate_raw(install, &config);
    let mut raw_source_names = Vec::with_capacity(raw_entries.len());
    for (entry, content) in raw_entries {
        let data = match content {
            RawContent::Inline(bytes) => bytes,
            RawContent::Disk(path) => fs::read(&path)
                .with_context(|| format!("Failed to read raw source {}", path.display()))?,
        };
        alf_writer.add_raw_source("zeroclaw", &entry.path, &data)?;
        raw_source_names.push(entry.path);
    }

    let inner = alf_writer.finish()?;
    drop(inner);

    let output_size = fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(ExportReport {
        agent_name,
        alf_version: "1.0.0".to_string(),
        memory_records: total_records,
        identity_version,
        principals_count,
        credentials_count,
        attachments_count: 0,
        raw_sources: raw_source_names,
        output_path: output.to_string_lossy().to_string(),
        output_size_bytes: output_size,
        excluded_by_alfignore,
        missing_includes,
        warnings: export_warnings,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain_db;
    use std::fs;
    use tempfile::TempDir;

    /// Create a flat ZeroClaw install root (the real/harness layout): everything
    /// — `config.toml`, `SOUL.md`, `memory/*.md` — lives directly under the root.
    fn create_zeroclaw_home(
        config_toml: &str,
        files: &[(&str, &str)],
    ) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("config.toml"), config_toml).unwrap();
        for (name, content) in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
        }
        (dir, root)
    }

    const A: &str = "aaaaaaaa-0000-0000-0000-0000000000a1";

    /// Seed a real-schema `brain.db` at `<root>/data/memory/brain.db` with one
    /// agent (`agent_a`) and two rows.
    fn create_test_db(root: &Path) {
        use rusqlite::Connection;
        let db = brain_db::real_schema_db(&root.join("data").join("memory"), &[(A, "agent_a")]);
        let conn = Connection::open(&db).unwrap();
        for (id, key, content, cat, ts) in [
            (
                "a1b2c3d4-0000-0000-0000-000000000001",
                "user_pref",
                "User prefers Rust over Go",
                "core",
                "2026-01-15T10:00:00Z",
            ),
            (
                "a1b2c3d4-0000-0000-0000-000000000002",
                "daily_log",
                "Reviewed migration plan",
                "episodic",
                "2026-02-20T14:00:00Z",
            ),
        ] {
            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, \
                 updated_at, session_id, namespace, importance, superseded_by, agent_id) \
                 VALUES (?1,?2,?3,?4,NULL,?5,?5,NULL,'default',0.5,NULL,?6)",
                rusqlite::params![id, key, content, cat, ts, A],
            )
            .unwrap();
        }
    }

    #[test]
    fn extract_semver_variants() {
        assert_eq!(extract_semver("zeroclaw 0.8.2").as_deref(), Some("0.8.2"));
        assert_eq!(
            extract_semver("zeroclaw v0.8.2 (build 9)").as_deref(),
            Some("0.8.2")
        );
        assert_eq!(extract_semver("1.2.3.4").as_deref(), Some("1.2.3"));
        assert_eq!(extract_semver("no digits here"), None);
        assert_eq!(extract_semver("v0.8"), None);
    }

    #[test]
    fn config_schema_version_present_and_absent() {
        let (_d, root) =
            create_zeroclaw_home("schema_version = 3\n[memory]\nbackend = \"sqlite\"\n", &[]);
        assert_eq!(detect_config_schema_version(&root), Some(3));
        let (_d2, root2) = create_zeroclaw_home("[memory]\nbackend = \"sqlite\"\n", &[]);
        assert_eq!(detect_config_schema_version(&root2), None);
    }

    #[test]
    fn detect_version_prefers_config_then_binary() {
        // An explicit config `version` wins (offline, deterministic).
        let (_d, root) = create_zeroclaw_home("version = \"9.9.9\"\nschema_version = 3\n", &[]);
        assert_eq!(detect_zeroclaw_version(&root).as_deref(), Some("9.9.9"));
        // No config version → binary fallback (absent in tests → None, or a real
        // semver if installed); must not panic.
        let (_d2, root2) = create_zeroclaw_home("schema_version = 3\n", &[]);
        let _ = detect_zeroclaw_version(&root2);
    }

    #[test]
    fn export_sqlite_workspace() {
        let config = r#"
[memory]
backend = "sqlite"
embedding_provider = "none"

[identity]
format = "openclaw"
"#;
        let (dir, root) = create_zeroclaw_home(
            config,
            &[("SOUL.md", "# ZCAgent\n\nA test ZeroClaw agent.\n")],
        );
        create_test_db(&root);

        let output = dir.path().join("test.alf");
        let report = export(&root, &output).unwrap();

        // The agent NAME is the per-agent alias (unique per tenant), not the
        // shared-install SOUL.md H1 "ZCAgent" — WP6 fix for the sync --all 409.
        assert_eq!(report.agent_name, "agent_a");
        assert_eq!(report.memory_records, 2);
        assert!(report.identity_version.is_some());
        assert!(output.is_file());
        assert!(report.output_size_bytes > 0);
        assert!(report.raw_sources.contains(&"config.toml".to_string()));
        // The DDL sidecar rides along for lazy-store restore.
        assert!(report
            .raw_sources
            .contains(&brain_db::SCHEMA_SIDECAR.to_string()));
    }

    #[test]
    fn export_markdown_workspace() {
        let config = r#"
[memory]
backend = "markdown"
"#;
        let (dir, root) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# MdAgent\n\nMarkdown backend.\n"),
                (
                    "memory/2026-02-15.md",
                    "## Morning\n\nDid stuff.\n\n## Evening\n\nMore stuff.\n",
                ),
            ],
        );

        let output = dir.path().join("test.alf");
        let report = export(&root, &output).unwrap();

        assert_eq!(report.agent_name, "MdAgent");
        assert_eq!(report.memory_records, 2);
    }

    #[test]
    fn two_agents_export_distinct_names() {
        // Regression guard for the sync --all 409: two agents in one shared
        // brain.db must export DISTINCT names (their aliases), or the second's
        // registration violates the tenant's unique-name constraint.
        use rusqlite::Connection;
        const B: &str = "bbbbbbbb-0000-0000-0000-0000000000b2";
        let config = "[memory]\nbackend = \"sqlite\"\nembedding_provider = \"none\"\n\
                      [agents.agent_a]\n[agents.agent_b]\n";
        // A shared SOUL.md → detect_agent_name is identical for both; only the
        // alias distinguishes them.
        let (dir, root) = create_zeroclaw_home(config, &[("SOUL.md", "# SharedSoul\n")]);
        let db = brain_db::real_schema_db(
            &root.join("data").join("memory"),
            &[(A, "agent_a"), (B, "agent_b")],
        );
        let conn = Connection::open(&db).unwrap();
        for (id, key, aid) in [
            ("11111111-0000-0000-0000-000000000001", "k_a", A),
            ("22222222-0000-0000-0000-000000000002", "k_b", B),
        ] {
            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, \
                 updated_at, session_id, namespace, importance, superseded_by, agent_id) \
                 VALUES (?1,?2,'c','core',NULL,'2026-01-15T10:00:00Z','2026-01-15T10:00:00Z',\
                 NULL,'default',0.5,NULL,?3)",
                rusqlite::params![id, key, aid],
            )
            .unwrap();
        }

        let export_alias = |alias: &str, zid: &str| {
            let binding = AgentBinding {
                runtime_agent: alias.to_string(),
                runtime_agent_id: Some(zid.to_string()),
                workspace: root.join("agents").join(alias).join("workspace"),
                memory_source: MemorySource::SharedDb {
                    path: db.clone(),
                    filter_key: "agent_id".to_string(),
                },
                default_enabled: true,
            };
            let out = dir.path().join(format!("{alias}.alf"));
            export_agent(&binding, Uuid::parse_str(zid).unwrap(), &out).unwrap()
        };

        let ra = export_alias("agent_a", A);
        let rb = export_alias("agent_b", B);
        assert_eq!(ra.agent_name, "agent_a");
        assert_eq!(rb.agent_name, "agent_b");
        assert_ne!(ra.agent_name, rb.agent_name);
    }

    #[test]
    fn agent_id_stability() {
        let config = "[memory]\nbackend = \"sqlite\"";
        let (dir, root) = create_zeroclaw_home(config, &[("SOUL.md", "# Stable\n\nTest.\n")]);
        create_test_db(&root);

        let out1 = dir.path().join("out1.alf");
        let out2 = dir.path().join("out2.alf");
        export(&root, &out1).unwrap();
        export(&root, &out2).unwrap();

        assert!(root.join(".alf-agent-id").is_file());
    }

    #[test]
    fn export_nonexistent_workspace() {
        let result = export(Path::new("/nonexistent"), Path::new("/tmp/out.alf"));
        assert!(result.is_err());
    }

    #[test]
    fn load_agent_vault_reads_and_handles_missing() {
        use alf_core::{CredentialRecord, CredentialType, CredentialsDocument, EncryptionMetadata};
        use std::collections::HashMap;

        let dir = TempDir::new().unwrap();
        assert!(load_agent_vault(None).unwrap().is_none());
        assert!(load_agent_vault(Some(&dir.path().join("nope.json")))
            .unwrap()
            .is_none());

        let path = dir.path().join("credentials.json");
        let doc = CredentialsDocument {
            credentials: vec![CredentialRecord {
                id: Uuid::nil(),
                agent_id: Uuid::nil(),
                service: "telegram".into(),
                credential_type: CredentialType::Account,
                encrypted_payload: "ZmFrZQ==".into(),
                encryption: EncryptionMetadata {
                    algorithm: "xchacha20-poly1305".into(),
                    nonce: "bm9uY2U=".into(),
                    kdf: None,
                    kdf_params: None,
                    extra: HashMap::new(),
                },
                created_at: Utc::now(),
                label: Some("mybot".into()),
                description: None,
                capabilities_granted: vec![],
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags: vec!["alf-vault".into()],
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        };
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        let loaded = load_agent_vault(Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.credentials.len(), 1);
        assert_eq!(loaded.credentials[0].service, "telegram");
    }

    // --- .alfignore enumeration (ZeroClaw parity) ----------------------------

    /// `.alfignore` filters workspace files but never the synthesized
    /// `config.toml`, which is not a workspace path.
    #[test]
    fn enumerate_alfignore_filters_workspace_not_config() {
        let config = "[memory]\nbackend = \"markdown\"\n";
        let (_dir, ws) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# Agent\n\nHello.\n"),
                ("memory/2026-02-15.md", "## A\n\nlog\n"),
                ("memory/2026-02-16.md", "## B\n\nlog\n"),
                (".alfignore", "memory/2026-02-15.md\n"),
            ],
        );

        let result = enumerate(&ws).unwrap();
        let paths: Vec<String> = result.files.iter().map(|f| f.path.clone()).collect();

        // config.toml is synthesized and unfilterable — always present.
        assert!(paths.contains(&"config.toml".to_string()));
        assert!(paths.contains(&"SOUL.md".to_string()));
        // The .alfignore'd memory file is gone; the other remains.
        assert!(!paths.contains(&"memory/2026-02-15.md".to_string()));
        assert!(paths.contains(&"memory/2026-02-16.md".to_string()));
        assert_eq!(result.excluded_by_alfignore, 1);
    }

    /// `enumerate` is the single source of truth: its file list equals the
    /// real export's `raw/zeroclaw/` entries.
    #[test]
    fn enumerate_matches_export_raw_sources() {
        let config = "[memory]\nbackend = \"markdown\"\n";
        let (dir, ws) = create_zeroclaw_home(
            config,
            &[
                ("SOUL.md", "# Agent\n\nHello.\n"),
                ("memory/2026-02-15.md", "## A\n\nlog\n"),
            ],
        );

        let enumerated: std::collections::BTreeSet<String> = enumerate(&ws)
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();

        let output = dir.path().join("out.alf");
        let report = export(&ws, &output).unwrap();
        let exported: std::collections::BTreeSet<String> = report.raw_sources.into_iter().collect();

        assert_eq!(enumerated, exported);
    }
}
