//! `.alf-map.json` — the generic runtime's declarative extraction map.
//!
//! The map is the whole point of `adapter-generic`: a runtime that is *not* one
//! of the built-in adapters describes, in one file, how its workspace files
//! become ALF memory records. The schema is normative (design §8). This module
//! owns parsing and validation; extraction lives in [`crate::export`].
//!
//! Validation is the goal-(d) enforcement point (design F3): it hard-errors on
//! the two things that would silently corrupt dashboard parity or id stability —
//! a non-canonical `memory_type` without the explicit escape hatch, and a
//! mid-segment `**` glob (which gitignore treats as a plain `*`, so a map author
//! writing gitignore-style globs would silently over-match, and glob semantics
//! are an id-stability contract that can never be corrected once shipped).
//! Everything else that merely *degrades* the dashboard (non-canonical
//! namespaces, out-of-range watch intervals) is a warning, not a failure.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Workspace-relative name of the map file.
pub const MAP_FILE: &str = ".alf-map.json";

/// Dashboard-canonical memory types. The web filter chips are hardcoded to
/// these three (`MemoryFilters.vue`); anything else is a hard error unless the
/// source opts in via `allow_noncanonical`.
const CANONICAL_TYPES: &[&str] = &["episodic", "semantic", "procedural"];

/// Namespaces the dashboard filters/groups on. Outside this set records still
/// export fine but lose chip filtering + grouping — a warning, never an error.
const CANONICAL_NAMESPACES: &[&str] = &["daily", "curated", "procedural"];

/// Delta interval floor (design R3 / §11.3): a memory/raw change produces a
/// cheap delta, so 1 minute is the tightest cadence allowed.
const DELTA_FLOOR: Duration = Duration::from_secs(60);
/// Tracked-file interval floor (design §11.3): a tracked-file change triggers a
/// full-snapshot rollover, so its cadence floor is 15 minutes to avoid a full
/// archive upload per minute on a churning tracked file.
const TRACKED_FLOOR: Duration = Duration::from_secs(15 * 60);
/// Interval ceiling for both knobs (24 h).
const INTERVAL_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// A parsed `.alf-map.json`. Unknown fields are preserved on every level via
/// `#[serde(flatten)]` so a newer map written by a future tool round-trips
/// through an older `alf` without loss (design §8: "unknown fields preserved").
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryMap {
    pub version: u32,
    /// Informational only (design review resolution 3): prefixes
    /// `source_runtime_version` and can seed the display name. Never affects
    /// dispatch, paths, or ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework_version: Option<String>,
    /// Optional workspace-relative identity document → Layer 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub memory_sources: Vec<MemorySourceSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchConfig>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// One extraction rule: a glob and how the matched files map to records.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemorySourceSpec {
    pub id: String,
    /// Workspace-relative glob (see `alf_core::chunk::path_matches`).
    pub glob: String,
    pub memory_type: String,
    pub namespace: String,
    pub chunking: ChunkingMode,
    #[serde(default = "default_timestamp")]
    pub timestamp: String,
    /// Tag-extraction directives: `hashtags`, `static:<tag>`, `frontmatter:<key>`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Escape hatch: downgrade a non-canonical `memory_type` from error to
    /// warning (records will not match the dashboard filter chips).
    #[serde(default)]
    pub allow_noncanonical: bool,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// How a matched file is chunked into records. Maps 1:1 onto
/// [`alf_core::chunk::ChunkingStrategy`]. An unknown value fails at parse time
/// with a clear serde error — a malformed known field, not an unknown field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkingMode {
    PerFile,
    ByHeading,
}

impl ChunkingMode {
    /// The `alf_core::chunk` strategy. `by_heading` is fixed to fence-aware ATX
    /// level-2 splitting — byte-compatible with OpenClaw's splitter (design §9).
    pub fn strategy(self) -> alf_core::chunk::ChunkingStrategy {
        use alf_core::chunk::ChunkingStrategy;
        match self {
            ChunkingMode::PerFile => ChunkingStrategy::OneRecordPerFile,
            ChunkingMode::ByHeading => ChunkingStrategy::SplitByHeading {
                fence_aware: true,
                level: 2,
            },
        }
    }
}

