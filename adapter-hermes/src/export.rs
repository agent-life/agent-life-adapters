//! Export a Hermes profile (`HERMES_HOME`) to an `.alf` archive.
//!
//! The `workspace` argument IS the Hermes home (e.g. `~/.hermes`). Orchestrates:
//! parse `config.yaml` → collect curated + session records → build
//! identity/principals/credentials → write archive + an allowlisted `raw/hermes/`
//! tree. Raw preservation is an **allowlist** (D7): only durable text/config is
//! included, so `.env`, `state.db`, and admin dirs are excluded by construction.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::adapter::{AgentBinding, MemorySource};
use alf_core::{
    AgentMetadata, AlfWriter, CredentialsLayerInfo, FileEntry, IdentityLayerInfo, LayerInventory,
    Manifest, MemoryInventory, MemoryPartitionInfo, PartitionAssigner, PrincipalsLayerInfo,
    WorkspaceEnumeration,
};

use crate::config_parser::{self, HermesConfig};
use crate::curated_parser;
use crate::identity_parser;
use crate::principals_parser;
use crate::session_extractor;
use crate::ExportReport;

const RUNTIME: &str = "hermes";

/// UUID v5 namespace for deriving agent IDs from profile paths (bytes spell
/// `alf-hrms-agent-1`).
const AGENT_ID_NS: Uuid = Uuid::from_bytes([
    0x61, 0x6c, 0x66, 0x2d, 0x68, 0x72, 0x6d, 0x73, // "alf-hrms"
    0x2d, 0x61, 0x67, 0x65, 0x6e, 0x74, 0x2d, 0x31, // "-agent-1"
]);

/// Single root file preserved at the home top level. (`USER.md`/`MEMORY.md` live
/// under `memories/`.)
const ROOT_FILES: &[&str] = &["SOUL.md"];

/// Subdirectories of the home preserved verbatim under `raw/hermes/`.
const RAW_DIRS: &[&str] = &["memories", "skill-bundles", "cron"];

/// The schema sidecar's archive-relative path under `raw/hermes/`.
const SCHEMA_SIDECAR: &str = ".alf-state-db-schema.json";

// ---------------------------------------------------------------------------
// Agent discovery (WP5 multi-agent)
// ---------------------------------------------------------------------------

/// Discover the Hermes profiles in an install (WP5 override of the WP0 single-
/// agent fallback).
///
/// Hermes is profile-isolated: each agent is a Hermes *profile* with its own
/// `state.db` (session-keyed, no agent column) + `memories/*.md`. Two shapes:
///
/// - The **default profile** is `install` (`~/.hermes`) itself, interleaved with
///   the shared runtime (`node/`, `bin/`, `hermes-agent/`, caches). Its binding
///   workspace is `install`; [`export`]'s allowlist excludes the runtime *and*
///   the nested `profiles/` by construction (`ROOT_FILES` + `RAW_DIRS` only), so
///   the default-profile archive carries agent data only.
/// - **Named profiles** live at `install/profiles/<name>/` and are clean
///   (agent data only).
///
/// Each profile becomes one [`MemorySource::PerAgentDb`] binding at
/// `<profile>/state.db` — a descriptor, not an existence guarantee: the DB is
/// created lazily on first session run, so the path need not exist. Every
/// profile is a real, user-configured agent, so `default_enabled` is true
/// (design §10: Hermes `default` is on, unlike ZeroClaw's vestigial `default`).
/// The `default` binding is always present, so the result is never empty —
/// it is itself the single-agent fallback. Read-only; never writes the install.
pub fn discover_agents(install: &Path) -> Result<Vec<AgentBinding>> {
    let mut bindings = vec![profile_binding("default", install)];
    let profiles_dir = install.join("profiles");
    if let Ok(entries) = fs::read_dir(&profiles_dir) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            let dir = profiles_dir.join(&name);
            bindings.push(profile_binding(&name, &dir));
        }
    }
    Ok(bindings)
}

