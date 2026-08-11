//! `alf_configure` — validated read-modify-write of the generic runtime's
//! `.alf-map.json` (design §6, §8). No CLI equivalent: the built-in adapters own
//! their own extraction, so this tool is generic-only and errors on any other
//! runtime.
//!
//! Two modes: `map` replaces the file wholesale; `patch` deep-merges into the
//! existing map — objects merge recursively; `memory_sources` merges KEYED BY
//! entry `id` (matching id → deep-merge that source, new id → append, missing
//! id → error) so "add one source" can never clobber the others (manual §3.7);
//! all other scalars/arrays replace. Either way the result is parsed and
//! validated **before** anything is written, so an invalid configuration is
//! rejected with no partial application — the file on disk is never left
//! half-written.

use std::path::Path;

use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use adapter_generic::{MemoryMap, MAP_FILE};

use crate::config::Config;

/// The `alf_configure` tool result: the effective (validated) map plus any
/// non-fatal validation warnings (e.g. a non-canonical namespace).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ConfigureResult {
    ok: bool,
    map_path: String,
    /// The effective `.alf-map.json` after applying `map`/`patch`, exactly as
    /// written to disk.
    map: Value,
    // Skipped when empty on a non-Option ⇒ `#[serde(default)]` (M2a §2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    /// Operational note about when the change takes effect.
    note: String,
}

/// Apply a `map` (full replacement) or `patch` (deep merge) to the workspace's
/// `.alf-map.json`, validating before writing. Generic-only.
pub(crate) fn configure(
    runtime: &str,
    workspace_flag: Option<&Path>,
    map: Option<Value>,
    patch: Option<Value>,
) -> Result<ConfigureResult> {
    if runtime != "generic" {
        bail!(
            "alf_configure is only available for the generic runtime (got '{runtime}'). \
             The openclaw/zeroclaw/hermes adapters own their own extraction — there is no \
             map file to configure."
        );
    }

    let config = Config::load()?;
    let workspace =
        crate::commands::check::resolve_workspace_required(workspace_flag, &config, runtime)?;
    let map_path = workspace.join(MAP_FILE);

    // Build the effective map from exactly one of map/patch.
    let effective: Value = match (map, patch) {
        (Some(_), Some(_)) => {
            bail!("pass exactly one of `map` (full replacement) or `patch` (deep merge), not both")
        }
        (None, None) => {
            bail!("pass one of `map` (full replacement) or `patch` (deep merge)")
        }
        (Some(m), None) => m,
        (None, Some(p)) => {
            let mut base = if map_path.is_file() {
                let raw = std::fs::read_to_string(&map_path)
                    .with_context(|| format!("reading {}", map_path.display()))?;
                serde_json::from_str(&raw)
                    .with_context(|| format!("{} is not valid JSON", map_path.display()))?
            } else {
                Value::Object(serde_json::Map::new())
            };
            merge_top(&mut base, p)?;
            base
        }
    };

    // Parse + validate BEFORE writing — a hard violation aborts with the file
    // untouched (no partial application). Re-serialize the parsed map so the file
    // on disk is exactly the shape the validator (and every later export) sees.
    let parsed: MemoryMap = serde_json::from_value(effective).context(
        "the resulting configuration is not a valid .alf-map.json (see .alf-map.json schema)",
    )?;
    let warnings = parsed.validate()?;

    let serialized = format!(
        "{}\n",
        serde_json::to_string_pretty(&parsed).context("serializing the map")?
    );

    // A generic workspace may not exist yet on first contact — create it so the
    // write lands, then atomic temp+rename so a crash can't leave a torn map.
    if let Some(parent) = map_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating workspace {}", parent.display()))?;
    }
    atomic_write(&map_path, &serialized)?;

    let map_value = serde_json::to_value(&parsed).context("re-encoding the written map")?;
    Ok(ConfigureResult {
        ok: true,
        map_path: map_path.to_string_lossy().into_owned(),
        map: map_value,
        warnings,
        note: "The watch surface refreshes automatically: newly-added memory \
               locations are watched within one tick (manual §4.3)."
            .into(),
    })
}

