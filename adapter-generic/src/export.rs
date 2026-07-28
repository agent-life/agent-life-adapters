//! Export a generic (map-driven) workspace to an `.alf` archive.
//!
//! The shape mirrors `adapter-openclaw`'s export, but every "which file maps to
//! what" decision comes from `.alf-map.json` instead of a hardcoded handler
//! table. Records are emitted with full dashboard-parity provenance; the raw
//! tree preserves every source file verbatim under `raw/generic/` so a
//! same-runtime restore is byte-lossless.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::chunk::{split_markdown_sections, ChunkingStrategy, MarkdownSection};
use alf_core::include::{IncludeList, INCLUDE_FILE, SYNC_LOG_FILE};
use alf_core::{
    agent_vault_path, home_dir, AgentMetadata, AlfWriter, CredentialsDocument,
    CredentialsLayerInfo, ExtractionMethod, Identity, IdentityLayerInfo, LayerInventory, Manifest,
    MemoryInventory, MemoryPartitionInfo, MemoryRecord, MemoryStatus, MemoryType, Names,
    PartitionAssigner, ProseIdentity, SourceProvenance, StructuredIdentity, TemporalMetadata,
    AGENT_ID_FILE,
};

use crate::map::{MemoryMap, MemorySourceSpec, MAP_FILE};
use crate::{ExportReport, FileEntry, WorkspaceEnumeration};

pub(crate) const RUNTIME: &str = "generic";

/// ALF's own control/metadata files. These never become memory records even
/// when a broad glob (`**`) matches them (S3) — the `.alf-agent-id` pin, the map
/// itself, and the include/ignore/sync sentinels are ALF machinery, not the
/// agent's memory. (The map + include/sync sentinels still travel *raw* via
/// their dedicated passes so a restore carries the extraction rules and sync
/// config.) A dotfile a map *deliberately* globs that is not on this list is the
/// author's choice and is treated normally.
const CONTROL_FILES: &[&str] = &[
    MAP_FILE,
    INCLUDE_FILE,
    SYNC_LOG_FILE,
    AGENT_ID_FILE,
    ".alfignore",
    ".alf-include.lock",
];

/// UUID v5 namespace for content-addressed generic record ids.
///
/// **One-way door.** Every generic agent's birth ids derive from this constant;
/// changing it re-mints every record id in every existing generic archive. The
/// bytes spell `generic-alf-ns01`. Do not change.
const GENERIC_NS: Uuid = Uuid::from_bytes(*b"generic-alf-ns01");

/// UUID v5 namespace for deriving a stable agent id from a workspace path when
/// no `.alf-agent-id` file is present. Distinct from [`GENERIC_NS`] so the two
/// derivations never collide. Bytes spell `generic-agent-id`.
const GENERIC_AGENT_ID_NS: Uuid = Uuid::from_bytes(*b"generic-agent-id");

// ---------------------------------------------------------------------------
// Agent id
// ---------------------------------------------------------------------------

/// Resolve the agent UUID without writing anything: `{workspace}/.alf-agent-id`
/// if present, else a deterministic UUID v5 of the canonical workspace path.
/// Backs `Adapter::resolve_agent_id`.
pub fn resolve_agent_id_readonly(workspace: &Path) -> Result<Uuid> {
    let id_file = workspace.join(AGENT_ID_FILE);
    if id_file.is_file() {
        let raw = fs::read_to_string(&id_file).context("reading .alf-agent-id")?;
        return Uuid::parse_str(raw.trim()).context("invalid UUID in .alf-agent-id");
    }
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    Ok(Uuid::new_v5(
        &GENERIC_AGENT_ID_NS,
        canonical.to_string_lossy().as_bytes(),
    ))
}

/// Resolve and persist the agent id (writes `.alf-agent-id` when absent).
fn resolve_agent_id(workspace: &Path) -> Result<Uuid> {
    let id = resolve_agent_id_readonly(workspace)?;
    let id_file = workspace.join(AGENT_ID_FILE);
    if !id_file.is_file() {
        let _ = fs::write(&id_file, id.to_string());
    }
    Ok(id)
}

// ---------------------------------------------------------------------------
// Workspace walk + `.alfignore`
// ---------------------------------------------------------------------------

/// Load `<workspace>/.alfignore` into a gitignore matcher (C1). A missing file
/// yields an empty matcher; a malformed one yields an empty matcher + a warning,
/// so a broken ignore file can never silently drop files or block the backup —
/// the same posture as every other adapter.
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

fn is_alfignored(matcher: &Gitignore, rel: &str) -> bool {
    matcher
        .matched_path_or_any_parents(rel, /* is_dir = */ false)
        .is_ignore()
}

/// Every regular workspace file as `(relative_path, absolute_path)`, sorted for
/// deterministic ordering. **Symlinks are skipped** (S2): `follow_links(false)`
/// only stops directory descent, but `is_file()`/`read` still resolve a
/// symlinked *file* to its target, so a `knowledge/leak.md -> ~/.ssh/id_rsa`
/// would otherwise be ingested. `.git/` internals are always skipped.
fn workspace_files(workspace: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = WalkDir::new(workspace)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| !e.path_is_symlink() && e.file_type().is_file())
        .filter_map(|e| {
            let rel = e
                .path()
                .strip_prefix(workspace)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, e.path().to_path_buf()))
        })
        .filter(|(rel, _)| !rel.starts_with(".git/") && rel != ".git")
        .collect();
    files.sort();
    files
}