fn default_timestamp() -> String {
    "file_mtime".to_string()
}

/// Watch cadence knobs. Consumed by the watch loop (WP-M3); validated + clamped
/// here so an invalid cadence is caught the moment the map is loaded.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WatchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_interval: Option<String>,
    #[serde(default)]
    pub per_source: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_files_interval: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Load + validate
// ---------------------------------------------------------------------------

impl MemoryMap {
    /// Parse from JSON text (no semantic validation).
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("parsing .alf-map.json")
    }

    /// Load and parse `{workspace}/.alf-map.json`.
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&content)
    }

    /// Full semantic validation. Returns the list of non-fatal warnings; a hard
    /// violation returns `Err`. Idempotent and side-effect-free.
    ///
    /// Hard errors (a broken map must not export): unrecognized `version`,
    /// non-canonical `memory_type` without `allow_noncanonical`, mid-segment
    /// `**` globs, an absolute / `..`-escaping `identity_file`, unknown
    /// timestamp/tag directives. Everything advisory — non-canonical namespaces,
    /// clamped/invalid *watch* intervals (nothing consumes them until WP-M3) —
    /// is a warning so one stray watch value can never brick a memory export.
    pub fn validate(&self) -> Result<Vec<String>> {
        let mut warnings = Vec::new();

        // `version` is a one-way-door contract: a v2 map may redefine glob /
        // chunking / id semantics, so parsing it under v1 rules would mint birth
        // ids under the wrong scheme (irreversible once synced). Fail closed.
        if self.version != 1 {
            bail!(
                "unsupported .alf-map.json version {} (this build understands version 1). \
                 A newer map may use different extraction/id semantics; refusing to \
                 parse it under v1 rules.",
                self.version
            );
        }

        // S1: an identity_file that escapes the workspace would exfiltrate a
        // host file into Layer 1 + raw on sync. Reject at config time.
        if let Some(rel) = &self.identity_file {
            reject_unsafe_relpath(rel).context("identity_file")?;
        }

        let mut ids: HashSet<&str> = HashSet::new();
        for src in &self.memory_sources {
            let ctx = || format!("memory source `{}`", src.id);

            if !ids.insert(src.id.as_str()) {
                warnings.push(format!("duplicate memory source id `{}`", src.id));
            }

            validate_glob(&src.glob).with_context(ctx)?;
            validate_timestamp(&src.timestamp).with_context(ctx)?;
            for tag in &src.tags {
                validate_tag_directive(tag).with_context(ctx)?;
            }

            if !CANONICAL_TYPES.contains(&src.memory_type.as_str()) {
                if src.allow_noncanonical {
                    warnings.push(format!(
                        "source `{}`: non-canonical memory_type `{}` — dashboard \
                         filter chips only recognize episodic/semantic/procedural",
                        src.id, src.memory_type
                    ));
                } else {
                    bail!(
                        "source `{}`: memory_type `{}` is not one of \
                         episodic/semantic/procedural. Add \"allow_noncanonical\": \
                         true to this source to override (its records will not match \
                         the dashboard filter chips).",
                        src.id,
                        src.memory_type
                    );
                }
            }

            if !CANONICAL_NAMESPACES.contains(&src.namespace.as_str()) {
                warnings.push(format!(
                    "source `{}`: non-canonical namespace `{}` loses dashboard chip \
                     filtering and grouping",
                    src.id, src.namespace
                ));
            }
        }

        if let Some(watch) = &self.watch {
            // Deterministic warning order: `per_source` is a HashMap.
            let mut keys: Vec<&String> = watch.per_source.keys().collect();
            keys.sort();
            for key in &keys {
                if !self.memory_sources.iter().any(|s| &s.id == *key) {
                    warnings.push(format!(
                        "watch.per_source references unknown source id `{key}`"
                    ));
                }
            }
            if let Some(raw) = &watch.default_interval {
                push_interval_warning(&mut warnings, "watch.default_interval", raw, DELTA_FLOOR);
            }
            for key in &keys {
                push_interval_warning(
                    &mut warnings,
                    &format!("watch.per_source[{key}]"),
                    &watch.per_source[*key],
                    DELTA_FLOOR,
                );
            }
            if let Some(raw) = &watch.tracked_files_interval {
                push_interval_warning(
                    &mut warnings,
                    "watch.tracked_files_interval",
                    raw,
                    TRACKED_FLOOR,
                );
            }
        }

        Ok(warnings)
    }

    /// `source_runtime_version` = `{framework}/{framework_version}` (design §8),
    /// omitting whichever side is absent.
    pub fn runtime_version(&self) -> Option<String> {
        match (&self.framework, &self.framework_version) {
            (Some(f), Some(v)) => Some(format!("{f}/{v}")),
            (Some(s), None) | (None, Some(s)) => Some(s.clone()),
            (None, None) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Validators
// ---------------------------------------------------------------------------

/// Reject a mid-segment `**` in a glob. Only whole-component `**` is legal:
/// leading `**/`, `/**/` between segments, or trailing `/**` (and the bare `**`
/// whole pattern). `notes**`, `a**b`, `dir/**.md`, and `***` are rejected —
/// gitignore treats those "other consecutive asterisks" as a plain `*`, so a map
/// author would silently over-match, and `alf_core::chunk::glob_match` crosses
/// `/` on a bare `**` wherever it appears.
fn validate_glob(glob: &str) -> Result<()> {
    if glob.is_empty() {
        bail!("glob is empty");
    }
    let bytes = glob.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'*' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'*' {
            i += 1;
        }
        let run = i - start;
        if run >= 2 {
            let before_ok = start == 0 || bytes[start - 1] == b'/';
            let after_ok = i == bytes.len() || bytes[i] == b'/';
            if run != 2 || !before_ok || !after_ok {
                bail!(
                    "invalid glob `{glob}`: `**` must be a whole path component — \
                     only leading `**/`, `/**/` between segments, or trailing `/**` \
                     are allowed. A mid-segment `**` (e.g. `notes**`, `a**b`, \
                     `dir/**.md`) is rejected because gitignore treats it as a plain \
                     `*`, so the map would silently over-match; glob semantics are an \
                     id-stability contract and cannot be corrected once shipped."
                );
            }
        }
    }
    Ok(())
}

/// Recognize a timestamp mode: `filename_date`, `file_mtime`, or
/// `frontmatter:<key>`.
fn validate_timestamp(mode: &str) -> Result<()> {
    if mode == "filename_date" || mode == "file_mtime" {
        return Ok(());
    }
    if let Some(key) = mode.strip_prefix("frontmatter:") {
        if key.is_empty() {
            bail!("timestamp `frontmatter:` needs a key, e.g. `frontmatter:date`");
        }
        return Ok(());
    }
    bail!(
        "unknown timestamp mode `{mode}` (expected filename_date, file_mtime, or \
         frontmatter:<key>)"
    )
}

/// Recognize a tag directive: `hashtags`, `static:<tag>`, or `frontmatter:<key>`.
fn validate_tag_directive(tag: &str) -> Result<()> {
    if tag == "hashtags" {
        return Ok(());
    }
    for (prefix, what) in [("static:", "value"), ("frontmatter:", "key")] {
        if let Some(rest) = tag.strip_prefix(prefix) {
            if rest.is_empty() {
                bail!("tag `{prefix}` needs a {what}");
            }
            return Ok(());
        }
    }
    bail!(
        "unknown tag directive `{tag}` (expected hashtags, static:<tag>, or \
         frontmatter:<key>)"
    )
}

/// Reject an absolute or `..`-escaping workspace-relative path before it reaches
/// the filesystem (S1). Pure-string / component check, so it fires even when the
/// referenced file does not exist. Export re-checks with a canonicalizing guard.
pub(crate) fn reject_unsafe_relpath(rel: &str) -> Result<()> {
    use std::path::{Component, Path};
    let p = Path::new(rel);
    if p.is_absolute() || rel.starts_with('/') || rel.starts_with('\\') {
        bail!("path `{rel}` must be workspace-relative, not absolute");
    }
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                bail!("path `{rel}` must not contain `..` (it would escape the workspace)")
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("path `{rel}` must be a workspace-relative path")
            }
            _ => {}
        }
    }
    Ok(())
}