/// Top-level merge: like [`merge`], but the `memory_sources` key routes through
/// the keyed array merge (manual §3.7) and can therefore error (a patch entry
/// without an `id`).
fn merge_top(base: &mut Value, patch: Value) -> Result<()> {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                let is_sources = k == "memory_sources";
                let slot = b.entry(k).or_insert(Value::Null);
                if is_sources {
                    merge_memory_sources(slot, v)?;
                } else {
                    merge(slot, v);
                }
            }
            Ok(())
        }
        (b, p) => {
            *b = p;
            Ok(())
        }
    }
}

/// Keyed array merge for `memory_sources`: patch entries pair with existing
/// sources by their `id` (matching id → deep-merge that entry; new id →
/// append; entry without an id → error, nothing written). Removal stays a
/// `replace` operation. The natural weak-model call — "add this one source" —
/// therefore never silently drops the others (the old wholesale-replace
/// behavior deleted their records on the next sync).
fn merge_memory_sources(base: &mut Value, patch: Value) -> Result<()> {
    match (base, patch) {
        (Value::Array(b), Value::Array(p)) => {
            for (i, entry) in p.into_iter().enumerate() {
                let Some(id) = entry.get("id").and_then(Value::as_str).map(str::to_owned) else {
                    bail!(
                        "patch memory_sources[{i}] has no \"id\" — patch entries merge by \
                         id (include the id of the source to add or modify, or use \
                         operation \"replace\" to rewrite the whole list)"
                    );
                };
                match b
                    .iter_mut()
                    .find(|e| e.get("id").and_then(Value::as_str) == Some(id.as_str()))
                {
                    Some(existing) => merge(existing, entry),
                    None => b.push(entry),
                }
            }
            Ok(())
        }
        // Not two arrays (first write, or a deliberate non-list) — replace;
        // validation rejects a malformed result before anything is written.
        (b, p) => {
            *b = p;
            Ok(())
        }
    }
}

/// Deep-merge `patch` into `base`: two objects merge key-by-key (recursively);
/// anything else (scalars, arrays) is replaced by the patch value.
fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                merge(b.entry(k).or_insert(Value::Null), v);
            }
        }
        (b, p) => *b = p,
    }
}