/// Resolve a map-declared workspace-relative file to a safe absolute path
/// inside the workspace (S1 belt-and-suspenders for a hand-built `MemoryMap`
/// that bypassed `map::validate`). Rejects absolute/`..` paths (string) and
/// symlink-or-otherwise-escaping targets (canonicalize + containment). `Ok(None)`
/// means the path is safe but the file simply does not exist.
fn safe_map_file(workspace: &Path, rel: &str) -> Result<Option<PathBuf>> {
    crate::map::reject_unsafe_relpath(rel)?;
    let abs = workspace.join(rel);
    if !abs.exists() {
        return Ok(None);
    }
    let ws_canon = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let canon = abs
        .canonicalize()
        .with_context(|| format!("resolving map file {rel}"))?;
    if !canon.starts_with(&ws_canon) {
        bail!("{rel} resolves outside the workspace (symlink or `..` escape)");
    }
    if !canon.is_file() {
        return Ok(None);
    }
    Ok(Some(canon))
}

// ---------------------------------------------------------------------------
// File → records
// ---------------------------------------------------------------------------

pub(crate) fn parse_memory_type(s: &str) -> MemoryType {
    // Every string deserializes: known variants map through, the rest become
    // `MemoryType::Unknown(s)` (validation already flagged non-canonical types).
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .unwrap_or_else(|_| MemoryType::Unknown(s.to_string()))
}

/// Chunk one file into records per its source spec.
fn parse_source_file(
    src: &MemorySourceSpec,
    relative_path: &str,
    content: &str,
    file_mtime: DateTime<Utc>,
    agent_id: Uuid,
) -> Vec<MemoryRecord> {
    let sections = match src.chunking.strategy() {
        ChunkingStrategy::OneRecordPerFile => {
            if content.trim().is_empty() {
                return Vec::new();
            }
            vec![MarkdownSection {
                heading: None,
                content: content.to_string(),
                line_start: 1,
                line_end: content.lines().count().max(1),
            }]
        }
        ChunkingStrategy::SplitByHeading { fence_aware, level } => {
            split_markdown_sections(content, level, fence_aware)
        }
        // The map exposes only per_file / by_heading; these vocabulary-only
        // strategies are never produced by `ChunkingMode::strategy`.
        ChunkingStrategy::VaultEntries | ChunkingStrategy::FileListingOnly => return Vec::new(),
    };

    let memory_type = parse_memory_type(&src.memory_type);

    // Birth ids are a pure function of the file: occurrence counts duplicate
    // contents over the FULL ordered section list, trailing-trim keyed to match
    // `alf_core::ids::memory_record_id`.
    let ids: Vec<Uuid> = {
        let mut occurrences: HashMap<&str, u32> = HashMap::new();
        sections
            .iter()
            .map(|section| {
                let occ = occurrences
                    .entry(section.content.trim_end())
                    .and_modify(|c| *c += 1)
                    .or_insert(0);
                alf_core::ids::memory_record_id(
                    &GENERIC_NS,
                    agent_id,
                    relative_path,
                    &section.content,
                    *occ,
                )
            })
            .collect()
    };

    // Timestamps (design §8/§9): a resolvable date → midnight UTC as both
    // created_at and observed_at; otherwise created_at = mtime, observed_at
    // absent. updated_at = mtime always.
    let date = timestamp_date(&src.timestamp, relative_path, content);
    let (created_from_date, observed_at) = match date {
        Some(d) => {
            let midnight = d
                .and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("midnight is valid"))
                .and_utc();
            (Some(midnight), Some(midnight))
        }
        None => (None, None),
    };

    sections
        .into_iter()
        .enumerate()
        .map(|(idx, section)| {
            let tags = build_tags(src, &section.content);
            let raw_source_format = serde_json::json!({
                "line_start": section.line_start,
                "line_end": section.line_end,
                "heading": section.heading,
            });
            MemoryRecord {
                id: ids[idx],
                agent_id,
                content: section.content,
                memory_type: memory_type.clone(),
                source: SourceProvenance {
                    runtime: RUNTIME.to_string(),
                    runtime_version: None,
                    origin: Some("workspace".to_string()),
                    origin_file: Some(relative_path.to_string()),
                    extraction_method: Some(ExtractionMethod::AgentWritten),
                    session_id: None,
                    interaction_id: None,
                    identity_version: None,
                    extra: HashMap::new(),
                },
                temporal: TemporalMetadata {
                    created_at: created_from_date.unwrap_or(file_mtime),
                    updated_at: Some(file_mtime),
                    observed_at,
                    valid_from: None,
                    valid_until: None,
                    last_accessed_at: None,
                    access_count: None,
                    extra: HashMap::new(),
                },
                status: MemoryStatus::Active,
                namespace: src.namespace.clone(),
                category: None,
                supersedes: None,
                confidence: None,
                entities: Vec::new(),
                tags,
                embeddings: Vec::new(),
                related_records: Vec::new(),
                raw_source_format: Some(raw_source_format),
                extra: HashMap::new(),
            }
        })
        .collect()
}