/// Parse a human duration — one or more `<n><unit>` segments (`15m`, `1h30m`,
/// `90s`). Units are `s`/`m`/`h`/`d`. All arithmetic is checked, so a giant
/// value errors instead of panicking (debug) or wrapping (release).
fn parse_duration(raw: &str) -> Result<Duration> {
    let s = raw.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total: u64 = 0;
    let mut segments = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            bail!("interval `{raw}`: expected a number, e.g. `15m` or `1h30m`");
        }
        let value: u64 = s[start..i].parse().map_err(|_| {
            anyhow::anyhow!("interval `{raw}`: number `{}` is too large", &s[start..i])
        })?;
        let Some(&unit) = bytes.get(i) else {
            bail!(
                "interval `{raw}`: number `{}` has no unit (s/m/h/d)",
                &s[start..i]
            );
        };
        i += 1;
        let factor: u64 = match unit {
            b's' => 1,
            b'm' => 60,
            b'h' => 3600,
            b'd' => 86_400,
            other => bail!(
                "interval `{raw}` has unknown unit `{}` (expected s/m/h/d)",
                other as char
            ),
        };
        let secs = value
            .checked_mul(factor)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(|| anyhow::anyhow!("interval `{raw}` is too large"))?;
        total = secs;
        segments += 1;
    }
    if segments == 0 {
        bail!("interval `{raw}` is empty");
    }
    Ok(Duration::from_secs(total))
}