/// Write `contents` to `path` atomically (sibling temp + rename), so a crash
/// mid-write leaves any pre-existing map untouched.
///
/// The temp name carries a **process-unique** suffix (pid + a monotonic counter)
/// so two concurrent writers — rmcp fans a request batch out across threads
/// (WP-M3 review E1) — never collide on the same temp path and stream into each
/// other's file. Combined with the atomic rename, concurrent writes converge on a
/// whole (last-writer-wins) map, never a torn one.
fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    // alf-core's writer: pid+counter temp naming, write, fsync, rename — the
    // fsync is what the old local implementation lacked (a power loss could
    // rename an unflushed temp over a good map).
    alf_core::write_atomic(path, contents.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn valid_map() -> Value {
        serde_json::json!({
            "version": 1,
            "memory_sources": [
                { "id": "journal", "glob": "memories/*.md", "memory_type": "episodic",
                  "namespace": "daily", "chunking": "by_heading", "timestamp": "filename_date" }
            ]
        })
    }

    #[test]
    fn non_generic_runtime_errors() {
        let err = configure("openclaw", None, Some(valid_map()), None).unwrap_err();
        assert!(format!("{err:#}").contains("only available for the generic runtime"));
    }

    #[test]
    fn map_writes_and_validates() {
        let ws = TempDir::new().unwrap();
        let result = configure("generic", Some(ws.path()), Some(valid_map()), None).unwrap();
        assert!(result.ok);
        // File exists and reloads through the real map parser.
        let path = ws.path().join(MAP_FILE);
        assert!(path.is_file());
        let reloaded = MemoryMap::load(&path).unwrap();
        assert_eq!(reloaded.version, 1);
        assert_eq!(reloaded.memory_sources.len(), 1);
    }

    #[test]
    fn invalid_map_is_rejected_without_writing() {
        let ws = TempDir::new().unwrap();
        // memory_type `summary` is non-canonical without the escape hatch → hard error.
        let bad = serde_json::json!({
            "version": 1,
            "memory_sources": [
                { "id": "s", "glob": "a/*.md", "memory_type": "summary",
                  "namespace": "curated", "chunking": "per_file" }
            ]
        });
        let err = configure("generic", Some(ws.path()), Some(bad), None).unwrap_err();
        assert!(format!("{err:#}").contains("allow_noncanonical"));
        // No partial application: the file was never written.
        assert!(!ws.path().join(MAP_FILE).is_file());
    }

    #[test]
    fn patch_merges_into_existing_map() {
        let ws = TempDir::new().unwrap();
        configure("generic", Some(ws.path()), Some(valid_map()), None).unwrap();

        // Patch only the framework field; memory_sources must survive.
        let patch = serde_json::json!({ "framework": "acme", "framework_version": "1.0" });
        let result = configure("generic", Some(ws.path()), None, Some(patch)).unwrap();
        assert_eq!(result.map["framework"], "acme");
        assert_eq!(result.map["memory_sources"].as_array().unwrap().len(), 1);

        let reloaded = MemoryMap::load(&ws.path().join(MAP_FILE)).unwrap();
        assert_eq!(reloaded.framework.as_deref(), Some("acme"));
        assert_eq!(reloaded.memory_sources.len(), 1);
    }

    #[test]
    fn patch_adds_a_source_preserving_existing_ones() {
        let ws = TempDir::new().unwrap();
        configure("generic", Some(ws.path()), Some(valid_map()), None).unwrap();

        // The natural "add one source" call must NOT clobber `journal`.
        let patch = serde_json::json!({ "memory_sources": [
            { "id": "kb", "glob": "knowledge/**/*.md", "memory_type": "semantic",
              "namespace": "curated", "chunking": "per_file" }
        ]});
        let result = configure("generic", Some(ws.path()), None, Some(patch)).unwrap();
        let sources = result.map["memory_sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2, "append, never replace: {sources:?}");
        let ids: Vec<_> = sources.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"journal") && ids.contains(&"kb"));
    }

    #[test]
    fn patch_modifies_one_source_by_id() {
        let ws = TempDir::new().unwrap();
        configure("generic", Some(ws.path()), Some(valid_map()), None).unwrap();

        let patch = serde_json::json!({ "memory_sources": [
            { "id": "journal", "glob": "diary/*.md" }
        ]});
        let result = configure("generic", Some(ws.path()), None, Some(patch)).unwrap();
        let sources = result.map["memory_sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["glob"], "diary/*.md", "patched field applied");
        assert_eq!(
            sources[0]["memory_type"], "episodic",
            "unpatched fields survive the keyed deep-merge"
        );
    }

    #[test]
    fn patch_source_without_id_errors_without_writing() {
        let ws = TempDir::new().unwrap();
        configure("generic", Some(ws.path()), Some(valid_map()), None).unwrap();
        let before = std::fs::read_to_string(ws.path().join(MAP_FILE)).unwrap();

        let patch = serde_json::json!({ "memory_sources": [ { "glob": "x/*.md" } ]});
        let err = configure("generic", Some(ws.path()), None, Some(patch)).unwrap_err();
        assert!(
            format!("{err:#}").contains("has no \"id\""),
            "error must be actionable: {err:#}"
        );
        let after = std::fs::read_to_string(ws.path().join(MAP_FILE)).unwrap();
        assert_eq!(before, after, "nothing written on a rejected patch");
    }

    #[test]
    fn patch_arrays_inside_a_source_still_replace() {
        let ws = TempDir::new().unwrap();
        let mut map = valid_map();
        map["memory_sources"][0]["tags"] = serde_json::json!(["static:old", "hashtags"]);
        configure("generic", Some(ws.path()), Some(map), None).unwrap();

        let patch = serde_json::json!({ "memory_sources": [
            { "id": "journal", "tags": ["static:new"] }
        ]});
        let result = configure("generic", Some(ws.path()), None, Some(patch)).unwrap();
        assert_eq!(
            result.map["memory_sources"][0]["tags"],
            serde_json::json!(["static:new"]),
            "non-keyed arrays keep replace semantics"
        );
    }

    #[test]
    fn atomic_write_leaves_no_temp_sibling() {
        let ws = TempDir::new().unwrap();
        configure("generic", Some(ws.path()), Some(valid_map()), None).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(ws.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "no temp siblings: {leftovers:?}");
    }

    #[test]
    fn both_map_and_patch_errors() {
        let ws = TempDir::new().unwrap();
        let err = configure(
            "generic",
            Some(ws.path()),
            Some(valid_map()),
            Some(valid_map()),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("exactly one"));
    }

    #[test]
    fn neither_map_nor_patch_errors() {
        let ws = TempDir::new().unwrap();
        let err = configure("generic", Some(ws.path()), None, None).unwrap_err();
        assert!(format!("{err:#}").contains("pass one of"));
    }
}