/// One `PerAgentDb` binding for the Hermes profile rooted at `dir`.
fn profile_binding(name: &str, dir: &Path) -> AgentBinding {
    AgentBinding {
        runtime_agent: name.to_string(),
        runtime_agent_id: None,
        workspace: dir.to_path_buf(),
        memory_source: MemorySource::PerAgentDb {
            path: dir.join("state.db"),
        },
        default_enabled: true,
    }
}

// ---------------------------------------------------------------------------
// Agent ID persistence
// ---------------------------------------------------------------------------

pub(crate) fn resolve_agent_id_readonly(home: &Path) -> Result<Uuid> {
    let id_file = home.join(".alf-agent-id");
    if id_file.is_file() {
        let raw = fs::read_to_string(&id_file).context("Failed to read .alf-agent-id")?;
        return Uuid::parse_str(raw.trim()).context("Invalid UUID in .alf-agent-id");
    }
    let canonical = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    Ok(Uuid::new_v5(
        &AGENT_ID_NS,
        canonical.to_string_lossy().as_bytes(),
    ))
}

fn resolve_agent_id(home: &Path) -> Result<Uuid> {
    let id = resolve_agent_id_readonly(home)?;
    let id_file = home.join(".alf-agent-id");
    if !id_file.is_file() {
        let _ = fs::write(&id_file, id.to_string());
    }
    Ok(id)
}

// ---------------------------------------------------------------------------
// Raw source collection
// ---------------------------------------------------------------------------

/// The set of files an export would archive, before contents are read.
pub struct EnumerationResult {
    pub files: Vec<FileEntry>,
    pub excluded_by_alfignore: u32,
    pub alfignore_warnings: Vec<String>,
    pub missing_includes: Vec<String>,
}

enum RawContent {
    Disk(PathBuf),
    Inline(Vec<u8>),
}

type RawEnumeration = (Vec<(FileEntry, RawContent)>, u32, Vec<String>, Vec<String>);