/// Assemble a record's tags: the namespace is always tag 0 (dashboard grouping),
/// then each directive appends, deduplicated in directive order.
pub(crate) fn build_tags(src: &MemorySourceSpec, content: &str) -> Vec<String> {
    let mut tags = vec![src.namespace.clone()];
    let push = |tags: &mut Vec<String>, t: String| {
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    };
    for directive in &src.tags {
        if directive == "hashtags" {
            for line in content.lines() {
                for word in line.split_whitespace() {
                    if word.starts_with('#') && word.len() > 1 {
                        let tag = word
                            .trim_start_matches('#')
                            .trim_end_matches(|c: char| !c.is_alphanumeric())
                            .to_string();
                        push(&mut tags, tag);
                    }
                }
            }
        } else if let Some(value) = directive.strip_prefix("static:") {
            push(&mut tags, value.to_string());
        } else if let Some(key) = directive.strip_prefix("frontmatter:") {
            for value in frontmatter_values(content, key) {
                push(&mut tags, value);
            }
        }
        // Unknown directives were rejected at validation.
    }
    tags
}

/// Resolve the record date for a timestamp mode, or `None` (→ mtime behavior).
fn timestamp_date(mode: &str, relative_path: &str, content: &str) -> Option<NaiveDate> {
    if mode == "filename_date" {
        filename_date(relative_path)
    } else if let Some(key) = mode.strip_prefix("frontmatter:") {
        frontmatter_values(content, key)
            .into_iter()
            .find_map(|v| NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d").ok())
    } else {
        None
    }
}

/// Parse a `YYYY-MM-DD` date from a file's basename stem.
fn filename_date(relative_path: &str) -> Option<NaiveDate> {
    let name = Path::new(relative_path).file_name()?.to_str()?;
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// Parse the leading `---`…`---` YAML frontmatter block into a mapping (R5).
/// Uses `serde_yaml` rather than a hand-rolled `split(':')`/`split(',')`, which
/// mis-parsed legal YAML — comments (`date: 2026-01-01 # note`) and quoted
/// commas (`topics: ["a, b", c]`) both corrupted the extracted value.
fn frontmatter_map(content: &str) -> Option<serde_yaml::Mapping> {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }
    let mut block = String::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        block.push_str(line);
        block.push('\n');
    }
    if !closed {
        return None; // no closing fence → not a valid frontmatter block
    }
    serde_yaml::from_str::<serde_yaml::Mapping>(&block).ok()
}

/// Values for a frontmatter `key`: a scalar becomes one item, a sequence its
/// scalar members (non-scalar members dropped).
fn frontmatter_values(content: &str, key: &str) -> Vec<String> {
    let Some(map) = frontmatter_map(content) else {
        return Vec::new();
    };
    match map.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::Sequence(seq)) => {
            seq.iter().filter_map(yaml_scalar_string).collect()
        }
        Some(other) => yaml_scalar_string(other).into_iter().collect(),
        None => Vec::new(),
    }
}