/// Parse `raw` and clamp it into `[floor, INTERVAL_CEILING]`. Returns the
/// clamped value plus a note when clamping actually changed it. Callers that
/// only need the clamped value (WP-M3) discard the note.
pub(crate) fn parse_and_clamp(raw: &str, floor: Duration) -> Result<(Duration, Option<String>)> {
    let parsed = parse_duration(raw)?;
    let clamped = parsed.clamp(floor, INTERVAL_CEILING);
    let note = (clamped != parsed).then(|| {
        format!(
            "interval `{raw}` clamped to {}s (allowed {}s–{}s)",
            clamped.as_secs(),
            floor.as_secs(),
            INTERVAL_CEILING.as_secs()
        )
    });
    Ok((clamped, note))
}

/// Watch intervals are advisory (nothing consumes them until WP-M3), so a bad
/// value is a warning — never a hard error that would abort the memory export.
fn push_interval_warning(warnings: &mut Vec<String>, label: &str, raw: &str, floor: Duration) {
    match parse_and_clamp(raw, floor) {
        Ok((_, Some(note))) => warnings.push(format!("{label}: {note}")),
        Ok((_, None)) => {}
        Err(e) => warnings.push(format!(
            "{label}: {e:#}; interval ignored (watch is advisory)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, glob: &str, ty: &str, ns: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "glob": glob, "memory_type": ty,
            "namespace": ns, "chunking": "per_file"
        })
    }

    fn map_with(sources: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "version": 1, "memory_sources": sources })
    }

    #[test]
    fn valid_map_parses_and_validates_clean() {
        let json = map_with(vec![source(
            "journal",
            "memories/*.md",
            "episodic",
            "daily",
        )]);
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        assert!(map.validate().unwrap().is_empty());
    }

    #[test]
    fn noncanonical_type_is_error_without_hatch() {
        let json = map_with(vec![source("s", "a/*.md", "summary", "curated")]);
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        let err = map.validate().unwrap_err();
        assert!(format!("{err:#}").contains("allow_noncanonical"));
    }

    #[test]
    fn noncanonical_type_is_warning_with_hatch() {
        let mut src = source("s", "a/*.md", "summary", "curated");
        src["allow_noncanonical"] = serde_json::json!(true);
        let map = MemoryMap::parse(&map_with(vec![src]).to_string()).unwrap();
        let warnings = map.validate().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.contains("non-canonical memory_type")));
    }

    #[test]
    fn noncanonical_namespace_is_warning_only() {
        let json = map_with(vec![source("s", "a/*.md", "semantic", "scratch")]);
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        let warnings = map.validate().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.contains("non-canonical namespace")));
    }

    #[test]
    fn mid_segment_double_star_is_rejected() {
        for bad in ["notes**", "a**b", "memory/**.md", "dir/a**/*.md", "x/***/y"] {
            let json = map_with(vec![source("s", bad, "semantic", "curated")]);
            let map = MemoryMap::parse(&json.to_string()).unwrap();
            let err = map
                .validate()
                .expect_err(&format!("expected `{bad}` to be rejected"));
            assert!(
                format!("{err:#}").contains("whole path component"),
                "glob `{bad}` should be rejected as mid-segment `**`"
            );
        }
    }

    #[test]
    fn component_double_star_is_accepted() {
        for good in [
            "**/*.md",
            "knowledge/**/*.md",
            "knowledge/**",
            "**",
            "a/**/b/*.md",
        ] {
            let json = map_with(vec![source("s", good, "semantic", "curated")]);
            let map = MemoryMap::parse(&json.to_string()).unwrap();
            assert!(
                map.validate().is_ok(),
                "glob `{good}` should be accepted as component `**`"
            );
        }
    }

    #[test]
    fn interval_ceiling_clamps_to_24h() {
        let (clamped, note) = parse_and_clamp("48h", DELTA_FLOOR).unwrap();
        assert_eq!(clamped, INTERVAL_CEILING);
        assert!(note.unwrap().contains("clamped"));
    }

    #[test]
    fn delta_floor_clamps_to_one_minute() {
        let (clamped, note) = parse_and_clamp("5s", DELTA_FLOOR).unwrap();
        assert_eq!(clamped, DELTA_FLOOR);
        assert!(note.is_some());
    }

    #[test]
    fn tracked_floor_is_fifteen_minutes() {
        let (clamped, note) = parse_and_clamp("1m", TRACKED_FLOOR).unwrap();
        assert_eq!(clamped, TRACKED_FLOOR);
        assert!(note.is_some());
        // A value already above the floor is not clamped.
        let (ok, none) = parse_and_clamp("1h", TRACKED_FLOOR).unwrap();
        assert_eq!(ok, Duration::from_secs(3600));
        assert!(none.is_none());
    }

    #[test]
    fn watch_clamps_surface_as_warnings() {
        let json = serde_json::json!({
            "version": 1,
            "memory_sources": [source("journal", "memories/*.md", "episodic", "daily")],
            "watch": { "default_interval": "10s", "tracked_files_interval": "48h" }
        });
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        let warnings = map.validate().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.contains("watch.default_interval")));
        assert!(warnings
            .iter()
            .any(|w| w.contains("watch.tracked_files_interval")));
    }

    #[test]
    fn bad_duration_is_hard_error() {
        assert!(parse_duration("15x").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10").is_err());
    }

    #[test]
    fn compound_duration_parses() {
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172_800));
    }

    #[test]
    fn giant_duration_errors_without_panic() {
        // Would overflow u64 seconds; must error cleanly, not panic/wrap (R4).
        assert!(parse_duration("999999999999999999999d").is_err());
        assert!(parse_duration("18446744073709551615d").is_err());
    }

    #[test]
    fn bad_watch_interval_is_warning_not_export_abort() {
        // A malformed/compound-broken watch value must NOT fail validation —
        // watch is advisory until WP-M3; a stray value can't brick the export.
        let json = serde_json::json!({
            "version": 1,
            "memory_sources": [source("journal", "memories/*.md", "episodic", "daily")],
            "watch": { "default_interval": "1h30x" }
        });
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        let warnings = map.validate().expect("bad watch interval must not error");
        assert!(warnings
            .iter()
            .any(|w| w.contains("watch.default_interval") && w.contains("advisory")));
    }

    #[test]
    fn unsupported_version_is_hard_error() {
        for v in [0u32, 2, 99] {
            let json = serde_json::json!({ "version": v, "memory_sources": [] });
            let map = MemoryMap::parse(&json.to_string()).unwrap();
            let err = map.validate().expect_err("non-1 version must error");
            assert!(format!("{err:#}").contains("version"));
        }
    }

    #[test]
    fn unsafe_identity_file_is_rejected() {
        for bad in [
            "/etc/hostname",
            "../secrets.md",
            "a/../../x.md",
            "\\\\host\\share",
        ] {
            let json = serde_json::json!({
                "version": 1, "identity_file": bad, "memory_sources": []
            });
            let map = MemoryMap::parse(&json.to_string()).unwrap();
            assert!(
                map.validate().is_err(),
                "identity_file `{bad}` should be rejected"
            );
        }
        // A normal in-workspace identity file validates clean.
        let json = serde_json::json!({
            "version": 1, "identity_file": "IDENTITY.md", "memory_sources": []
        });
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        assert!(map.validate().unwrap().is_empty());
    }

    #[test]
    fn per_source_warnings_are_deterministically_ordered() {
        // HashMap iteration order must not leak into warnings — the unknown-id
        // warnings for `aaa`/`mmm`/`zzz` must appear in sorted order.
        let json = serde_json::json!({
            "version": 1,
            "memory_sources": [source("journal", "memories/*.md", "episodic", "daily")],
            "watch": { "per_source": { "zzz": "1s", "aaa": "1s", "mmm": "1s" } }
        });
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        let warnings = map.validate().unwrap();
        let order: Vec<usize> = ["aaa", "mmm", "zzz"]
            .iter()
            .map(|k| {
                warnings
                    .iter()
                    .position(|w| w.contains("unknown source id") && w.contains(k))
                    .unwrap_or_else(|| panic!("no unknown-id warning for `{k}`"))
            })
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "warnings not sorted");
    }

    #[test]
    fn unknown_timestamp_mode_rejected() {
        let mut src = source("s", "a/*.md", "semantic", "curated");
        src["timestamp"] = serde_json::json!("epoch");
        let map = MemoryMap::parse(&map_with(vec![src]).to_string()).unwrap();
        assert!(map.validate().is_err());
    }

    #[test]
    fn frontmatter_timestamp_and_tag_directives_recognized() {
        let mut src = source("s", "a/*.md", "semantic", "curated");
        src["timestamp"] = serde_json::json!("frontmatter:date");
        src["tags"] = serde_json::json!(["hashtags", "static:kb", "frontmatter:topics"]);
        let map = MemoryMap::parse(&map_with(vec![src]).to_string()).unwrap();
        assert!(map.validate().unwrap().is_empty());
    }

    #[test]
    fn unknown_fields_preserved() {
        let json = serde_json::json!({
            "version": 1,
            "future_top_level": "kept",
            "memory_sources": [{
                "id": "s", "glob": "a/*.md", "memory_type": "semantic",
                "namespace": "curated", "chunking": "per_file",
                "future_source_field": 42
            }]
        });
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        assert_eq!(map.extra.get("future_top_level").unwrap(), "kept");
        assert_eq!(
            map.memory_sources[0]
                .extra
                .get("future_source_field")
                .unwrap(),
            42
        );
        // And they survive a round-trip through serialization.
        let round = serde_json::to_string(&map).unwrap();
        assert!(round.contains("future_top_level"));
        assert!(round.contains("future_source_field"));
    }

    #[test]
    fn runtime_version_formats_framework_slash_version() {
        let json = serde_json::json!({
            "version": 1, "framework": "acme", "framework_version": "2.3.1",
            "memory_sources": []
        });
        let map = MemoryMap::parse(&json.to_string()).unwrap();
        assert_eq!(map.runtime_version().as_deref(), Some("acme/2.3.1"));
    }
}