fn load_alfignore(home: &Path) -> (Gitignore, Vec<String>) {
    let path = home.join(".alfignore");
    if !path.is_file() {
        return (Gitignore::empty(), Vec::new());
    }
    let mut warnings = Vec::new();
    let mut builder = GitignoreBuilder::new(home);
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

fn is_alfignored(matcher: &Gitignore, rel: &str) -> bool {
    matcher
        .matched_path_or_any_parents(rel, /* is_dir = */ false)
        .is_ignore()
}

/// Enumerate every raw-source entry alongside the location of its bytes.
///
/// Allowlist by design (D7): `config.yaml` (redacted) and the `state.db` schema
/// sidecar are synthesized & unfilterable; `SOUL.md`, `memories/**`,
/// `skill-bundles/**`, `cron/**` and the agent's `alf add` list are filterable
/// by `.alfignore`. `.env`, `state.db`, `checkpoints/`, `state-snapshots/`,
/// `backups/`, `logs/`, `sessions/`, and `profiles/` are never walked.
fn enumerate_raw(home: &Path, config: &HermesConfig) -> RawEnumeration {
    let (matcher, mut warnings) = load_alfignore(home);
    let mut entries: Vec<(FileEntry, RawContent)> = Vec::new();
    let mut excluded: u32 = 0;
    let mut missing_includes: Vec<String> = Vec::new();

    // config.yaml — synthesized & redacted; unfilterable.
    let redacted = config_parser::redact_secrets(&config.raw_yaml).into_bytes();
    entries.push((
        FileEntry {
            path: "config.yaml".to_string(),
            size: redacted.len() as u64,
        },
        RawContent::Inline(redacted),
    ));

    // state.db schema sidecar — synthesized; unfilterable. Replayed on rebuild.
    if let Some(bytes) = capture_schema_bytes(home) {
        entries.push((
            FileEntry {
                path: SCHEMA_SIDECAR.to_string(),
                size: bytes.len() as u64,
            },
            RawContent::Inline(bytes),
        ));
    }

    // Root files (just SOUL.md) — `.alfignore` applies.
    for name in ROOT_FILES {
        let path = home.join(name);
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

    // Allowlisted directories, walked sorted — `.alfignore` applies.
    for dir in RAW_DIRS {
        let root = home.join(dir);
        if !root.is_dir() {
            continue;
        }
        let mut walked: Vec<(String, PathBuf)> = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| {
                let rel = e
                    .path()
                    .strip_prefix(home)
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

    // Agent-managed include list (`alf add`) — runtime-agnostic; raw only.
    let mut seen: HashSet<String> = entries.iter().map(|(fe, _)| fe.path.clone()).collect();
    let include = match alf_core::include::IncludeList::load(home) {
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
            continue;
        }
        let abs = home.join(&rel);
        if !abs.is_file() {
            missing_includes.push(rel);
            continue;
        }
        // A4.2: re-validate that a (possibly restored/edited) stored entry still
        // resolves inside the home before packing it.
        if let Err(e) = alf_core::include::safe_include_path(home, &rel) {
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

    // External entries (D3): re-validated at export (the two-time TOCTOU guard
    // — denylist, allowed-root, symlink-resolve), packed under
    // `raw/hermes/external/<sanitized>` with bytes from the absolute source.
    // Inert (unverified) and failing entries are surfaced as warnings, not packed.
    let roots = alf_core::include::load_allowed_roots();
    let (externals, ext_skipped) = alf_core::include::external_entries_for_export(&include, &roots);
    for (archive_rel, source_canon) in externals {
        if seen.contains(&archive_rel) {
            continue;
        }
        let size = fs::metadata(&source_canon).map(|m| m.len()).unwrap_or(0);
        entries.push((
            FileEntry {
                path: archive_rel.clone(),
                size,
            },
            RawContent::Disk(source_canon),
        ));
        seen.insert(archive_rel);
    }
    warnings.extend(ext_skipped);

    // Include list + sync log themselves travel as raw.
    for sentinel in [
        alf_core::include::INCLUDE_FILE,
        alf_core::include::SYNC_LOG_FILE,
    ] {
        if seen.contains(sentinel) {
            continue;
        }
        let abs = home.join(sentinel);
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

/// Capture the `state.db` schema as pretty JSON bytes (the sidecar payload), or
/// `None` when there is no `state.db`. Deterministic, so export and dry-run
/// produce identical sidecar bytes.
fn capture_schema_bytes(home: &Path) -> Option<Vec<u8>> {
    let db = home.join("state.db");
    if !db.is_file() {
        return None;
    }
    let schema = session_extractor::capture_state_schema(&db).ok()?;
    serde_json::to_vec_pretty(&schema).ok()
}

/// The single source of truth for the export file list.
pub fn enumerate(home: &Path) -> Result<EnumerationResult> {
    let config = load_config(home)?;
    let (entries, excluded, warnings, missing_includes) = enumerate_raw(home, &config);
    Ok(EnumerationResult {
        files: entries.into_iter().map(|(fe, _)| fe).collect(),
        excluded_by_alfignore: excluded,
        alfignore_warnings: warnings,
        missing_includes,
    })
}

/// `export --dry-run` preview: the enumerated file list + agent name + record count.
pub fn enumerate_workspace(home: &Path) -> Result<WorkspaceEnumeration> {
    if !home.is_dir() {
        bail!("Hermes home directory does not exist: {}", home.display());
    }
    let config = load_config(home)?;
    let (entries, excluded, mut warnings, missing_includes) = enumerate_raw(home, &config);
    let files: Vec<FileEntry> = entries.into_iter().map(|(fe, _)| fe).collect();
    let total_size = files.iter().map(|f| f.size).sum();

    let agent_id = resolve_agent_id_readonly(home)?;
    let agent_name = identity_parser::detect_agent_name(home, &config);
    let records = collect_records(home, agent_id, None)?;

    for rel in &missing_includes {
        warnings.push(format!(
            "tracked file {rel} no longer exists (will be removed from sync on next `alf sync`)"
        ));
    }

    Ok(WorkspaceEnumeration {
        agent_name,
        memory_records: records.len() as u64,
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
        3 | 12 => 31,
        _ => 30,
    };
    NaiveDate::from_ymd_opt(year, end_month, last_day).unwrap()
}

// ---------------------------------------------------------------------------
// Config + record helpers (shared by export and dry-run)
// ---------------------------------------------------------------------------

fn load_config(home: &Path) -> Result<HermesConfig> {
    Ok(config_parser::parse_config(&home.join("config.yaml"))?.unwrap_or_default())
}

/// Collect the full record set: curated memory + sessions (when `state.db`
/// exists). Sessions are always included from first init (D1).
fn collect_records(
    home: &Path,
    agent_id: Uuid,
    runtime_version: Option<&str>,
) -> Result<Vec<alf_core::MemoryRecord>> {
    let mut records = curated_parser::collect_curated_memory(home, agent_id, runtime_version)?;
    let db = home.join("state.db");
    if db.is_file() {
        let export = session_extractor::extract_sessions(&db, agent_id, runtime_version)?;
        records.extend(export.records);
    }
    Ok(records)
}

/// D4: detect API keys in `~/.hermes/.env` that aren't covered by the ALF vault
/// and return a one-line "not backed up" advisory (or `None`). We never copy
/// `.env` into the archive; this only points the user at `alf vault add`.
fn detect_unvaulted_env(
    home: &Path,
    vault: Option<&alf_core::CredentialsDocument>,
) -> Option<String> {
    let content = fs::read_to_string(home.join(".env")).ok()?;
    let keys = parse_env_keys(&content);
    if keys.is_empty() {
        return None;
    }
    let vaulted: HashSet<String> = vault
        .map(|d| {
            d.credentials
                .iter()
                .flat_map(|c| {
                    let mut v = vec![c.service.to_lowercase()];
                    if let Some(l) = &c.label {
                        v.push(l.to_lowercase());
                    }
                    v
                })
                .collect()
        })
        .unwrap_or_default();
    let unvaulted: Vec<&String> = keys
        .iter()
        .filter(|k| !vaulted.contains(&k.to_lowercase()))
        .collect();
    if unvaulted.is_empty() {
        return None;
    }
    let preview: Vec<&str> = unvaulted.iter().take(5).map(|s| s.as_str()).collect();
    Some(format!(
        "{} key(s) in ~/.hermes/.env are not backed up in the ALF vault ({}{}). \
         Vault them with `alf vault add --secret-file …` so they travel with this agent; \
         ALF never copies plaintext .env into the archive.",
        unvaulted.len(),
        preview.join(", "),
        if unvaulted.len() > preview.len() {
            ", …"
        } else {
            ""
        },
    ))
}

/// Extract assignment keys from a `.env` file (skip blanks/comments; strip a
/// leading `export `).
fn parse_env_keys(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                return None;
            }
            let t = t.strip_prefix("export ").unwrap_or(t);
            let key = t.split('=').next()?.trim();
            (!key.is_empty()).then(|| key.to_string())
        })
        .collect()
}

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
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Runtime version detection
// ---------------------------------------------------------------------------

/// First `<digits>.<digits>.<digits>` run in `text`, if any. Hand-rolled to
/// avoid a regex dependency; mirrors the harness kit's `\d+\.\d+\.\d+` scan.
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

/// Best-effort Hermes version via `hermes --version` (e.g.
/// `"Hermes Agent v0.17.0 (2026.6.19) ..."` → `"0.17.0"`). `None` when the
/// binary is absent or unparseable — Hermes has no version field in
/// `config.yaml` or `state.db`, so the CLI is the only source. Host unit tests
/// (no `hermes` on PATH) correctly yield `None`; live runs execute `alf` inside
/// the container where `hermes` resolves.
fn detect_hermes_version() -> Option<String> {
    let output = std::process::Command::new("hermes")
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

/// Best-effort `state.db` `schema_version` (16 on a real install), or `None`
/// when there is no `state.db` (lazy install) or no version row.
fn detect_state_schema_version(home: &Path) -> Option<i64> {
    let db = home.join("state.db");
    if !db.is_file() {
        return None;
    }
    let sv = session_extractor::capture_state_schema(&db)
        .ok()?
        .schema_version;
    (sv >= 0).then_some(sv)
}

// ---------------------------------------------------------------------------
// Export entry point
// ---------------------------------------------------------------------------

/// Export a Hermes profile to an `.alf` archive.
pub fn export(home: &Path, output: &Path) -> Result<ExportReport> {
    if !home.is_dir() {
        bail!("Hermes home directory does not exist: {}", home.display());
    }

    let config = load_config(home)?;
    let agent_id = resolve_agent_id(home)?;
    let agent_name = identity_parser::detect_agent_name(home, &config);
    let runtime_version = detect_hermes_version();
    let schema_version = detect_state_schema_version(home);

    // Records: curated memory + sessions.
    let records = collect_records(home, agent_id, runtime_version.as_deref())?;
    let total_records = records.len() as u64;
    let has_embeddings = records.iter().any(|r| !r.embeddings.is_empty());

    // Partition by quarter.
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
        partition_infos.push((
            MemoryPartitionInfo {
                file: file_path.clone(),
                from,
                to,
                record_count: group_records.len() as u64,
                sealed: false,
                extra: std::collections::HashMap::new(),
            },
            group_records.clone(),
        ));
    }

    // Other layers. Per-agent vault path (WP1): the CLI migrates any legacy
    // install-scoped vault before export — no legacy fallback here.
    let identity = identity_parser::parse_identity(home, &config, agent_id)?;
    let principals = principals_parser::parse_principals(home, agent_id)?;
    let vault_path = alf_core::home_dir().map(|h| alf_core::agent_vault_path(&h, agent_id));
    let credentials = load_agent_vault(vault_path.as_deref())?;

    // D4: warn about plaintext `.env` keys not covered by the vault.
    let mut warnings = Vec::new();
    if let Some(msg) = detect_unvaulted_env(home, credentials.as_ref()) {
        warnings.push(msg);
    }

    // D5: non-bundled skills as artifacts (the first real use of the tier).
    let skill_export = crate::skills::collect_skill_artifacts(home)?;
    let attachments_layer = skill_export.as_ref().map(|se| {
        let total = se.index.attachments.len() as u32;
        let included: u64 = se
            .tier2
            .iter()
            .filter_map(|a| fs::metadata(&a.source_path).ok())
            .map(|m| m.len())
            .sum();
        alf_core::AttachmentsLayerInfo {
            count: total,
            file: "attachments.json".to_string(),
            included_count: Some(se.included_count),
            included_size_bytes: Some(included),
            referenced_count: Some(se.referenced_count),
            referenced_size_bytes: None,
            extra: std::collections::HashMap::new(),
        }
    });
    let attachments_count = attachments_layer.as_ref().map(|a| a.count).unwrap_or(0);

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

    let manifest = Manifest {
        alf_version: "1.0.0".to_string(),
        created_at: Utc::now(),
        agent: AgentMetadata {
            id: agent_id,
            name: agent_name.clone(),
            source_runtime: RUNTIME.to_string(),
            source_runtime_version: runtime_version,
            extra: schema_version
                .map(|sv| {
                    std::collections::HashMap::from([(
                        "schema_version".to_string(),
                        serde_json::json!(sv),
                    )])
                })
                .unwrap_or_default(),
        },
        layers: LayerInventory {
            identity: has_identity.then(|| IdentityLayerInfo {
                version: identity_version.unwrap_or(1),
                file: "identity/identity.json".to_string(),
                extra: std::collections::HashMap::new(),
            }),
            principals: (principals_count > 0).then(|| PrincipalsLayerInfo {
                count: principals_count,
                file: "principals/principals.json".to_string(),
                extra: std::collections::HashMap::new(),
            }),
            credentials: (credentials_count > 0).then(|| CredentialsLayerInfo {
                count: credentials_count,
                file: "credentials/credentials.json".to_string(),
                extra: std::collections::HashMap::new(),
            }),
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
            attachments: attachments_layer,
            extra: std::collections::HashMap::new(),
        },
        runtime_hints: None,
        sync: None,
        raw_sources: vec![RUNTIME.to_string()],
        checksum: None,
        extra: std::collections::HashMap::new(),
    };

    // Write archive.
    let file = File::create(output)
        .with_context(|| format!("Failed to create output file: {}", output.display()))?;
    let mut alf_writer = AlfWriter::new(BufWriter::new(file), manifest)?;

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

    // Skill artifacts (D5): the index, then the Tier-2 file bytes.
    if let Some(ref se) = skill_export {
        alf_writer.set_attachments(&se.index)?;
        for art in &se.tier2 {
            let data = fs::read(&art.source_path).with_context(|| {
                format!(
                    "Failed to read skill artifact {}",
                    art.source_path.display()
                )
            })?;
            alf_writer.add_artifact(&art.archive_path, &data)?;
        }
    }

    // Raw sources — `enumerate_raw` is the single source of truth.
    let (raw_entries, excluded_by_alfignore, _warnings, missing_includes) =
        enumerate_raw(home, &config);
    let mut raw_source_names = Vec::with_capacity(raw_entries.len());
    for (entry, content) in raw_entries {
        let data = match content {
            RawContent::Inline(bytes) => bytes,
            RawContent::Disk(path) => fs::read(&path)
                .with_context(|| format!("Failed to read raw source {}", path.display()))?,
        };
        alf_writer.add_raw_source(RUNTIME, &entry.path, &data)?;
        raw_source_names.push(entry.path);
    }

    alf_writer.finish()?;
    let output_size = fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(ExportReport {
        agent_name,
        alf_version: "1.0.0".to_string(),
        memory_records: total_records,
        identity_version,
        principals_count,
        credentials_count,
        attachments_count,
        raw_sources: raw_source_names,
        output_path: output.to_string_lossy().to_string(),
        output_size_bytes: output_size,
        excluded_by_alfignore,
        missing_includes,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    /// Build a Hermes home with SOUL.md, memories, config.yaml, optional state.db.
    fn make_home(with_db: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        fs::write(
            home.join("SOUL.md"),
            "# Atlas\n\nA steadfast Hermes agent.\n",
        )
        .unwrap();
        fs::write(
            home.join("config.yaml"),
            "agent:\n  system_prompt: \"Be terse.\"\n",
        )
        .unwrap();
        let mem = home.join("memories");
        fs::create_dir_all(&mem).unwrap();
        fs::write(
            mem.join("MEMORY.md"),
            "User prefers Rust.\n§\nAlways run fmt.",
        )
        .unwrap();
        fs::write(
            mem.join("USER.md"),
            "# Johan\n\n## Timezone\n\nAfrica/Johannesburg\n",
        )
        .unwrap();
        if with_db {
            let db = home.join("state.db");
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version VALUES (16);
                 CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT NOT NULL, title TEXT,
                     parent_session_id TEXT, started_at REAL NOT NULL, ended_at REAL);
                 CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                     role TEXT NOT NULL, content TEXT, tool_name TEXT, timestamp REAL NOT NULL);
                 CREATE VIRTUAL TABLE messages_fts USING fts5(content);",
            )
            .unwrap();
            c.execute("INSERT INTO sessions (id,source,title,started_at) VALUES ('20260101_120000_aa','cli','S',1767268800.0)", []).unwrap();
            c.execute("INSERT INTO messages (session_id,role,content,timestamp) VALUES ('20260101_120000_aa','user','hi',1767268800.0)", []).unwrap();
        }
        dir
    }

    #[test]
    fn extract_semver_variants() {
        assert_eq!(
            extract_semver("Hermes Agent v0.17.0 (2026.6.19) built abc").as_deref(),
            Some("0.17.0")
        );
        assert_eq!(extract_semver("0.17.0").as_deref(), Some("0.17.0"));
        assert_eq!(extract_semver("1.2.3.4-rc").as_deref(), Some("1.2.3"));
        assert_eq!(extract_semver("no version at all"), None);
        assert_eq!(extract_semver("only v1.2 here"), None);
    }

    #[test]
    fn detect_hermes_version_is_graceful() {
        // Best-effort: must never panic whether or not `hermes` is on PATH.
        let _ = detect_hermes_version();
    }

    #[test]
    fn state_schema_version_present_and_absent() {
        assert_eq!(
            detect_state_schema_version(make_home(true).path()),
            Some(16)
        );
        assert_eq!(detect_state_schema_version(make_home(false).path()), None);
    }

    #[test]
    fn export_curated_only() {
        let dir = make_home(false);
        let out = dir.path().join("out.alf");
        let report = export(dir.path(), &out).unwrap();
        assert_eq!(report.agent_name, "Atlas");
        assert_eq!(report.memory_records, 2); // two §-entries
        assert!(report.identity_version.is_some());
        assert_eq!(report.principals_count, 1);
        assert!(report.raw_sources.contains(&"config.yaml".to_string()));
        assert!(report.raw_sources.contains(&"SOUL.md".to_string()));
        assert!(report
            .raw_sources
            .contains(&"memories/MEMORY.md".to_string()));
        // No state.db → no schema sidecar.
        assert!(!report.raw_sources.iter().any(|s| s == SCHEMA_SIDECAR));
    }

    #[test]
    fn export_with_sessions() {
        let dir = make_home(true);
        let out = dir.path().join("out.alf");
        let report = export(dir.path(), &out).unwrap();
        // 2 curated + 1 session.
        assert_eq!(report.memory_records, 3);
        assert!(report.raw_sources.iter().any(|s| s == SCHEMA_SIDECAR));
        // state.db binary is NEVER archived (D7).
        assert!(!report.raw_sources.iter().any(|s| s == "state.db"));
    }

    #[test]
    fn enumerate_matches_export_raw_sources() {
        let dir = make_home(true);
        let enumerated: std::collections::BTreeSet<String> = enumerate(dir.path())
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();
        let out = dir.path().join("out.alf");
        let exported: std::collections::BTreeSet<String> = export(dir.path(), &out)
            .unwrap()
            .raw_sources
            .into_iter()
            .collect();
        assert_eq!(enumerated, exported);
    }

    #[test]
    fn agent_id_persisted_per_profile() {
        let dir = make_home(false);
        let out = dir.path().join("out.alf");
        export(dir.path(), &out).unwrap();
        assert!(dir.path().join(".alf-agent-id").is_file());
    }

    #[test]
    fn detects_unvaulted_env_keys() {
        let dir = make_home(false);
        fs::write(
            dir.path().join(".env"),
            "# secrets\nOPENAI_API_KEY=sk-x\nexport TELEGRAM_TOKEN=abc\n\n",
        )
        .unwrap();
        let out = dir.path().join("out.alf");
        let report = export(dir.path(), &out).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains(".env") && w.contains("not backed up")),
            "expected a D4 .env advisory, got {:?}",
            report.warnings
        );
        // The keys are named in the advisory.
        assert!(report.warnings.iter().any(|w| w.contains("OPENAI_API_KEY")));
    }

    #[test]
    fn export_skips_poisoned_include_entry() {
        // A4.2: a hand-edited `.alf-include.json` that points outside the home
        // must not cause export to pack the external file.
        let dir = make_home(false);
        let outside = dir.path().parent().unwrap().join("hermes-secret.txt");
        fs::write(&outside, "TOPSECRET").unwrap();
        let poisoned = format!(
            "{{\"files\":[{{\"path\":\"../{}\"}}]}}",
            outside.file_name().unwrap().to_string_lossy()
        );
        fs::write(dir.path().join(".alf-include.json"), poisoned).unwrap();

        let out = dir.path().join("out.alf");
        let report = export(dir.path(), &out).unwrap();
        assert!(
            !report
                .raw_sources
                .iter()
                .any(|s| s.contains("hermes-secret")),
            "poisoned include entry must be skipped, got {:?}",
            report.raw_sources
        );
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn export_excludes_env_and_admin_dirs() {
        let dir = make_home(true);
        fs::write(dir.path().join(".env"), "OPENAI_API_KEY=sk-secret\n").unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::write(dir.path().join("logs").join("run.log"), "noise").unwrap();
        let out = dir.path().join("out.alf");
        let report = export(dir.path(), &out).unwrap();
        assert!(!report.raw_sources.iter().any(|s| s == ".env"));
        assert!(!report.raw_sources.iter().any(|s| s.starts_with("logs/")));
    }

    #[test]
    fn discover_agents_enumerates_default_and_named_profiles() {
        let dir = make_home(false);
        let home = dir.path();
        // A named profile with a lazy (absent) state.db.
        let prof = home.join("profiles").join("agent_a");
        fs::create_dir_all(prof.join("memories")).unwrap();
        fs::write(prof.join("SOUL.md"), "# Agent A\n").unwrap();

        let bindings = discover_agents(home).unwrap();
        assert_eq!(bindings.len(), 2);

        let default = &bindings[0];
        assert_eq!(default.runtime_agent, "default");
        assert_eq!(default.workspace.as_path(), home);
        assert!(default.default_enabled);
        assert!(default.runtime_agent_id.is_none());
        assert_eq!(
            default.memory_source,
            MemorySource::PerAgentDb {
                path: home.join("state.db")
            }
        );

        let named = &bindings[1];
        assert_eq!(named.runtime_agent, "agent_a");
        assert_eq!(named.workspace, prof);
        assert_eq!(
            named.memory_source,
            MemorySource::PerAgentDb {
                path: prof.join("state.db")
            }
        );
        // Lazy tolerance: the profile binds even though state.db does not exist.
        assert!(!prof.join("state.db").exists());
    }

    #[test]
    fn discover_agents_default_only_when_no_profiles_dir() {
        let dir = make_home(false);
        let bindings = discover_agents(dir.path()).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].runtime_agent, "default");
        assert_eq!(bindings[0].workspace.as_path(), dir.path());
    }

    #[test]
    fn default_profile_export_excludes_runtime_and_nested_profiles() {
        // The default profile's workspace is `~/.hermes` itself, interleaved
        // with the shared runtime. Exporting it must carry agent data only —
        // no runtime dirs, no nested named profiles, no state.db binary.
        let dir = make_home(true);
        let home = dir.path();
        for d in ["node", "bin", "hermes-agent"] {
            fs::create_dir_all(home.join(d)).unwrap();
            fs::write(home.join(d).join("junk.txt"), "runtime").unwrap();
        }
        let nested = home.join("profiles").join("agent_a").join("memories");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("MEMORY.md"), "OTHER PROFILE DATA").unwrap();
        // The per-agent vault KEY lives under `~/.hermes/state/<id>/` (WP5). It
        // must NEVER travel in an archive — `state/` is not in the allowlist.
        let key_dir = home
            .join("state")
            .join("11111111-1111-1111-1111-111111111111");
        fs::create_dir_all(&key_dir).unwrap();
        fs::write(key_dir.join(".alf-vault-key"), "TOPSECRETKEYBYTES").unwrap();

        let out = home.join("out.alf");
        let report = export(home, &out).unwrap();

        for leaked in [
            "node",
            "bin",
            "hermes-agent",
            "profiles",
            "state.db",
            "state/",
        ] {
            assert!(
                !report.raw_sources.iter().any(|s| s.starts_with(leaked)),
                "default-profile export leaked {leaked:?}: {:?}",
                report.raw_sources
            );
        }
        // Sanity: it DID carry the agent data + the schema sidecar.
        assert!(report.raw_sources.iter().any(|s| s == "SOUL.md"));
        assert!(report.raw_sources.iter().any(|s| s == "memories/MEMORY.md"));
        assert!(report.raw_sources.iter().any(|s| s == SCHEMA_SIDECAR));
    }
}