/// A YAML scalar as a string; `None` for maps/sequences/null.
fn yaml_scalar_string(val: &serde_yaml::Value) -> Option<String> {
    match val {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Identity (Layer 1)
// ---------------------------------------------------------------------------

/// Build Layer 1 from the map's optional `identity_file` (minimal name +
/// description parse; the whole file also travels as prose for fidelity). The
/// path is resolved through [`safe_map_file`] so it can never read outside the
/// workspace (S1).
fn build_identity(workspace: &Path, map: &MemoryMap, agent_id: Uuid) -> Result<Option<Identity>> {
    let Some(rel) = &map.identity_file else {
        return Ok(None);
    };
    let Some(path) = safe_map_file(workspace, rel)? else {
        return Ok(None);
    };
    let content = fs::read_to_string(&path).with_context(|| format!("reading identity {rel}"))?;
    if content.trim().is_empty() {
        return Ok(None);
    }

    let (name, description) = parse_identity(&content, workspace);
    Ok(Some(Identity {
        // Deterministic id + mtime so an unchanged identity re-exports
        // identically (no spurious delta every sync).
        id: alf_core::ids::identity_id(agent_id),
        agent_id,
        version: 1,
        updated_at: alf_core::ids::newest_mtime([&path]),
        structured: Some(StructuredIdentity {
            names: Some(Names {
                primary: name,
                nickname: None,
                full: None,
                extra: HashMap::new(),
            }),
            role: description,
            goals: Vec::new(),
            psychology: None,
            linguistics: None,
            capabilities: Vec::new(),
            sub_agents: Vec::new(),
            aieos_extensions: None,
            extra: HashMap::new(),
        }),
        prose: Some(ProseIdentity {
            soul: None,
            operating_instructions: None,
            identity_profile: Some(content),
            custom_blocks: HashMap::new(),
            extra: HashMap::new(),
        }),
        source_format: Some(RUNTIME.to_string()),
        raw_source: None,
        extra: HashMap::new(),
    }))
}

/// Minimal identity parse: `Name:`/`Description:` fields (bullet or plain),
/// falling back to the first H1 for the name and the workspace basename.
fn parse_identity(content: &str, workspace: &Path) -> (String, Option<String>) {
    let mut name = None;
    let mut description = None;
    let mut first_h1 = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if first_h1.is_none() {
            if let Some(h) = trimmed.strip_prefix("# ") {
                first_h1 = Some(h.trim().to_string());
            }
        }
        if name.is_none() {
            name = field_value(trimmed, "name");
        }
        if description.is_none() {
            description = field_value(trimmed, "description");
        }
    }
    let name = name
        .or(first_h1)
        .unwrap_or_else(|| workspace_basename(workspace));
    (name, description)
}

/// Extract the value of a `key:`/`**key:**` field line (bullet-tolerant,
/// case-insensitive on the key), or `None`.
fn field_value(line: &str, key: &str) -> Option<String> {
    let body = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .trim();
    let lower = body.to_ascii_lowercase();
    for prefix in [
        format!("**{key}:**"),
        format!("**{key}**:"),
        format!("{key}:"),
    ] {
        if lower.starts_with(&prefix) {
            let value = body[prefix.len()..].trim().trim_matches('*').trim();
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

fn workspace_basename(workspace: &Path) -> String {
    workspace
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("generic-agent")
        .to_string()
}

/// Display name for the manifest, derived from the already-built [`Identity`]
/// (no second read of the identity file): the parsed identity name, else the
/// map's `framework`, else the workspace basename.
fn agent_display_name(identity: Option<&Identity>, map: &MemoryMap, workspace: &Path) -> String {
    identity
        .and_then(|i| i.structured.as_ref())
        .and_then(|s| s.names.as_ref())
        .map(|n| n.primary.clone())
        .or_else(|| map.framework.clone())
        .unwrap_or_else(|| workspace_basename(workspace))
}

// ---------------------------------------------------------------------------
// Credentials (Layer 4) — agent vault, verbatim
// ---------------------------------------------------------------------------

/// Load the agent's explicit ALF vault (already AEAD-encrypted → verbatim).
/// The write-twin lives in [`crate::import`]. Same posture as OpenClaw: ALF
/// never captures a runtime keystore, only the agent's `alf vault add` records.
fn load_agent_vault(vault_path: Option<&Path>) -> Option<CredentialsDocument> {
    let path = vault_path.filter(|p| p.is_file())?;
    let content = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<CredentialsDocument>(&content) {
        Ok(doc) if doc.credentials.is_empty() => None,
        Ok(doc) => Some(doc),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Collection — one workspace walk feeding both records and the raw tree
// ---------------------------------------------------------------------------

/// Where a raw entry's bytes come from. Memory-source files are read once (for
/// parsing) and their bytes reused here; a `sqlite_rows` source's `.db` and its
/// `-wal`/`-shm` sidecars are captured eagerly and consecutively as bytes so the
/// trio travels as one consistent unit (WP-G.3 — never `VACUUM INTO`, raw
/// fidelity is the contract). Everything else is read lazily at pack time so a
/// large tracked file is never held in memory twice. Every read — eager or
/// lazy — goes through [`read_raw_capped`] (WP-I.1 size caps).
enum RawSource {
    Bytes(Vec<u8>),
    Path(PathBuf),
}

/// Read a raw-tree file with the per-entry size cap enforced up front
/// (WP-I.1): a file over [`alf_core::MAX_RAW_ENTRY_BYTES`] fails the export
/// before its bytes are pulled into memory — a restore would reject the entry
/// anyway. The underlying `io::Error` is preserved in the chain so callers can
/// detect `NotFound` (the WAL-sidecar checkpoint race in [`collect`]).
fn read_raw_capped(path: &Path, rel: &str) -> Result<Vec<u8>> {
    let len = fs::metadata(path)
        .with_context(|| format!("reading raw source {rel}"))?
        .len();
    if len > alf_core::MAX_RAW_ENTRY_BYTES {
        bail!(
            "raw source {rel} is {len} bytes, over the {} byte per-file cap \
             (a restore would reject it)",
            alf_core::MAX_RAW_ENTRY_BYTES
        );
    }
    fs::read(path).with_context(|| format!("reading raw source {rel}"))
}

/// Whether an error chain bottoms out in `io::ErrorKind::NotFound`.
fn is_not_found(err: &anyhow::Error) -> bool {
    err.root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
}

/// `Some(base)` when `rel` carries a SQLite sidecar suffix (`x.db-wal` → `x.db`).
fn sidecar_base(rel: &str) -> Option<&str> {
    rel.strip_suffix("-wal")
        .or_else(|| rel.strip_suffix("-shm"))
}

/// Everything one workspace walk produces: parsed records + the deduplicated raw
/// tree (keyed by the path passed to `add_raw_source`), plus counts/warnings.
struct Collected {
    records: Vec<MemoryRecord>,
    raw: BTreeMap<String, RawSource>,
    excluded_by_alfignore: u32,
    missing_includes: Vec<String>,
    warnings: Vec<String>,
}

/// Walk the workspace once and build both the memory records and the raw tree.
///
/// The raw tree preserves every memory-source file, the map file, the identity
/// file, the include-list tracked files (in-workspace + external), and the
/// include/sync sentinels — packed under `raw/generic/`. The map + identity file
/// are packed even though they are not memory sources: without them a
/// same-runtime restore→re-export would drop Layer 1 / the extraction rules and
/// show a spurious delta (the zero-delta restore contract).
fn collect(workspace: &Path, map: &MemoryMap, agent_id: Uuid) -> Result<Collected> {
    let (matcher, mut warnings) = load_alfignore(workspace);
    let mut excluded: u32 = 0;
    let mut records = Vec::new();
    let mut raw: BTreeMap<String, RawSource> = BTreeMap::new();
    let mut source_hit = vec![false; map.memory_sources.len()];

    // Memory-source files (single walk; symlinks/.git already filtered).
    for (rel, abs) in workspace_files(workspace) {
        // S3: control/metadata files never become records.
        if CONTROL_FILES.contains(&rel.as_str()) {
            continue;
        }
        let Some(idx) = map
            .memory_sources
            .iter()
            .position(|s| alf_core::chunk::path_matches(&s.glob, &rel))
        else {
            continue;
        };
        if is_alfignored(&matcher, &rel) {
            excluded += 1;
            continue;
        }
        source_hit[idx] = true;
        let src = &map.memory_sources[idx];
        let bytes = read_raw_capped(&abs, &rel)?;
        if src.chunking.is_sqlite() {
            // A `-wal`/`-shm` sidecar the SAME source's glob happens to match
            // (`data/*`, `brain.db*` — inevitable once WAL mode creates them)
            // is not a database: capture it raw and move on, never route it
            // through row extraction (opening a WAL file as a database
            // hard-failed the whole export). Only skipped when the matched
            // base `.db` actually exists — a glob naming ONLY a sidecar is a
            // real misconfiguration and keeps the hard-fail (decision 4).
            if let Some(base_rel) = sidecar_base(&rel) {
                if alf_core::chunk::path_matches(&src.glob, base_rel)
                    && workspace.join(base_rel).is_file()
                {
                    raw.entry(rel).or_insert(RawSource::Bytes(bytes));
                    continue;
                }
            }
            // A `sqlite_rows` source: the `.db` bytes were just read; capture
            // its `-wal`/`-shm` sidecars eagerly and CONSECUTIVELY (WP-G.3) so
            // the trio travels as one near-consistent unit, BEFORE row
            // extraction gets a chance to interleave. A sidecar that vanishes
            // mid-capture (a checkpoint race) is skipped; any other read error
            // fails the export. Never `VACUUM INTO` — raw fidelity.
            if let Some(fname) = abs.file_name().and_then(|f| f.to_str()) {
                for suffix in ["-wal", "-shm"] {
                    let side = abs.with_file_name(format!("{fname}{suffix}"));
                    let side_rel = format!("{rel}{suffix}");
                    match read_raw_capped(&side, &side_rel) {
                        Ok(data) => {
                            raw.entry(side_rel).or_insert(RawSource::Bytes(data));
                        }
                        Err(e) if is_not_found(&e) => {} // checkpointed away mid-capture
                        Err(e) => return Err(e),
                    }
                }
            }
            // Extract per-row records via the sqlite reader instead of parsing
            // the `.db` as text. Extraction failure hard-fails the export
            // (decision 4): degrading to zero records would compute a delta
            // that mass-deletes the agent's cloud history. The temp-then-rename
            // archive write leaves any previous good archive untouched.
            match crate::sqlite::extract_rows(
                &abs,
                &rel,
                src,
                &GENERIC_NS,
                agent_id,
                file_mtime(&abs),
            ) {
                Ok((recs, warns)) => {
                    records.extend(recs);
                    warnings.extend(warns);
                }
                Err(e) => {
                    return Err(e.context(format!(
                        "{}: source `{}` ({rel})",
                        crate::SQLITE_EXTRACTION_FAILED,
                        src.id
                    )))
                }
            }
        } else {
            // R2: a non-UTF-8 match (e.g. a binary caught by `knowledge/**`) is
            // preserved raw but not parsed — a whole export must not fail on one file.
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    records.extend(parse_source_file(src, &rel, text, file_mtime(&abs), agent_id))
                }
                Err(_) => warnings.push(format!(
                    "source file {rel} is not valid UTF-8; preserved raw but not parsed into records"
                )),
            }
        }
        // First capture wins (`or_insert`): a sidecar captured consecutively
        // with its `.db` (the WP-G.3 trio) must not be replaced by this later
        // re-read when a glob also matches the sidecar itself.
        raw.entry(rel).or_insert(RawSource::Bytes(bytes));
    }
    records.sort_by_key(|r| r.temporal.created_at);

    // A source that matched nothing is usually a typo or an unsupported glob
    // metachar (`?`, `[a-c]`, `{a,b}` are literals here) — surface it.
    for (i, s) in map.memory_sources.iter().enumerate() {
        if !source_hit[i] {
            warnings.push(format!(
                "memory source `{}` (glob `{}`) matched no files",
                s.id, s.glob
            ));
        }
    }

    // Map file (control file; travels raw so a restore carries the rules).
    if workspace.join(MAP_FILE).is_file() {
        raw.entry(MAP_FILE.to_string())
            .or_insert_with(|| RawSource::Path(workspace.join(MAP_FILE)));
    }
    // Identity file (S1-safe path).
    if let Some(rel) = &map.identity_file {
        if let Some(path) = safe_map_file(workspace, rel)? {
            raw.entry(rel.clone()).or_insert(RawSource::Path(path));
        }
    }

    // Include-list tracked files (in-workspace).
    let mut missing_includes = Vec::new();
    let include = match IncludeList::load(workspace) {
        Ok(list) => list,
        Err(err) => {
            warnings.push(format!(
                "{INCLUDE_FILE} could not be read ({err}); tracked files not synced this run"
            ));
            IncludeList::default()
        }
    };
    for rel in include.paths() {
        if raw.contains_key(&rel) {
            continue;
        }
        let abs = workspace.join(&rel);
        if !abs.is_file() {
            missing_includes.push(rel);
            continue;
        }
        // Re-validate a (possibly restored/edited) stored entry resolves inside
        // the workspace before packing it (A4.2).
        if let Err(e) = alf_core::include::safe_include_path(workspace, &rel) {
            warnings.push(format!("ignoring tracked path {rel}: {e}"));
            continue;
        }
        if is_alfignored(&matcher, &rel) {
            excluded += 1;
            continue;
        }
        raw.insert(rel, RawSource::Path(abs));
    }

    // External (D3) tracked files → `raw/generic/external/<sanitized>`. The
    // TOCTOU guard (denylist + allowed-root + symlink-resolve) is re-run at
    // export; inert/failing entries surface as warnings, not silent drops.
    let roots = alf_core::include::load_allowed_roots();
    let (externals, ext_skipped) = alf_core::include::external_entries_for_export(&include, &roots);
    for (archive_rel, source_canon) in externals {
        raw.entry(archive_rel)
            .or_insert(RawSource::Path(source_canon));
    }
    warnings.extend(ext_skipped);

    // Include list + sync log themselves travel raw.
    for sentinel in [INCLUDE_FILE, SYNC_LOG_FILE] {
        if raw.contains_key(sentinel) {
            continue;
        }
        let abs = workspace.join(sentinel);
        if abs.is_file() {
            raw.insert(sentinel.to_string(), RawSource::Path(abs));
        }
    }

    Ok(Collected {
        records,
        raw,
        excluded_by_alfignore: excluded,
        missing_includes,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Export entry point
// ---------------------------------------------------------------------------

/// Export a generic workspace to an `.alf` archive.
pub fn export(workspace: &Path, output: &Path) -> Result<ExportReport> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }
    let map_path = workspace.join(MAP_FILE);
    if !map_path.is_file() {
        bail!(
            "generic runtime requires a {MAP_FILE} in the workspace ({})",
            workspace.display()
        );
    }
    let map = MemoryMap::load(&map_path)?;
    let mut warnings = map.validate()?; // hard violations abort the export

    let agent_id = resolve_agent_id(workspace)?;
    let runtime_version = map.runtime_version();
    let identity = build_identity(workspace, &map, agent_id)?;
    let agent_name = agent_display_name(identity.as_ref(), &map, workspace);

    let collected = collect(workspace, &map, agent_id)?;
    warnings.extend(collected.warnings);
    let total_records = collected.records.len() as u64;

    // Partitions (time-based, via the shared assigner).
    let mut groups: BTreeMap<String, Vec<MemoryRecord>> = BTreeMap::new();
    for record in collected.records {
        groups
            .entry(PartitionAssigner::partition_for_record(&record))
            .or_default()
            .push(record);
    }
    let mut partition_infos: Vec<(MemoryPartitionInfo, Vec<MemoryRecord>)> = Vec::new();
    for (file, group) in &groups {
        let (from, to) = PartitionAssigner::date_range_for_partition(file)
            .map(|(f, t)| (f, Some(t)))
            .unwrap_or((NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), None));
        partition_infos.push((
            MemoryPartitionInfo {
                file: file.clone(),
                from,
                to,
                record_count: group.len() as u64,
                sealed: false,
                extra: HashMap::new(),
            },
            group.clone(),
        ));
    }

    // Credentials (Layer 4 = agent vault, verbatim).
    let vault_path = home_dir().map(|h| agent_vault_path(&h, agent_id));
    let credentials = load_agent_vault(vault_path.as_deref());

    let identity_version = identity.as_ref().map(|i| i.version);
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
            extra: HashMap::new(),
        },
        layers: LayerInventory {
            identity: identity.as_ref().map(|i| IdentityLayerInfo {
                version: i.version,
                file: "identity/identity.json".to_string(),
                extra: HashMap::new(),
            }),
            principals: None,
            credentials: (credentials_count > 0).then(|| CredentialsLayerInfo {
                count: credentials_count,
                file: "credentials/credentials.json".to_string(),
                extra: HashMap::new(),
            }),
            memory: Some(MemoryInventory {
                record_count: total_records,
                index_file: "memory/index.json".to_string(),
                partitions: partition_infos.iter().map(|(i, _)| i.clone()).collect(),
                has_embeddings: Some(false),
                has_raw_source: Some(true),
                extra: HashMap::new(),
            }),
            attachments: None,
            extra: HashMap::new(),
        },
        runtime_hints: None,
        sync: None,
        raw_sources: vec![RUNTIME.to_string()],
        checksum: None,
        extra: HashMap::new(),
    };

    // Write to a sibling temp file, then atomically rename (R3): a mid-write
    // failure leaves any pre-existing `output` (a prior good backup) untouched.
    let tmp = temp_output_path(output);
    let raw_source_names = match write_archive(
        &tmp,
        manifest,
        identity.as_ref(),
        credentials.as_ref(),
        &partition_infos,
        &collected.raw,
    ) {
        Ok(names) => names,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    fs::rename(&tmp, output).with_context(|| format!("Failed to finalize {}", output.display()))?;

    for rel in &collected.missing_includes {
        warnings.push(format!(
            "tracked file {rel} no longer exists (will be removed from sync on next `alf sync`)"
        ));
    }
    let output_size = fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(ExportReport {
        agent_name,
        alf_version: "1.0.0".to_string(),
        memory_records: total_records,
        identity_version,
        principals_count: 0,
        credentials_count,
        attachments_count: 0,
        raw_sources: raw_source_names,
        output_path: output.to_string_lossy().to_string(),
        output_size_bytes: output_size,
        excluded_by_alfignore: collected.excluded_by_alfignore,
        missing_includes: collected.missing_includes,
        warnings,
    })
}

/// Build the `export --dry-run` preview for a generic workspace: the enumerated
/// raw-tree file list plus the agent name and memory-record count.
///
/// Strictly read-only — writes no archive and, unlike [`export`], never persists
/// `.alf-agent-id` (it resolves the id via [`resolve_agent_id_readonly`]). The
/// file list is the same raw tree [`export`] would pack under `raw/generic/`, so
/// a preview matches the eventual archive.
pub fn enumerate_workspace(workspace: &Path) -> Result<WorkspaceEnumeration> {
    if !workspace.is_dir() {
        bail!(
            "Workspace directory does not exist: {}",
            workspace.display()
        );
    }
    let map_path = workspace.join(MAP_FILE);
    if !map_path.is_file() {
        bail!(
            "generic runtime requires a {MAP_FILE} in the workspace ({})",
            workspace.display()
        );
    }
    let map = MemoryMap::load(&map_path)?;
    let mut warnings = map.validate()?;

    let agent_id = resolve_agent_id_readonly(workspace)?;
    let identity = build_identity(workspace, &map, agent_id)?;
    let agent_name = agent_display_name(identity.as_ref(), &map, workspace);

    let collected = collect(workspace, &map, agent_id)?;
    warnings.extend(collected.warnings);
    let memory_records = collected.records.len() as u64;

    // The raw BTreeMap is already key-sorted, so the file list is deterministic.
    let mut files = Vec::with_capacity(collected.raw.len());
    let mut total_size = 0u64;
    for (rel, source) in &collected.raw {
        let size = match source {
            RawSource::Bytes(bytes) => bytes.len() as u64,
            RawSource::Path(path) => fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        };
        total_size += size;
        files.push(FileEntry {
            path: rel.clone(),
            size,
        });
    }

    for rel in &collected.missing_includes {
        warnings.push(format!(
            "tracked file {rel} no longer exists (will be removed from sync on next `alf sync`)"
        ));
    }

    Ok(WorkspaceEnumeration {
        agent_name,
        memory_records,
        files,
        excluded_by_alfignore: collected.excluded_by_alfignore,
        total_size,
        warnings,
    })
}

/// Write the archive to `path`. Returns the raw-source path list (for the
/// report). Isolated so the caller can clean up the temp file on any failure.
fn write_archive(
    path: &Path,
    manifest: Manifest,
    identity: Option<&Identity>,
    credentials: Option<&CredentialsDocument>,
    partition_infos: &[(MemoryPartitionInfo, Vec<MemoryRecord>)],
    raw: &BTreeMap<String, RawSource>,
) -> Result<Vec<String>> {
    let file = File::create(path)
        .with_context(|| format!("Failed to create output file: {}", path.display()))?;
    let mut writer = AlfWriter::new(BufWriter::new(file), manifest)?;
    if let Some(id) = identity {
        writer.set_identity(id)?;
    }
    if let Some(c) = credentials {
        writer.set_credentials(c)?;
    }
    for (info, group) in partition_infos {
        writer.add_memory_partition(info.clone(), group)?;
    }
    let mut names = Vec::with_capacity(raw.len());
    // WP-I.1: per-entry cap on every lazy read + a running whole-tree total,
    // mirroring the restore side's zip-bomb guard — an archive we would refuse
    // to restore must not be produced in the first place.
    let mut total: u64 = 0;
    for (rel, source) in raw {
        let data: std::borrow::Cow<'_, [u8]> = match source {
            RawSource::Bytes(bytes) => std::borrow::Cow::Borrowed(bytes.as_slice()),
            RawSource::Path(p) => std::borrow::Cow::Owned(read_raw_capped(p, rel)?),
        };
        total = total.saturating_add(data.len() as u64);
        if total > alf_core::MAX_RAW_TOTAL_BYTES {
            bail!(
                "raw tree exceeds the {} byte total cap at {rel} \
                 (a restore would reject the archive)",
                alf_core::MAX_RAW_TOTAL_BYTES
            );
        }
        writer.add_raw_source(RUNTIME, rel, &data)?;
        names.push(rel.clone());
    }
    writer.finish()?;
    Ok(names)
}

/// Sibling temp path for the atomic write, unique per process.
fn temp_output_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("export.alf");
    output.with_file_name(format!(".{name}.tmp.{}", std::process::id()))
}

/// Last-modified time of a file, falling back to the Unix epoch on error (a
/// deterministic anchor, matching `alf_core::ids::newest_mtime`, so a metadata
/// hiccup can't inject `Utc::now()` and manufacture a spurious delta).
fn file_mtime(path: &Path) -> DateTime<Utc> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| DateTime::<Utc>::from(std::time::SystemTime::UNIX_EPOCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_date_parses_iso_stem() {
        assert_eq!(
            filename_date("memories/2026-07-04.md"),
            NaiveDate::from_ymd_opt(2026, 7, 4)
        );
        assert_eq!(filename_date("knowledge/rust.md"), None);
    }

    #[test]
    fn frontmatter_values_reads_key() {
        let md = "---\ndate: 2026-07-04\ntopics: [rust, alf]\n---\n# Title\nbody";
        assert_eq!(frontmatter_values(md, "date"), vec!["2026-07-04"]);
        assert_eq!(frontmatter_values(md, "topics"), vec!["rust", "alf"]);
        assert!(frontmatter_values(md, "missing").is_empty());
        // No frontmatter block → nothing.
        assert!(frontmatter_values("# Just a title\nbody", "date").is_empty());
    }

    #[test]
    fn frontmatter_timestamp_resolves_date() {
        let md = "---\nwhen: 2025-12-31\n---\nnote";
        assert_eq!(
            timestamp_date("frontmatter:when", "x.md", md),
            NaiveDate::from_ymd_opt(2025, 12, 31)
        );
        // file_mtime mode never resolves a date.
        assert_eq!(timestamp_date("file_mtime", "2026-01-01.md", md), None);
    }

    #[test]
    fn tags_seed_namespace_then_hashtags() {
        let src = MemorySourceSpec {
            id: "j".into(),
            glob: "m/*.md".into(),
            memory_type: "episodic".into(),
            namespace: "daily".into(),
            chunking: crate::map::ChunkingMode::ByHeading,
            timestamp: "filename_date".into(),
            tags: vec!["hashtags".into()],
            sqlite: None,
            allow_noncanonical: false,
            extra: HashMap::new(),
        };
        assert_eq!(
            build_tags(&src, "## H\nAgreed. #planning"),
            vec!["daily", "planning"]
        );
        assert_eq!(build_tags(&src, "## H\nno tags here"), vec!["daily"]);
    }

    #[test]
    fn tags_static_and_frontmatter_directives() {
        let src = MemorySourceSpec {
            id: "k".into(),
            glob: "k/*.md".into(),
            memory_type: "semantic".into(),
            namespace: "curated".into(),
            chunking: crate::map::ChunkingMode::PerFile,
            timestamp: "file_mtime".into(),
            tags: vec!["static:kb".into(), "frontmatter:topics".into()],
            sqlite: None,
            allow_noncanonical: false,
            extra: HashMap::new(),
        };
        let md = "---\ntopics: [rust, alf]\n---\n# Doc\nbody";
        assert_eq!(build_tags(&src, md), vec!["curated", "kb", "rust", "alf"]);
    }

    #[test]
    fn identity_field_parse() {
        let (name, desc) = parse_identity(
            "# Fallback\n- **Name:** Toybot\n- **Description:** A toy agent.",
            Path::new("/ws/agent"),
        );
        assert_eq!(name, "Toybot");
        assert_eq!(desc.as_deref(), Some("A toy agent."));

        // No name field → first H1.
        let (name, desc) = parse_identity("# HeadingName\n\nbody", Path::new("/ws/agent"));
        assert_eq!(name, "HeadingName");
        assert!(desc.is_none());
    }

    #[test]
    fn frontmatter_yaml_comment_and_quoted_commas() {
        // R5: the hand-rolled splitter mis-parsed both of these; serde_yaml
        // strips the comment and keeps the quoted comma inside one item.
        let md = "---\ndate: 2026-01-01 # deploy day\ntopics: [\"a, b\", c]\n---\nbody";
        assert_eq!(
            timestamp_date("frontmatter:date", "x.md", md),
            NaiveDate::from_ymd_opt(2026, 1, 1)
        );
        assert_eq!(frontmatter_values(md, "topics"), vec!["a, b", "c"]);
    }

    #[test]
    fn frontmatter_colon_in_value_preserved() {
        let md = "---\nnote: a:b:c\n---\nbody";
        assert_eq!(frontmatter_values(md, "note"), vec!["a:b:c"]);
    }
}
